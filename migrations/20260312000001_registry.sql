-- Dynamic project registry: auto-discovered GitHub repositories.
-- Tracks repos from `gh repo list` with per-repo config overrides.

CREATE TABLE IF NOT EXISTS registry_repos (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    full_name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    default_branch TEXT NOT NULL DEFAULT 'main',
    is_archived INTEGER NOT NULL DEFAULT 0,
    is_fork INTEGER NOT NULL DEFAULT 0,
    visibility TEXT NOT NULL DEFAULT 'private',
    language TEXT,
    local_path TEXT,
    clone_url TEXT NOT NULL,
    ssh_url TEXT NOT NULL DEFAULT '',
    -- Per-repo overrides (NULL = inherit from agent defaults)
    worker_model TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    -- Linkage to existing projects table
    project_id TEXT,
    -- Sync metadata
    last_synced_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE SET NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_registry_repos_full_name
    ON registry_repos(agent_id, full_name);
CREATE INDEX IF NOT EXISTS idx_registry_repos_agent
    ON registry_repos(agent_id);
CREATE INDEX IF NOT EXISTS idx_registry_repos_enabled
    ON registry_repos(agent_id, enabled);
