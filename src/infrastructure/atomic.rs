//! Single durable atomic-write primitive for all `~/.ccsight/` JSON state.
//!
//! tmp + rename is the standard atomic pattern, but two details bite when
//! hand-rolled: the data isn't durable until `sync_all` (lint #29), and
//! 0o600 must be applied AFTER the handle closes because some filesystems
//! (macOS APFS observed) reset perms on close. Centralizing both here keeps
//! every writer consistent instead of each re-deriving it.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// Durably write `bytes` to `path` via a sibling `.tmp` + rename, 0o600 on
/// Unix. Recovers once from a stale wrong-owned tmp by removing it and
/// retrying the create; the target itself is only ever replaced by rename.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_with(path, |w| w.write_all(bytes))
}

/// Streaming variant: `write` receives a `BufWriter` over the tmp file so
/// serializers can stream straight to disk instead of building the whole
/// payload in memory first — the cache file is tens of MB, and a `to_vec`
/// there spikes RSS on every save.
pub(crate) fn atomic_write_with(
    path: &Path,
    write: impl FnOnce(&mut io::BufWriter<File>) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Per-process tmp name: the TUI and the MCP server instances write
    // cache.json concurrently — a shared tmp name lets one process rename
    // the other's half-written tmp into place, corrupting the target. With
    // distinct tmps each rename publishes a complete file (last writer
    // wins). Crash-stranded tmps: swept by ensure_private_state_root.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let f = match File::create(&tmp) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            // Stale wrong-owned tmp: remove it and retry. NEVER touch the
            // target here — rename(2) needs only directory write permission
            // to replace it, and deleting it before the new data exists
            // would lose the user's file if the retry also fails.
            let _ = fs::remove_file(&tmp);
            File::create(&tmp)?
        }
        Err(e) => return Err(e),
    };
    // Any failure past this point must remove the tmp — the per-process name
    // means no later writer truncates it, so a leak (e.g. a tens-of-MB cache
    // tmp stranded by ENOSPC mid-write) would persist until the next sweep.
    let result = (|| {
        let mut w = io::BufWriter::new(f);
        write(&mut w)?;
        let f = w.into_inner().map_err(io::IntoInnerError::into_error)?;
        f.sync_all()?;
        drop(f);
        // chmod after close: APFS silently resets perms when the handle drops.
        set_owner_only(&tmp)?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_bytes_and_is_owner_only_on_unix() {
        let dir = std::env::temp_dir().join(format!("ccsight-atomic-{}", std::process::id()));
        let path = dir.join("x.json");
        atomic_write(&path, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"a\":1}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // No leftover tmp after a successful write.
        let tmp = dir.join(format!("x.json.tmp.{}", std::process::id()));
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_write_removes_tmp_and_keeps_target() {
        // The pid-suffixed tmp has no later writer to truncate it, so a
        // serialization failure mid-write must clean it up itself — and the
        // existing target must survive untouched.
        let dir = std::env::temp_dir().join(format!("ccsight-atomic3-{}", std::process::id()));
        let path = dir.join("z.json");
        atomic_write(&path, b"keep").unwrap();
        let err = atomic_write_with(&path, |_| Err(io::Error::other("serializer blew up")));
        assert!(err.is_err());
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        assert!(!tmp.exists(), "tmp must be cleaned up on write failure");
        assert_eq!(fs::read(&path).unwrap(), b"keep", "target must survive");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = std::env::temp_dir().join(format!("ccsight-atomic2-{}", std::process::id()));
        let path = dir.join("y.json");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let _ = fs::remove_dir_all(&dir);
    }
}
