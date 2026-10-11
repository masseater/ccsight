// `atomic` + `state_dir` live in the library crate; re-export `state_dir` (used
// module-qualified by sibling modules) and the bare functions so existing
// `crate::infrastructure::…` paths keep resolving.
pub use ccsight::infrastructure::state_dir;

mod cache;
pub mod cowork_source;
mod file_discovery;
pub mod live_diagnostic;
pub mod live_sessions;
pub mod live_snapshots;
pub mod mcp_config;
pub mod resource_config;
pub mod search_index;

pub use cache::*;
pub(crate) use ccsight::infrastructure::{atomic_write, atomic_write_with};
pub use ccsight::infrastructure::{
    cache_path, ensure_private_state_root, home_dir, index_dir, migrate_legacy_state_dirs,
    search_history_path,
};
pub use cowork_source::{
    cowork_session_id, is_cowork_audit_path, resolve_cowork_title, resolve_project_name,
};
pub use file_discovery::{FileDiscovery, RetentionWarning, check_cleanup_period};
pub use mcp_config::{McpServerStatus, compute_mcp_status};
pub use resource_config::{ConfiguredResources, discover_configured_resources};
pub use search_index::SearchIndex;
