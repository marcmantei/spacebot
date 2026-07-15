//! Per-worker workspace isolation via git worktrees.
//!
//! # Why this exists (issue #224)
//!
//! Every builtin worker used to run its shell and file tools in the agent's
//! single shared checkout (`runtime_config.workspace_dir`). When two workers
//! ran concurrently against the same repo, one worker's `git add` swept up a
//! sibling's in-progress edits, branches got contaminated with stale content,
//! and work was committed to the wrong branch and then stranded. See the
//! incidents catalogued in issue #224 (PR #211 stale-file sweep, PR #218/#219
//! byte-identical blob, the 2026-07-15 `feature/1` stranding).
//!
//! # What it does
//!
//! [`WorkerWorkspace::provision`] hands each worker its own isolated directory.
//! For every git repository directly under the shared workspace it adds a cheap
//! `git worktree` (shared object store, private working tree + index) checked
//! out on a fresh per-worker branch off the repo's current `HEAD`. The worker's
//! tool server is pointed at this directory instead of the shared workspace, so
//! two concurrent workers can never see or clobber each other's edits.
//!
//! # Guarantees
//!
//! - **Isolation.** Each worker gets its own working tree and index per repo, so
//!   `git add`/`commit`/`checkout` in one worker are invisible to siblings.
//! - **Correct starting point.** Each worktree starts at the repo's current
//!   `HEAD`; the worker checks out or creates its target branch inside its own
//!   worktree, never mutating the shared checkout's branch.
//! - **Cleanup with forensics.** [`WorkerWorkspace::release`] removes the
//!   worktrees on success. On failure the caller keeps the workspace for
//!   forensics; [`reap_orphaned`] bounds retained workspaces on startup.
//! - **No behaviour change for the degenerate case.** If the shared workspace
//!   holds no git repos, provisioning yields an isolated-but-empty directory and
//!   falls back transparently; a single worker sees no difference.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::WorkerId;

/// Directory (under the shared workspace) that holds all per-worker isolated
/// workspaces. Kept under `.spacebot` so it never looks like a project repo to
/// [`crate::projects::git::discover_repos`].
const WORKSPACES_SUBDIR: &str = ".spacebot/worker-workspaces";

/// Maximum number of leftover per-worker workspaces to retain on startup before
/// reaping the oldest. Bounds disk use while keeping recent failures for
/// forensics.
const MAX_RETAINED_WORKSPACES: usize = 20;

/// An isolated per-worker workspace: a directory containing one `git worktree`
/// per repo discovered in the shared workspace.
///
/// Drop does **not** clean up — cleanup is explicit via [`Self::release`] so the
/// caller controls the success-vs-forensics policy.
#[derive(Debug)]
pub struct WorkerWorkspace {
    /// The shared workspace this isolated workspace was derived from. Needed to
    /// run `git worktree remove` from each source repo on release.
    shared_root: PathBuf,
    /// Root of the isolated workspace (`<shared>/.spacebot/worker-workspaces/<id>`).
    root: PathBuf,
    /// The worktrees created, as `(source_repo_path, worktree_path)` pairs.
    worktrees: Vec<IsolatedRepo>,
}

/// One repo mirrored into an isolated workspace as a git worktree.
#[derive(Debug, Clone)]
struct IsolatedRepo {
    /// Path to the source repo in the shared workspace (the worktree's parent).
    source: PathBuf,
    /// Path to the isolated worktree.
    worktree: PathBuf,
    /// The per-worker branch created in the worktree.
    branch: String,
}

impl WorkerWorkspace {
    /// The isolated workspace root the worker's tools should use as their
    /// base directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether any repo was isolated. When `false`, the isolated root is empty
    /// and the caller may prefer to fall back to the shared workspace.
    pub fn has_worktrees(&self) -> bool {
        !self.worktrees.is_empty()
    }

    /// Provision an isolated workspace for `worker_id` off `shared_root`.
    ///
    /// Discovers git repos directly under `shared_root` and adds a per-worker
    /// worktree for each on a fresh branch off the repo's current `HEAD`.
    /// Returns the workspace even if no repos were found (empty isolated root),
    /// so callers get a consistent, isolated base directory regardless.
    ///
    /// Errors only on unrecoverable filesystem failures (cannot create the
    /// workspace root). A repo that fails to isolate is logged and skipped
    /// rather than failing the whole worker — partial isolation is strictly
    /// better than the shared checkout.
    pub async fn provision(shared_root: &Path, worker_id: WorkerId) -> anyhow::Result<Self> {
        let root = shared_root
            .join(WORKSPACES_SUBDIR)
            .join(worker_id.to_string());
        tokio::fs::create_dir_all(&root).await.map_err(|e| {
            anyhow::anyhow!("failed to create worker workspace {}: {e}", root.display())
        })?;

        let repos = discover_repo_dirs(shared_root).await;
        let branch = worker_branch_name(worker_id);
        let mut worktrees = Vec::new();

        for source in repos {
            let name = match source.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };
            let worktree = root.join(&name);
            match add_worktree(&source, &worktree, &branch).await {
                Ok(()) => worktrees.push(IsolatedRepo {
                    source,
                    worktree,
                    branch: branch.clone(),
                }),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        repo = %source.display(),
                        %worker_id,
                        "failed to isolate repo into worker worktree — skipping"
                    );
                }
            }
        }

        tracing::info!(
            %worker_id,
            worktrees = worktrees.len(),
            root = %root.display(),
            "provisioned isolated worker workspace"
        );

        Ok(Self {
            shared_root: shared_root.to_path_buf(),
            root,
            worktrees,
        })
    }

    /// Remove all worktrees and the isolated workspace directory.
    ///
    /// Call on **successful** completion. On failure, skip this to retain the
    /// workspace for forensics — [`reap_orphaned`] bounds retention.
    pub async fn release(self) -> anyhow::Result<()> {
        for repo in &self.worktrees {
            if let Err(error) = remove_worktree(&repo.source, &repo.worktree).await {
                tracing::warn!(
                    %error,
                    worktree = %repo.worktree.display(),
                    "failed to remove worker worktree — leaving for reaper"
                );
            }
            // Best-effort: delete the per-worker branch so it doesn't accumulate.
            let _ = delete_branch(&repo.source, &repo.branch).await;
        }

        if let Err(error) = tokio::fs::remove_dir_all(&self.root).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                %error,
                root = %self.root.display(),
                "failed to remove isolated workspace directory"
            );
        }

        // Prune any dangling worktree administrative files.
        for repo in &self.worktrees {
            let _ = prune_worktrees(&repo.source).await;
        }

        let _ = &self.shared_root; // retained for clarity; source paths carry it.
        Ok(())
    }
}

/// The per-worker branch name used inside each isolated worktree. Namespaced so
/// it never collides with a real feature branch and is trivially greppable.
fn worker_branch_name(worker_id: WorkerId) -> String {
    format!("spacebot/worker/{worker_id}")
}

/// Discover git repository directories directly under `shared_root`.
///
/// A repo is a child directory containing a `.git` *directory* (a real
/// checkout). Directories with a `.git` *file* are existing worktrees and are
/// skipped, as are hidden directories (including our own workspaces subdir).
async fn discover_repo_dirs(shared_root: &Path) -> Vec<PathBuf> {
    let mut repos = Vec::new();
    let mut entries = match tokio::fs::read_dir(shared_root).await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(%error, root = %shared_root.display(), "failed to scan shared workspace for repos");
            return repos;
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        let dot_git = path.join(".git");
        // Only real checkouts (`.git` directory), not worktrees (`.git` file).
        if dot_git.is_dir() {
            repos.push(path);
        }
    }

    repos.sort();
    repos
}

/// Add a git worktree for `source` at `worktree_path` on a fresh `branch` off
/// the source's current `HEAD`.
///
/// If a worktree already exists at the path (leaked from a prior run), it is
/// removed first so provisioning is idempotent.
async fn add_worktree(source: &Path, worktree_path: &Path, branch: &str) -> anyhow::Result<()> {
    let worktree_str = worktree_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("worktree path is not valid UTF-8"))?;

    if worktree_path.exists() {
        let _ = remove_worktree(source, worktree_path).await;
        let _ = prune_worktrees(source).await;
    }
    // A stale branch of the same name blocks `-b`; delete it first (best effort).
    let _ = delete_branch(source, branch).await;

    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            "--force",
            "-b",
            branch,
            worktree_str,
            "HEAD",
        ])
        .current_dir(source)
        .output()
        .await?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!(
        "git worktree add failed in {}: {}",
        source.display(),
        stderr.trim()
    );
}

/// Remove a git worktree (`git worktree remove --force`).
async fn remove_worktree(source: &Path, worktree_path: &Path) -> anyhow::Result<()> {
    let worktree_str = worktree_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("worktree path is not valid UTF-8"))?;

    let output = Command::new("git")
        .args(["worktree", "remove", "--force", worktree_str])
        .current_dir(source)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "git worktree remove failed in {}: {}",
            source.display(),
            stderr.trim()
        );
    }
    Ok(())
}

/// Prune stale worktree administrative entries (`git worktree prune`).
async fn prune_worktrees(source: &Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(source)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree prune failed: {}", stderr.trim());
    }
    Ok(())
}

/// Delete a local branch (`git branch -D`). Best-effort; used to keep the
/// per-worker branch namespace from accumulating.
async fn delete_branch(source: &Path, branch: &str) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["branch", "-D", branch])
        .current_dir(source)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git branch -D failed: {}", stderr.trim());
    }
    Ok(())
}

/// Reap orphaned per-worker workspaces left behind by crashes or retained
/// failures, keeping at most [`MAX_RETAINED_WORKSPACES`] most-recent ones.
///
/// Called on startup so leaked worktrees don't accumulate unbounded. Also runs
/// `git worktree prune` on each repo so git's administrative view stays clean.
pub async fn reap_orphaned(shared_root: &Path) -> anyhow::Result<usize> {
    let workspaces_dir = shared_root.join(WORKSPACES_SUBDIR);
    if !workspaces_dir.exists() {
        return Ok(0);
    }

    // Collect (mtime, path) for each per-worker workspace directory.
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut entries = tokio::fs::read_dir(&workspaces_dir).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let mtime = entry
            .metadata()
            .await
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        dirs.push((mtime, path));
    }

    // Newest first; retain the first MAX_RETAINED_WORKSPACES, reap the rest.
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    let mut reaped = 0;
    for (_, path) in dirs.into_iter().skip(MAX_RETAINED_WORKSPACES) {
        if let Err(error) = tokio::fs::remove_dir_all(&path).await {
            tracing::warn!(%error, path = %path.display(), "failed to reap orphaned worker workspace");
        } else {
            reaped += 1;
        }
    }

    // Prune each source repo's worktree list so removed dirs don't linger.
    for repo in discover_repo_dirs(shared_root).await {
        let _ = prune_worktrees(&repo).await;
    }

    if reaped > 0 {
        tracing::info!(reaped, "reaped orphaned worker workspaces");
    }
    Ok(reaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;
    use uuid::Uuid;

    async fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Create a shared workspace containing one initialised repo with a commit.
    async fn make_shared_workspace_with_repo(repo_name: &str) -> (tempfile::TempDir, PathBuf) {
        let shared = tempfile::tempdir().expect("tempdir");
        let repo = shared.path().join(repo_name);
        tokio::fs::create_dir_all(&repo).await.unwrap();
        git(&repo, &["init", "-q", "-b", "main"]).await;
        git(&repo, &["config", "user.email", "t@t.io"]).await;
        git(&repo, &["config", "user.name", "T"]).await;
        tokio::fs::write(repo.join("README.md"), "base\n")
            .await
            .unwrap();
        git(&repo, &["add", "README.md"]).await;
        git(&repo, &["commit", "-q", "-m", "init"]).await;
        (shared, repo)
    }

    #[tokio::test]
    async fn provisions_isolated_worktree_per_repo() {
        let (shared, _repo) = make_shared_workspace_with_repo("app").await;
        let worker_id = Uuid::new_v4();

        let ws = WorkerWorkspace::provision(shared.path(), worker_id)
            .await
            .expect("provision");

        assert!(ws.has_worktrees(), "expected one isolated worktree");
        let isolated_repo = ws.root().join("app");
        assert!(isolated_repo.join(".git").exists(), "worktree checked out");
        // The isolated worktree is NOT the shared checkout.
        assert_ne!(isolated_repo, shared.path().join("app"));

        ws.release().await.expect("release");
    }

    /// The core acceptance property: two concurrent workers cannot see or
    /// clobber each other's edits, and never commit to the shared branch.
    #[tokio::test]
    async fn concurrent_workers_cannot_contaminate_each_other() {
        let (shared, repo) = make_shared_workspace_with_repo("app").await;

        let worker_a = Uuid::new_v4();
        let worker_b = Uuid::new_v4();
        let ws_a = WorkerWorkspace::provision(shared.path(), worker_a)
            .await
            .expect("provision a");
        let ws_b = WorkerWorkspace::provision(shared.path(), worker_b)
            .await
            .expect("provision b");

        let a_repo = ws_a.root().join("app");
        let b_repo = ws_b.root().join("app");

        // Worker A writes and commits a file that ONLY it touched.
        tokio::fs::write(a_repo.join("a_only.txt"), "from A\n")
            .await
            .unwrap();
        git(&a_repo, &["add", "a_only.txt"]).await;
        git(&a_repo, &["commit", "-q", "-m", "A work"]).await;

        // Worker B, running concurrently, does a broad `git add -A` — the exact
        // sweep that caused issue #224 — then commits.
        tokio::fs::write(b_repo.join("b_only.txt"), "from B\n")
            .await
            .unwrap();
        git(&b_repo, &["add", "-A"]).await;
        git(&b_repo, &["commit", "-q", "-m", "B work"]).await;

        // B's commit must NOT contain A's file (no cross-contamination).
        let b_tree = Command::new("git")
            .args(["show", "--name-only", "--format=", "HEAD"])
            .current_dir(&b_repo)
            .output()
            .await
            .unwrap();
        let b_files = String::from_utf8_lossy(&b_tree.stdout);
        assert!(b_files.contains("b_only.txt"), "B committed its own file");
        assert!(
            !b_files.contains("a_only.txt"),
            "B's `git add -A` swept up A's edit — isolation FAILED: {b_files}"
        );

        // A does not see B's file in its working tree either.
        assert!(
            !a_repo.join("b_only.txt").exists(),
            "A sees B's file — leaked"
        );

        // The shared checkout's branch is untouched: still `main` at the base
        // commit, with neither worker's file present.
        assert!(
            !repo.join("a_only.txt").exists(),
            "shared tree got A's file"
        );
        assert!(
            !repo.join("b_only.txt").exists(),
            "shared tree got B's file"
        );
        let head = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&head.stdout).trim(),
            "main",
            "shared checkout branch changed"
        );

        ws_a.release().await.expect("release a");
        ws_b.release().await.expect("release b");
    }

    #[tokio::test]
    async fn empty_workspace_yields_no_worktrees() {
        let shared = tempfile::tempdir().expect("tempdir");
        let ws = WorkerWorkspace::provision(shared.path(), Uuid::new_v4())
            .await
            .expect("provision");
        assert!(!ws.has_worktrees());
        ws.release().await.expect("release");
    }

    #[tokio::test]
    async fn reap_orphaned_bounds_retained_workspaces() {
        let (shared, _repo) = make_shared_workspace_with_repo("app").await;
        // Create more than the retention bound of leftover workspaces.
        let ws_dir = shared.path().join(WORKSPACES_SUBDIR);
        tokio::fs::create_dir_all(&ws_dir).await.unwrap();
        for i in 0..(MAX_RETAINED_WORKSPACES + 5) {
            tokio::fs::create_dir_all(ws_dir.join(format!("leftover-{i}")))
                .await
                .unwrap();
        }
        let reaped = reap_orphaned(shared.path()).await.expect("reap");
        assert_eq!(reaped, 5, "should reap the oldest 5 over the bound");
    }
}
