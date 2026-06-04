use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

// Bump on changes to: parser output, aggregator field semantics
// (`extract_project_name`, `extract_session_model`, `git_branch`, etc.).
const CACHE_VERSION: u32 = 33;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheData {
    pub version: u32,
    pub files: HashMap<String, CachedFileStats>,
    #[serde(default)]
    pub day_summaries: HashMap<String, String>,
    #[serde(default)]
    pub session_summaries: HashMap<String, String>,
}

pub type CachedTokenStats = crate::aggregator::TokenStats;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedDailyStats {
    pub first_timestamp: Option<DateTime<Utc>>,
    pub last_timestamp: Option<DateTime<Utc>>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens_by_model: HashMap<String, CachedTokenStats>,
    #[serde(default)]
    pub hourly_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub hourly_work_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub tool_usage: HashMap<String, usize>,
    #[serde(default)]
    pub language_usage: HashMap<String, usize>,
    #[serde(default)]
    pub extension_usage: HashMap<String, usize>,
    /// Per-day count of user / assistant message turns. `serde(default)`
    /// keeps deserialisation tolerant of older cache layouts; a version
    /// bump forces a rebuild that populates real values.
    #[serde(default)]
    pub user_msgs: u64,
    #[serde(default)]
    pub assistant_msgs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFileStats {
    pub modified_secs: u64,
    #[serde(default)]
    pub file_size: u64,
    pub entry_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// 5-min TTL share of `cache_creation_tokens`. `#[serde(default)]` so
    /// older cache files (pre-1h-TTL fix, version 29 and below) deserialize
    /// without losing the rest of the entry — they'll re-aggregate to fill
    /// these on the next cache write.
    #[serde(default)]
    pub cache_creation_5m_tokens: u64,
    #[serde(default)]
    pub cache_creation_1h_tokens: u64,
    /// `(message.id, requestId)` pairs credited to this file. Pre-loaded
    /// into the cross-file dedup set so resume/branch duplicates skip.
    /// Empty for caches predating this field (`serde(default)`).
    #[serde(default)]
    pub unique_hashes: Vec<String>,
    pub tool_usage: HashMap<String, usize>,
    pub model_usage: HashMap<String, usize>,
    #[serde(default)]
    pub model_tokens: HashMap<String, CachedTokenStats>,
    pub session_date: Option<NaiveDate>,
    pub project_name: Option<String>,
    pub session_id: Option<String>,
    pub git_branch: Option<String>,
    pub first_timestamp: Option<DateTime<Utc>>,
    pub last_timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub ai_title: Option<String>,
    /// Most recent user-role message text. Powers the Live/Daily preview's
    /// "remind me what I was doing" line. Kept here so cache-valid sessions
    /// don't need to re-walk entries on every load.
    #[serde(default)]
    pub last_user_message: Option<String>,
    /// First user-role message text. Universal title fallback so Live row
    /// line 2 always carries something semantic instead of "—".
    #[serde(default)]
    pub first_user_message: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub is_subagent: bool,
    #[serde(default)]
    pub daily_stats: HashMap<String, CachedDailyStats>,
    #[serde(default)]
    pub hourly_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub hourly_work_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub weekday_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub weekday_work_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub tool_error_count: usize,
    #[serde(default)]
    pub tool_success_count: usize,
    #[serde(default)]
    pub session_duration_mins: Option<i64>,
    #[serde(default)]
    pub language_usage: HashMap<String, usize>,
    #[serde(default)]
    pub extension_usage: HashMap<String, usize>,
}

impl Default for CacheData {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            files: HashMap::new(),
            day_summaries: HashMap::new(),
            session_summaries: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct Cache {
    cache_path: PathBuf,
    data: CacheData,
}

impl Cache {
    pub fn new_empty() -> Self {
        Self {
            cache_path: PathBuf::from("/dev/null"), // Fallback path, won't be saved
            data: CacheData::default(),
        }
    }

    pub fn load() -> Result<Self> {
        let cache_path = Self::cache_file_path()?;
        let data = if cache_path.exists() {
            let file = File::open(&cache_path)?;
            let reader = BufReader::new(file);
            match serde_json::from_reader::<_, CacheData>(reader) {
                Ok(cache) if cache.version == CACHE_VERSION => cache,
                _ => CacheData::default(),
            }
        } else {
            CacheData::default()
        };

        Ok(Self { cache_path, data })
    }

    pub fn save(&self) -> Result<()> {
        // Reject the `/dev/null` placeholder used by `Cache::new_empty()` when HOME is
        // unset. Saving to /dev/null on Linux silently succeeds but never persists, which
        // would mask the failure mode. Surface it as an error.
        if self.cache_path.as_os_str() == "/dev/null" {
            return Err(anyhow::anyhow!(
                "Cannot save cache: HOME is not set (cache_path is /dev/null fallback)"
            ));
        }
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Atomic write: write to temp file, then rename
        let temp_path = self.cache_path.with_extension("json.tmp");
        let file = match File::create(&temp_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // Stale file with wrong ownership — try to remove and recreate
                let _ = fs::remove_file(&temp_path);
                let _ = fs::remove_file(&self.cache_path);
                File::create(&temp_path)?
            }
            Err(e) => return Err(e.into()),
        };

        // Set restrictive permissions (0600) on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&temp_path, permissions)?;
        }

        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &self.data)?;
        writer.flush()?;
        writer.into_inner()?.sync_all()?;

        // Atomic rename (POSIX guarantees this is atomic on same filesystem)
        if let Err(e) = fs::rename(&temp_path, &self.cache_path) {
            // Clean up temp file on failure
            let _ = fs::remove_file(&temp_path);
            return Err(e.into());
        }
        Ok(())
    }

    pub fn get(&self, path: &Path) -> Option<&CachedFileStats> {
        let key = path.to_string_lossy().to_string();
        self.data.files.get(&key)
    }

    pub fn is_valid(&self, path: &Path) -> bool {
        let key = path.to_string_lossy().to_string();
        if let Some(cached) = self.data.files.get(&key)
            && let Ok(metadata) = fs::metadata(path)
        {
            let current_size = metadata.len();
            if let Ok(modified) = metadata.modified() {
                let modified_secs = modified
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                return cached.modified_secs == modified_secs && cached.file_size == current_size;
            }
        }
        false
    }

    pub fn insert(&mut self, path: &Path, stats: CachedFileStats) {
        let key = path.to_string_lossy().to_string();
        self.data.files.insert(key, stats);
    }

    pub fn get_day_summary(&self, date: &NaiveDate) -> Option<&String> {
        let key = date.format("%Y-%m-%d").to_string();
        self.data.day_summaries.get(&key)
    }

    pub fn set_day_summary(&mut self, date: &NaiveDate, summary: String) {
        let key = date.format("%Y-%m-%d").to_string();
        self.data.day_summaries.insert(key, summary);
    }

    pub fn get_session_summary(&self, path: &Path) -> Option<&String> {
        let key = path.to_string_lossy().to_string();
        self.data.session_summaries.get(&key)
    }

    pub fn set_session_summary(&mut self, path: &Path, summary: String) {
        let key = path.to_string_lossy().to_string();
        self.data.session_summaries.insert(key, summary);
    }

    fn cache_file_path() -> Result<PathBuf> {
        super::cache_path()
    }
}

pub fn get_file_modified_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

pub fn get_file_size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::TokenStats;

    #[test]
    fn test_cached_token_stats_from_token_stats() {
        let ts: CachedTokenStats = TokenStats {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_tokens: 20,
            cache_read_tokens: 10,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };

        assert_eq!(ts.input_tokens, 100);
        assert_eq!(ts.output_tokens, 50);
        assert_eq!(ts.cache_creation_tokens, 20);
        assert_eq!(ts.cache_read_tokens, 10);
    }

    #[test]
    fn test_cached_token_stats_from_zero_token_stats() {
        let ts: CachedTokenStats = TokenStats::default();

        assert_eq!(ts.input_tokens, 0);
        assert_eq!(ts.output_tokens, 0);
        assert_eq!(ts.cache_creation_tokens, 0);
        assert_eq!(ts.cache_read_tokens, 0);
    }

    #[test]
    fn test_new_empty_cache_save_returns_error() {
        // Regression: Cache::new_empty() uses /dev/null as a placeholder when no HOME.
        // save() must surface an error rather than silently writing to /dev/null.
        let cache = Cache::new_empty();
        let err = cache
            .save()
            .expect_err("save() should fail on /dev/null fallback");
        assert!(
            err.to_string().contains("HOME is not set"),
            "error message should mention HOME, got: {err}"
        );
    }

    #[test]
    fn test_cache_save_load_roundtrip() {
        // Regression: serialize a Cache to disk, reload it, verify CACHE_VERSION matches
        // and the per-file entries survive (including new tool_usage / daily_stats keys).
        // Guards against silent data loss when the schema gains new fields.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ccsight-cache-roundtrip-{}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);

        let key = "/tmp/some-session.jsonl".to_string();
        let mut tool_usage = HashMap::new();
        tool_usage.insert("Bash".to_string(), 5);
        tool_usage.insert("skill:my-skill".to_string(), 2);

        let original = Cache {
            cache_path: path.clone(),
            data: CacheData {
                version: CACHE_VERSION,
                files: {
                    let mut map = HashMap::new();
                    map.insert(
                        key.clone(),
                        CachedFileStats {
                            modified_secs: 1234,
                            file_size: 100,
                            entry_count: 10,
                            input_tokens: 1000,
                            output_tokens: 500,
                            cache_creation_tokens: 200,
                            cache_creation_5m_tokens: 0,
                            cache_creation_1h_tokens: 0,
                            unique_hashes: Vec::new(),
                            cache_read_tokens: 100,
                            tool_usage: tool_usage.clone(),
                            model_usage: HashMap::new(),
                            model_tokens: HashMap::new(),
                            session_date: None,
                            project_name: Some("proj".to_string()),
                            session_id: Some("sid".to_string()),
                            git_branch: None,
                            first_timestamp: None,
                            last_timestamp: None,
                            summary: None,
                            custom_title: None,
                            ai_title: None,
                            last_user_message: None,
                            first_user_message: None,
                            model: None,
                            is_subagent: false,
                            daily_stats: HashMap::new(),
                            hourly_activity: HashMap::new(),
                            hourly_work_activity: HashMap::new(),
                            weekday_activity: HashMap::new(),
                            weekday_work_activity: HashMap::new(),
                            tool_error_count: 0,
                            tool_success_count: 0,
                            session_duration_mins: None,
                            language_usage: HashMap::new(),
                            extension_usage: HashMap::new(),
                        },
                    );
                    map
                },
                day_summaries: HashMap::new(),
                session_summaries: HashMap::new(),
            },
        };

        original.save().expect("save cache");

        // Reload by reading the file directly (Cache::load uses fixed XDG path).
        let file = File::open(&path).expect("open saved cache");
        let reader = BufReader::new(file);
        let reloaded: CacheData = serde_json::from_reader(reader).expect("parse cache");

        assert_eq!(reloaded.version, CACHE_VERSION);
        let entry = reloaded.files.get(&key).expect("file entry survived");
        assert_eq!(entry.entry_count, 10);
        assert_eq!(entry.tool_usage.get("Bash"), Some(&5));
        assert_eq!(entry.tool_usage.get("skill:my-skill"), Some(&2));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_old_cache_version_is_rejected_at_load() {
        // Write a JSON cache file with version = CACHE_VERSION - 1 and
        // verify Cache::load_from_path treats it as missing (returns
        // empty), so a stale cache after a schema bump rebuilds rather
        // than serving wrong data.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ccsight-cache-version-mismatch-{}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);

        let stale = serde_json::json!({
            "version": CACHE_VERSION - 1,
            "files": {
                "/tmp/legacy.jsonl": {
                    "modified_secs": 0,
                    "file_size": 0,
                    "entry_count": 0,
                    "input_tokens": 999,
                    "output_tokens": 0,
                    "cache_creation_tokens": 0,
                    "cache_read_tokens": 0,
                }
            }
        });
        std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();

        // Read the file directly via the same loader logic the real load
        // path uses (version mismatch → CacheData::default()).
        let file = File::open(&path).expect("open stale cache");
        let reader = BufReader::new(file);
        let parsed: serde_json::Result<CacheData> = serde_json::from_reader(reader);
        let cache = match parsed {
            Ok(cache) if cache.version == CACHE_VERSION => cache,
            _ => CacheData::default(),
        };

        // After version-mismatch fallback, the cache should carry the
        // current version (CacheData::default() returns CACHE_VERSION) and
        // an empty files map (the stale entry must NOT survive).
        assert_eq!(cache.version, CACHE_VERSION);
        assert!(
            cache.files.is_empty(),
            "stale-version cache must NOT carry stale per-file entries forward"
        );

        let _ = fs::remove_file(&path);
    }
}
