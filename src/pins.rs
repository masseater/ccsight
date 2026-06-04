use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const PINS_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct PinsData {
    version: u32,
    pins: Vec<PinEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PinEntry {
    pub path: PathBuf,
    pub pinned_at: DateTime<Utc>,
}

pub struct Pins {
    data_path: PathBuf,
    entries: Vec<PinEntry>,
    lookup: HashSet<PathBuf>,
}

impl Pins {
    pub fn empty() -> Self {
        Self {
            data_path: Self::default_path().unwrap_or_else(|_| PathBuf::from("/dev/null")),
            entries: Vec::new(),
            lookup: HashSet::new(),
        }
    }

    pub fn load() -> Result<Self> {
        let data_path = Self::default_path()?;
        let (entries, lookup) = if data_path.exists() {
            let file = File::open(&data_path)?;
            let reader = BufReader::new(file);
            match serde_json::from_reader::<_, PinsData>(reader) {
                Ok(data) if data.version == PINS_VERSION => {
                    let lookup: HashSet<PathBuf> =
                        data.pins.iter().map(|e| e.path.clone()).collect();
                    (data.pins, lookup)
                }
                _ => {
                    // Parse failure / version mismatch: move file aside instead
                    // of zeroing, otherwise the next toggle+save would
                    // overwrite real pins with a single-entry file. Rename
                    // failures are silent (best-effort recovery).
                    let backup = data_path.with_extension(format!(
                        "json.corrupt-{}",
                        chrono::Utc::now().format("%Y%m%dT%H%M%S")
                    ));
                    let _ = fs::rename(&data_path, &backup);
                    (Vec::new(), HashSet::new())
                }
            }
        } else {
            (Vec::new(), HashSet::new())
        };

        Ok(Self {
            data_path,
            entries,
            lookup,
        })
    }

    pub fn save(&self) -> Result<()> {
        // Reject the `/dev/null` placeholder used by `Pins::empty()` when HOME is unset.
        // Saving to /dev/null on Linux silently succeeds but never persists, masking the
        // failure. Surface it as an error so callers can inform the user.
        if self.data_path.as_os_str() == "/dev/null" {
            return Err(anyhow::anyhow!(
                "Cannot save pins: HOME is not set (data_path is /dev/null fallback)"
            ));
        }
        if let Some(parent) = self.data_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temp_path = self.data_path.with_extension("json.tmp");
        let file = File::create(&temp_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&temp_path, permissions)?;
        }

        let data = PinsData {
            version: PINS_VERSION,
            pins: self.entries.clone(),
        };

        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &data)?;
        writer.flush()?;
        writer.into_inner()?.sync_all()?;

        if let Err(e) = fs::rename(&temp_path, &self.data_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(e.into());
        }
        Ok(())
    }

    pub fn toggle(&mut self, path: &Path) -> bool {
        if self.lookup.contains(path) {
            self.entries.retain(|e| e.path != path);
            self.lookup.remove(path);
            false
        } else {
            let entry = PinEntry {
                path: path.to_path_buf(),
                pinned_at: Utc::now(),
            };
            self.entries.insert(0, entry);
            self.lookup.insert(path.to_path_buf());
            true
        }
    }

    pub fn is_pinned(&self, path: &Path) -> bool {
        self.lookup.contains(path)
    }

    pub fn entries(&self) -> &[PinEntry] {
        &self.entries
    }

    pub fn remove(&mut self, path: &Path) {
        self.entries.retain(|e| e.path != path);
        self.lookup.remove(path);
    }

    /// Swap the entry at `idx` with the one above it. No-op when `idx`
    /// is at the top or out of range. Used by the pins popup so users
    /// can rearrange their order manually instead of being stuck with
    /// pin-time ordering.
    pub fn move_up(&mut self, idx: usize) -> bool {
        if idx == 0 || idx >= self.entries.len() {
            return false;
        }
        self.entries.swap(idx, idx - 1);
        true
    }

    /// Swap the entry at `idx` with the one below it.
    pub fn move_down(&mut self, idx: usize) -> bool {
        if idx + 1 >= self.entries.len() {
            return false;
        }
        self.entries.swap(idx, idx + 1);
        true
    }

    fn default_path() -> Result<PathBuf> {
        crate::infrastructure::pins_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_toggle_pin() {
        let mut pins = Pins::empty();
        let path = PathBuf::from("/tmp/test.jsonl");

        assert!(!pins.is_pinned(&path));
        assert!(pins.toggle(&path));
        assert!(pins.is_pinned(&path));
        assert!(!pins.toggle(&path));
        assert!(!pins.is_pinned(&path));
    }

    #[test]
    fn test_remove_pin() {
        let mut pins = Pins::empty();
        let path = PathBuf::from("/tmp/test.jsonl");

        pins.toggle(&path);
        assert!(pins.is_pinned(&path));
        pins.remove(&path);
        assert!(!pins.is_pinned(&path));
    }

    #[test]
    fn test_toggle_inserts_at_front() {
        let mut pins = Pins::empty();
        let p1 = PathBuf::from("/tmp/a.jsonl");
        let p2 = PathBuf::from("/tmp/b.jsonl");

        pins.toggle(&p1);
        pins.toggle(&p2);

        assert_eq!(pins.entries[0].path, p2);
        assert_eq!(pins.entries[1].path, p1);
    }

    #[test]
    fn test_empty_pins_save_returns_error() {
        // Regression: Pins::empty() falls back to /dev/null when HOME is unset. save()
        // must return an error rather than silently writing to /dev/null.
        let pins = Pins::empty();
        if pins.data_path.as_os_str() == "/dev/null" {
            let err = pins
                .save()
                .expect_err("save() should fail on /dev/null fallback");
            assert!(
                err.to_string().contains("HOME is not set"),
                "error message should mention HOME, got: {err}"
            );
        }
        // If HOME is set, default path is real and save would succeed; skip the assertion.
    }

    #[test]
    fn test_save_load_roundtrip() {
        // Regression: writing pins to disk and reading them back must reproduce the
        // exact entry list (order + paths). Guards against accidental schema drift.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ccsight-pins-roundtrip-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut pins = Pins {
            data_path: path.clone(),
            entries: Vec::new(),
            lookup: HashSet::new(),
        };
        let p1 = PathBuf::from("/tmp/a.jsonl");
        let p2 = PathBuf::from("/tmp/b.jsonl");
        pins.toggle(&p1);
        pins.toggle(&p2);
        pins.save().expect("save pins");

        // Reload from the same on-disk file.
        let file = File::open(&path).expect("open saved pins");
        let reader = BufReader::new(file);
        let data: PinsData = serde_json::from_reader(reader).expect("parse pins");

        assert_eq!(data.version, PINS_VERSION);
        assert_eq!(data.pins.len(), 2);
        // Most recently toggled is at the front.
        assert_eq!(data.pins[0].path, p2);
        assert_eq!(data.pins[1].path, p1);

        let _ = std::fs::remove_file(&path);
    }
}
