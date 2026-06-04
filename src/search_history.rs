//! Persisted query history for the global search popup. Stores the most
//! recent N queries in chronological order (newest last) at
//! `~/.ccsight/search_history.json`. The Esc-keeps-text behaviour covers
//! the "restore last query" case; this module covers everything older.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Cap so a long-lived user doesn't accumulate megabytes of dead queries.
/// 100 entries is enough to scroll back through a session's worth of
/// experimentation without making the JSON file unwieldy.
const MAX_ENTRIES: usize = 100;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct OnDisk {
    entries: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct SearchHistory {
    entries: VecDeque<String>,
    /// Cursor into `entries`. None = the user is composing a fresh query
    /// (not browsing). Some(i) = entries[i] is the surfaced past query.
    /// Recall direction: Up = older (i++), Down = newer (i--, eventually None).
    cursor: Option<usize>,
    /// Draft snapshot taken when the user first presses Up. Restoring it
    /// on Down past index 0 means the user can browse and back-out without
    /// losing what they were typing.
    draft: Option<String>,
}

impl SearchHistory {
    pub fn load() -> Self {
        let Ok(path) = crate::infrastructure::search_history_path() else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        let on_disk: OnDisk = serde_json::from_slice(&bytes).unwrap_or_default();
        let mut entries = VecDeque::from(on_disk.entries);
        while entries.len() > MAX_ENTRIES {
            entries.pop_front();
        }
        Self {
            entries,
            cursor: None,
            draft: None,
        }
    }

    /// Append a query to the history (and persist). Dedupes against the
    /// last entry so spamming Enter on the same query doesn't bloat the
    /// file. Empty queries are ignored.
    pub fn push(&mut self, query: &str) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.entries.back().is_some_and(|last| last == trimmed) {
            self.reset_cursor();
            return;
        }
        // Move existing duplicate to the end rather than keeping two copies.
        if let Some(pos) = self.entries.iter().position(|e| e == trimmed) {
            self.entries.remove(pos);
        }
        self.entries.push_back(trimmed.to_string());
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
        self.reset_cursor();
        // Tests share the same `infrastructure::search_history_path()`
        // and would otherwise overwrite the developer's real history file
        // when run on their workstation. Save is production-only.
        #[cfg(not(test))]
        let _ = self.save();
    }

    // The only caller is `#[cfg(not(test))]` above, so test builds see `save`
    // as dead. It is live in production — keep it, silence the test-only lint.
    #[cfg_attr(test, allow(dead_code))]
    fn save(&self) -> Result<()> {
        let path = crate::infrastructure::search_history_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let on_disk = OnDisk {
            entries: self.entries.iter().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&on_disk)?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Step to the previous (older) history entry. Stashes `current_draft`
    /// the first time so Down can restore it.
    pub fn step_back(&mut self, current_draft: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => {
                self.draft = Some(current_draft.to_string());
                self.entries.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.cursor = Some(next);
        self.entries.get(next).cloned()
    }

    /// Step to the next (newer) entry. When we step past the last entry
    /// (newest), return the stashed draft so the user can resume editing.
    pub fn step_forward(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let cur = self.cursor?;
        if cur + 1 < self.entries.len() {
            self.cursor = Some(cur + 1);
            self.entries.get(cur + 1).cloned()
        } else {
            self.cursor = None;
            self.draft.take()
        }
    }

    /// Drop the active recall cursor — called whenever the user types or
    /// closes the popup, since further edits should start a fresh history
    /// trip on the next Up.
    pub fn reset_cursor(&mut self) {
        self.cursor = None;
        self.draft = None;
    }

    pub fn is_browsing(&self) -> bool {
        self.cursor.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_back_returns_newest_then_older() {
        let mut h = SearchHistory::default();
        h.entries.push_back("a".into());
        h.entries.push_back("b".into());
        h.entries.push_back("c".into());
        assert_eq!(h.step_back("draft").as_deref(), Some("c"));
        assert_eq!(h.step_back("draft").as_deref(), Some("b"));
        assert_eq!(h.step_back("draft").as_deref(), Some("a"));
        // Bottoms out at oldest, no underflow.
        assert_eq!(h.step_back("draft").as_deref(), Some("a"));
    }

    #[test]
    fn step_forward_restores_draft_past_newest() {
        let mut h = SearchHistory::default();
        h.entries.push_back("a".into());
        h.entries.push_back("b".into());
        assert_eq!(h.step_back("my draft").as_deref(), Some("b"));
        // Past the newest, we get the original draft back.
        assert_eq!(h.step_forward().as_deref(), Some("my draft"));
        // After resuming the draft we're no longer browsing.
        assert!(!h.is_browsing());
    }

    #[test]
    fn push_dedupes_against_recent() {
        let mut h = SearchHistory::default();
        h.entries.push_back("same".into());
        // Without ever saving (load path unused in tests), just exercise the
        // dedup logic by calling push directly.
        let _ = h.entries.clone();
        // Push a duplicate of the last entry — should be a no-op.
        h.push("same");
        assert_eq!(h.entries.len(), 1);
        h.push("other");
        assert_eq!(h.entries.len(), 2);
    }
}
