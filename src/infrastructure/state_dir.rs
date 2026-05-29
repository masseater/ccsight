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

pub fn live_snapshot_path() -> Result<PathBuf> {
    Ok(state_root()?.join("live_snapshot.json"))
}

pub fn search_history_path() -> Result<PathBuf> {
    Ok(state_root()?.join("search_history.json"))
}

/// One-shot migration from the pre-1.1 split paths. Idempotent: safe to call
/// every startup. Renames `~/.cache/ccsight/{cache.json,index/}` and
/// `~/.config/ccsight/pins.json` into `~/.ccsight/`. If the new path already
/// exists (user already migrated, or both old and new co-exist for some
/// reason), the legacy file is left untouched — we never overwrite.
///
/// All errors are silenced because state directories are best-effort: a
/// migration failure should never block startup. The next run will retry.
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
