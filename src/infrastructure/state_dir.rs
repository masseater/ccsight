//! Single source of truth for ccsight's on-disk state paths.
//!
//! All state lives under `~/.ccsight/` so it sits next to Claude Code's
//! `~/.claude/` (the data ccsight reads from) and stays out of Claude Code's
//! own ownership tree. Same paths on macOS and Linux for documentation /
//! debugging consistency.
//!
//! Layout:
//!   ~/.ccsight/cache.json   — parsed-session JSON cache (incremental)
//!   ~/.ccsight/index/       — tantivy full-text index segments
//!   ~/.ccsight/pins.json    — pinned-session list
//!
//! Pre-1.1 versions used `~/.cache/ccsight/` (cache + index) and
//! `~/.config/ccsight/` (pins). `migrate_legacy_state_dirs()` is called once
//! at startup and renames any leftover files into the new layout, so
//! existing users don't lose their cache or pins.

use std::path::PathBuf;

use anyhow::Result;

/// Root directory for ccsight state. `~/.ccsight/` on both macOS and Linux.
pub fn state_root() -> Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join(".ccsight"))
}

pub fn cache_path() -> Result<PathBuf> {
    Ok(state_root()?.join("cache.json"))
}

pub fn index_dir() -> Result<PathBuf> {
    Ok(state_root()?.join("index"))
}

pub fn pins_path() -> Result<PathBuf> {
    Ok(state_root()?.join("pins.json"))
}

pub fn live_diagnostic_path() -> Result<PathBuf> {
    Ok(state_root()?.join("live_snapshot.json"))
}

/// Directory holding per-poll alive-session history files
/// (`<YYYY-MM-DD>-<HHMM>.json`). See [`super::live_snapshots`] for the
/// write semantics (diff detection + 30-min debounce, tiered retention).
pub fn live_snapshots_dir() -> Result<PathBuf> {
    Ok(state_root()?.join("live_snapshots"))
}

pub fn search_history_path() -> Result<PathBuf> {
    Ok(state_root()?.join("search_history.json"))
}

/// Create the state root and restrict it to the owner (0o700). The directory
/// holds conversation-derived data whose per-file modes vary (tantivy writes
/// world-readable index segments), so the directory itself is the privacy
/// boundary. Best-effort — startup must not block on a chmod failure.
/// Also sweeps crash-stranded atomic-write tmps (see [`sweep_stale_tmps`]).
pub fn ensure_private_state_root() {
    let Ok(root) = state_root() else {
        return;
    };
    let _ = std::fs::create_dir_all(&root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    }
    sweep_stale_tmps(&root);
    if let Ok(snaps) = live_snapshots_dir() {
        sweep_stale_tmps(&snaps);
    }
}

/// Remove crash-stranded `*.json.tmp.<pid>` files. Atomic writes use
/// per-process tmp names, so a SIGKILL mid-save (Claude Desktop reaps its MCP
/// instances freely) strands a tmp no later writer truncates — a cache tmp is
/// tens of MB and would otherwise accumulate forever. The mtime guard keeps a
/// concurrently-writing process's in-flight tmp safe.
fn sweep_stale_tmps(dir: &std::path::Path) {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(3600);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.contains(".json.tmp.") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE_AFTER);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Idempotent legacy-path migration into `~/.ccsight/`. Never overwrites
/// an existing target. Errors silenced — startup must not block; next
/// run retries.
pub fn migrate_legacy_state_dirs() {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let home = PathBuf::from(home);
    let Ok(new_root) = state_root() else {
        return;
    };

    let migrations = [
        (
            home.join(".cache/ccsight/cache.json"),
            new_root.join("cache.json"),
        ),
        (home.join(".cache/ccsight/index"), new_root.join("index")),
        (
            home.join(".config/ccsight/pins.json"),
            new_root.join("pins.json"),
        ),
        // In-tree rename: the early multi-snapshot prototype called this
        // directory `daily/` before semantics evolved to per-poll history.
        // `live_snapshots/` matches the diagnostic singleton file at the
        // root (`live_snapshot.json`) — singular = current, plural = history.
        (new_root.join("daily"), new_root.join("live_snapshots")),
    ];

    let needs_migration = migrations
        .iter()
        .any(|(old, new)| old.exists() && !new.exists());
    if !needs_migration {
        return;
    }

    let _ = std::fs::create_dir_all(&new_root);

    for (old, new) in &migrations {
        if !old.exists() || new.exists() {
            continue;
        }
        // Rename across same filesystem; falls back to copy+delete if cross-FS.
        if std::fs::rename(old, new).is_err() {
            // Best-effort copy fallback for directories or cross-FS moves.
            // Atomicity: if the copy fails partway, remove the partial dst
            // BEFORE leaving so the next startup retries (the loop guard
            // checks `new.exists()`; without cleanup a half-copied dst
            // would silently lock us into broken state).
            if old.is_dir() {
                if copy_dir_recursive(old, new).is_ok() {
                    let _ = std::fs::remove_dir_all(old);
                } else {
                    let _ = std::fs::remove_dir_all(new);
                }
            } else if std::fs::copy(old, new).is_ok() {
                let _ = std::fs::remove_file(old);
            } else {
                let _ = std::fs::remove_file(new);
            }
        }
    }

    // Try to clean up empty legacy parent dirs. Failures don't matter.
    let _ = std::fs::remove_dir(home.join(".cache/ccsight"));
    let _ = std::fs::remove_dir(home.join(".config/ccsight"));
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}
