// Best-effort cleanup / IO across state_dir.rs and friends: `let _ =
// remove_file(...)` etc. are intentional fire-and-forget (a stale file
// that fails to delete isn't an error the caller should react to).
#![allow(clippy::let_underscore_must_use)]

//! ccsight library: the byte seams plus the bounded dependency closure they
//! need. The TUI / MCP / aggregator / run loop stay in the binary (`main.rs`),
//! which re-exports these modules so `crate::` paths resolve in both crates.
//!
//! Nothing outside the binary consumes this crate, so the split earns its keep
//! only if a second consumer appears — but the modules are `pub`, so a release
//! that collapses it is a semver break for anyone depending on `ccsight` as a
//! library.

pub mod domain;
pub mod parser;
pub mod pins;

/// Light path / atomic-write helpers the config-loader seam needs. The heavy
/// infrastructure (live_sessions, cache, search_index, …) stays in the binary;
/// only these two self-contained leaves move here, re-exported by the binary's
/// `infrastructure` module so `crate::infrastructure::…` paths are unchanged.
pub mod infrastructure {
    pub mod atomic;
    pub mod state_dir;

    pub use atomic::{atomic_write, atomic_write_with};
    pub use state_dir::{
        cache_path, ensure_private_state_root, home_dir, index_dir, migrate_legacy_state_dirs,
        pins_path, search_history_path,
    };
}
