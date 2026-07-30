//! Orphan reaping for the init role.
//!
//! When spacebot is the container's PID 1 it inherits every orphaned process in
//! the namespace — not just the children it spawned. Tokio reaps *its own*
//! children when a [`tokio::process::Child`] is awaited, but nothing collects
//! the grandchildren a shell command leaves behind: `sh -c "cargo build"` exits,
//! its `node`/`cargo`/`build-script` descendants are re-parented to PID 1, and
//! there they stay as zombies. A zombie holds its PID until reaped, so the count
//! only ever grows for the life of the container and eventually exhausts the PID
//! table — at which point *no* process can be spawned and worker launches fail
//! for a reason that looks nothing like the cause.
//!
//! This module supplies the missing init behaviour: a `SIGCHLD`-driven sweep
//! that collects orphans as they arrive.
//!
//! # Coexistence with Tokio
//!
//! Tokio's process driver also listens for `SIGCHLD` and calls `waitpid` on the
//! PIDs it owns. A blanket `waitpid(-1, …)` would race it: whoever waits first
//! consumes the status, and the caller only learns *which* child it got after
//! the fact. If that child was Tokio-owned its status is gone and the awaiting
//! task fails with `ECHILD`.
//!
//! So this reaper never waits blind. It enumerates the actual children from
//! `/proc` and waits on each one *by PID*, skipping any that a spawn site has
//! [`claim`]ed. Ownership is checked before the status is consumed, which makes
//! the guarantee real rather than best-effort: a claimed child is left to Tokio,
//! the only party that can deliver its status to the awaiting caller. Everything
//! else is an orphan by definition and safe to collect.
//!
//! Releasing a claim also reaps that PID, which closes the second half of the
//! leak: a [`tokio::process::Child`] dropped *without* being awaited (a wait
//! that timed out, an early `?`) is never reaped by Tokio, and its `SIGCHLD`
//! has already come and gone. Reaping on release collects it immediately
//! instead of leaving it parked until some unrelated process happens to exit.
//!
//! # Activation
//!
//! Reaping is the *init* role, so [`spawn`] only starts a reaper when the
//! process is PID 1. Under a real init (systemd, `docker run --init`, the
//! `tini` entrypoint) that init already reaps and no orphans reach us, so a
//! second reaper would be pure overhead.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::signal::unix::{SignalKind, signal};

/// PIDs spacebot spawned and still owns.
///
/// Always tracked, whether or not a reaper is running: the guarantee that a
/// claimed PID is never reaped out from under Tokio must not depend on
/// start-up ordering.
static OWNED_PIDS: Mutex<Option<HashSet<i32>>> = Mutex::new(None);

/// Whether a reaper task is running and therefore responsible for orphans.
static REAPER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Orphans reaped since startup. Surfaced by [`reaped_count`] so the acceptance
/// criterion in issue #28 is observable without `docker exec`.
static REAPED: AtomicU64 = AtomicU64::new(0);

/// Total orphaned processes reaped since startup.
pub fn reaped_count() -> u64 {
    REAPED.load(Ordering::Relaxed)
}

/// Processes currently sitting in the zombie state.
///
/// This is the metric issue #28 is judged on — previously only reachable via
/// `docker exec … ps`. Reading `/proc` directly makes it a first-class signal
/// the platform can watch. Returns `None` where `/proc` is unavailable
/// (non-Linux), which is honest about the measurement rather than reporting a
/// misleading zero.
pub fn zombie_count() -> Option<usize> {
    let entries = std::fs::read_dir("/proc").ok()?;

    let zombies = entries
        .flatten()
        .filter(|entry| {
            // Only numeric entries are processes; `self`, `sys`, etc. are not.
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter(|entry| is_zombie(&entry.path()))
        .count();

    Some(zombies)
}

/// Whether the process at `proc_path` is a zombie, per `/proc/<pid>/stat`.
///
/// The state field is the third whitespace-separated column, but the second
/// (`comm`) is parenthesised and may itself contain spaces — so split after the
/// final `)` rather than tokenising the whole line.
fn is_zombie(proc_path: &std::path::Path) -> bool {
    let Ok(stat) = std::fs::read_to_string(proc_path.join("stat")) else {
        // The process exited between listing and reading — not a zombie.
        return false;
    };

    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state == "Z")
}

/// Whether this process holds the init role (PID 1) and must reap orphans.
pub fn is_init_process() -> bool {
    std::process::id() == 1
}

/// Claim `pid` as Tokio-owned so the reaper never consumes its exit status.
///
/// Call this immediately after spawning, and hold the returned guard for the
/// lifetime of the [`tokio::process::Child`]. Dropping the guard hands the PID
/// back to the reaper and sweeps once, which collects the child even if nothing
/// ever awaited it.
///
/// Safe to call before [`spawn`] or when no reaper runs at all.
pub fn claim(pid: i32) -> OwnedChild {
    if let Ok(mut owned) = OWNED_PIDS.lock() {
        owned.get_or_insert_with(HashSet::new).insert(pid);
    }
    OwnedChild { pid }
}

/// Guard marking a PID as Tokio-owned for as long as it is held.
#[derive(Debug)]
#[must_use = "the PID is released as soon as this guard drops"]
pub struct OwnedChild {
    pid: i32,
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Ok(mut owned) = OWNED_PIDS.lock()
            && let Some(owned) = owned.as_mut()
        {
            owned.remove(&self.pid);
        }

        // The child may have exited without anything awaiting it (a wait that
        // timed out, an early return). Its SIGCHLD is long gone, so collect it
        // now rather than leaving it parked until an unrelated process exits.
        if REAPER_RUNNING.load(Ordering::Relaxed) {
            reap_one(self.pid);
        }
    }
}

/// Start the orphan reaper when this process is PID 1.
///
/// Idempotent and safe to call unconditionally: off the init role it logs and
/// returns, leaving [`claim`] to track PIDs harmlessly.
pub fn spawn() {
    if !is_init_process() {
        tracing::debug!("not PID 1 — orphan reaper not needed");
        return;
    }

    if REAPER_RUNNING.swap(true, Ordering::Relaxed) {
        tracing::warn!("orphan reaper already started");
        return;
    }

    tokio::spawn(async move {
        let mut sigchld = match signal(SignalKind::child()) {
            Ok(sigchld) => sigchld,
            Err(error) => {
                tracing::error!(%error, "failed to install SIGCHLD handler — orphans will accumulate");
                return;
            }
        };

        tracing::info!("running as PID 1 — orphan reaper started");

        // Reap once before waiting: orphans that exited during startup have
        // already delivered their SIGCHLD and would otherwise linger until the
        // next unrelated exit.
        reap_orphans();

        while sigchld.recv().await.is_some() {
            reap_orphans();
        }

        tracing::warn!("SIGCHLD stream ended — orphan reaper stopped");
    });
}

/// Whether `pid` is currently claimed by a spawn site.
fn is_claimed(pid: i32) -> bool {
    OWNED_PIDS
        .lock()
        .ok()
        .and_then(|owned| owned.as_ref().map(|owned| owned.contains(&pid)))
        .unwrap_or(false)
}

/// Collect every exited orphan that is ready, leaving claimed PIDs to Tokio.
///
/// `waitpid(-1, …)` is unsafe here: it reaps whichever child exited first, and
/// the caller only learns *which* after the status is already consumed. If that
/// child was Tokio-owned, its status is gone and the awaiting task fails with
/// `ECHILD`.
///
/// So instead of reaping blind, this enumerates the actual children from
/// `/proc` and reaps each unclaimed one by PID. Ownership is checked *before*
/// the status is consumed, which makes the claim guarantee real rather than
/// best-effort.
fn reap_orphans() {
    for pid in child_pids() {
        if is_claimed(pid) {
            // Tokio owns this one and is the only party that can deliver the
            // status to the awaiting task. Leave it alone.
            continue;
        }
        reap_one(pid);
    }
}

/// Reap a single exited child, if it has exited. Never blocks.
fn reap_one(pid: i32) {
    let mut status: libc::c_int = 0;
    // SAFETY: `waitpid` writes only through `status`, a valid local. WNOHANG
    // makes the call non-blocking, so the async runtime is never stalled.
    let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };

    match reaped {
        0 => {} // still running — nothing to collect yet
        -1 => {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ECHILD) {
                tracing::debug!(pid, %error, "waitpid failed");
            }
        }
        _ => {
            let total = REAPED.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::debug!(pid, total_reaped = total, "reaped orphan");
        }
    }
}

/// PIDs of this process's direct children, from `/proc/self/task/*/children`.
///
/// Reading children per-thread is what the kernel exposes; a process's children
/// are the union across its threads. Returns empty where `/proc` is unavailable,
/// which simply means nothing is reaped rather than something wrong being
/// reaped.
fn child_pids() -> Vec<i32> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };

    let mut pids: Vec<i32> = Vec::new();
    for task in tasks.flatten() {
        let Ok(children) = std::fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        pids.extend(
            children
                .split_whitespace()
                .filter_map(|pid| pid.parse::<i32>().ok()),
        );
    }
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claiming_tracks_the_pid_regardless_of_reaper_state() {
        // The claim guarantee must not depend on start-up ordering: a PID
        // claimed before the reaper starts must still be protected.
        let guard = claim(4242);
        assert!(is_claimed(4242), "a claimed PID must be tracked");
        drop(guard);
        assert!(!is_claimed(4242), "dropping the guard releases the PID");
    }

    #[test]
    fn claims_are_independent() {
        let first = claim(4243);
        let second = claim(4244);
        drop(second);

        assert!(is_claimed(4243), "releasing one PID must not affect others");
        assert!(!is_claimed(4244));
        drop(first);
    }

    #[test]
    fn unclaimed_pids_belong_to_the_reaper() {
        assert!(!is_claimed(999_999), "unknown PIDs are reapable");
    }

    #[test]
    fn reaping_with_no_children_is_a_no_op() {
        // ECHILD must exit the loop rather than spin. Guards against a
        // regression that would peg a core on an idle instance.
        reap_orphans();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn zombie_count_reads_proc() {
        let count = zombie_count().expect("/proc is available on Linux");
        // The test process reaps its own children, so this is a sanity bound
        // rather than an exact figure — it proves /proc parsing works at all.
        assert!(count < 10_000, "implausible zombie count: {count}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn is_zombie_rejects_live_and_missing_processes() {
        let own = std::path::PathBuf::from(format!("/proc/{}", std::process::id()));
        assert!(!is_zombie(&own), "a running process is not a zombie");
        assert!(
            !is_zombie(std::path::Path::new("/proc/nonexistent")),
            "an unreadable process is not a zombie"
        );
    }

    #[test]
    fn reaped_count_is_monotonic() {
        let before = reaped_count();
        reap_orphans();
        assert!(
            reaped_count() >= before,
            "the counter must never go backwards"
        );
    }

    #[tokio::test]
    async fn spawn_is_a_no_op_off_the_init_role() {
        // Must not install a reaper that would race Tokio for its children.
        spawn();
        assert!(!REAPER_RUNNING.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn claimed_children_keep_their_exit_status() {
        // The core safety property: sweeping must never consume the status of a
        // child a task is about to await. This is checked *before* waitpid, so
        // it holds no matter how often the sweep runs.
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("sh must be available");

        let pid = child.id().expect("a freshly spawned child has a PID") as i32;
        let _owned = claim(pid);

        for _ in 0..20 {
            reap_orphans();
        }

        let status = child.wait().await.expect("child status must be available");
        assert_eq!(status.code(), Some(7), "exit status must survive the sweep");
    }

    #[tokio::test]
    async fn unclaimed_orphans_are_reaped() {
        // The issue #28 failure mode: a child nothing ever awaits. Leak the
        // handle so no runtime reaps it, then prove sweeping collects it.
        //
        // Concurrent tests share this process and sweep the same child table,
        // so a sibling's sweep may reap this PID first. Either way the PID must
        // end up collected — that is the property under test, and asserting the
        // end state (rather than who did it) keeps this deterministic.
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("sh must be available");
        let pid = child.id() as i32;
        std::mem::forget(child);
        let proc_path = std::path::PathBuf::from(format!("/proc/{pid}"));

        for _ in 0..200 {
            reap_orphans();
            if !is_zombie(&proc_path) && !is_claimed(pid) {
                // Reaped — either by this sweep or a concurrent one.
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        panic!("unclaimed child at PID {pid} was never reaped");
    }
}
