//! Dynamic project registry: auto-discovers GitHub repositories and keeps
//! a persistent index of repos with per-repo config overrides.

pub mod pr_conflicts;
pub mod store;
pub mod sync;

pub use store::{RegistryRepo, RegistryStore};
pub use sync::{SyncResult, SyncStatus};
