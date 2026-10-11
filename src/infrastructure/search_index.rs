use std::collections::HashMap;
use std::fs;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, RangeQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, OwnedValue, STORED, STRING, Schema, TextFieldIndexing,
    TextOptions,
};
use tantivy::tokenizer::{LowerCaser, NgramTokenizer, TextAnalyzer};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term, doc};

use chrono::{Local, NaiveDate};

use crate::aggregator::DailyGroup;
use crate::domain::{EntryType, Role};
use crate::parser::JsonlParser;
use crate::search::{SearchMatchType, SearchResult, extract_snippet};

const INDEX_VERSION: u32 = 7;
const TOKENIZER_NAME: &str = "ngram23";

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    files: HashMap<String, u64>,
}

struct Fields {
    session_path: Field,
    day_idx: Field,
    session_idx: Field,
    role: Field,
    text: Field,
    msg_ts: Field,
}

struct ParsedDoc {
    session_path: String,
    day_idx: u64,
    session_idx: u64,
    role: String,
    text: String,
    msg_ts: Option<i64>,
}

pub struct SearchIndex {
    reader: IndexReader,
    fields: Fields,
    query_parser: QueryParser,
}

fn get_u64(doc: &TantivyDocument, field: Field) -> u64 {
    doc.get_first(field)
        .map(OwnedValue::from)
        .and_then(|v| match v {
            OwnedValue::U64(n) => Some(n),
            _ => None,
        })
        .unwrap_or(0)
}

fn get_i64(doc: &TantivyDocument, field: Field) -> Option<i64> {
    doc.get_first(field)
        .map(OwnedValue::from)
        .and_then(|v| match v {
            OwnedValue::I64(n) => Some(n),
            _ => None,
        })
}

/// Epoch-second bounds of inclusive local dates; UTC seconds keep the stored
/// value independent of the zone the index was built in.
fn local_day_bounds(from: Option<NaiveDate>, to: Option<NaiveDate>) -> (Bound<i64>, Bound<i64>) {
    // A zone that springs forward at midnight has no 00:00 that day; the
    // day then starts at the first hour that exists, not at "unbounded".
    let start_of = |d: NaiveDate| {
        (0..3).find_map(|h| {
            d.and_hms_opt(h, 0, 0)
                .and_then(|t| t.and_local_timezone(Local).earliest())
                .map(|t| t.timestamp())
        })
    };
    let lower = from
        .and_then(start_of)
        .map_or(Bound::Unbounded, Bound::Included);
    let upper = to
        .and_then(|d| d.succ_opt())
        .and_then(start_of)
        .map_or(Bound::Unbounded, Bound::Excluded);
    (lower, upper)
}

fn get_str(doc: &TantivyDocument, field: Field) -> String {
    doc.get_first(field)
        .map(OwnedValue::from)
        .and_then(|v| match v {
            OwnedValue::Str(s) => Some(s),
            _ => None,
        })
        .unwrap_or_default()
}

fn register_tokenizer(index: &Index) {
    let ngram = TextAnalyzer::builder(NgramTokenizer::new(2, 3, false).unwrap())
        .filter(LowerCaser)
        .build();
    index.tokenizers().register(TOKENIZER_NAME, ngram);
}

fn build_file_map(daily_groups: &[DailyGroup]) -> HashMap<String, u64> {
    daily_groups
        .iter()
        .flat_map(crate::aggregator::DailyGroup::user_sessions)
        .map(|s| {
            let key = s.file_path.to_string_lossy().to_string();
            let mtime = fs::metadata(&s.file_path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            (key, mtime)
        })
        .collect()
}

/// Sessions new or written to since indexing, whose messages the index lacks.
fn changed_file_count(old: &HashMap<String, u64>, current: &HashMap<String, u64>) -> usize {
    current
        .iter()
        .filter(|(path, mtime)| old.get(*path) != Some(*mtime))
        .count()
}

fn parse_session(session_path: &Path, day_idx: usize, session_idx: usize) -> Vec<ParsedDoc> {
    let Ok(entries) = JsonlParser::parse_file(session_path) else {
        return Vec::new();
    };
    let path_str = session_path.to_string_lossy().to_string();

    entries
        .iter()
        .filter(|e| e.entry_type == EntryType::User || e.entry_type == EntryType::Assistant)
        .filter_map(|e| {
            let message = e.message.as_ref()?;
            let text = message.content.extract_text();
            if text.is_empty() {
                return None;
            }
            let role = match message.role {
                Role::User => "User",
                Role::Assistant => "AI",
                _ => return None,
            };
            Some(ParsedDoc {
                session_path: path_str.clone(),
                day_idx: day_idx as u64,
                session_idx: session_idx as u64,
                role: role.to_string(),
                text,
                msg_ts: e.timestamp.map(|t| t.timestamp()),
            })
        })
        .collect()
}

/// Tightened `LogMergePolicy` for 30s-cadence incremental writes:
/// `min_num_segments=3` and `del_docs_ratio_before_merge=0.3` (defaults
/// 8 / 1.0 let segments and `.del` files accumulate forever). Paired
/// with `wait_merging_threads` in `build` / `update_or_build`.
fn install_merge_policy(writer: &IndexWriter) {
    let mut policy = tantivy::indexer::LogMergePolicy::default();
    policy.set_min_num_segments(3);
    policy.set_del_docs_ratio_before_merge(0.3);
    writer.set_merge_policy(Box::new(policy));
}

fn write_docs(writer: &IndexWriter, fields: &Fields, docs: &[ParsedDoc]) -> anyhow::Result<()> {
    for d in docs {
        let mut document = doc!(
            fields.session_path => d.session_path.clone(),
            fields.day_idx => d.day_idx,
            fields.session_idx => d.session_idx,
            fields.role => d.role.clone(),
            fields.text => d.text.clone(),
        );
        if let Some(ts) = d.msg_ts {
            document.add_i64(fields.msg_ts, ts);
        }
        writer.add_document(document)?;
    }
    Ok(())
}

/// One task per session file, at its newest day. A file appears in every day
/// group it was active on, and each task parses the whole file, so a task per
/// appearance indexes a long-running session once per active day — its copies
/// then crowd every other session out of a result page.
fn collect_tasks(daily_groups: &[DailyGroup]) -> Vec<(usize, usize, PathBuf)> {
    let mut seen = std::collections::HashSet::new();
    daily_groups
        .iter()
        .enumerate()
        .flat_map(|(day_idx, group)| {
            group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .enumerate()
                .map(move |(session_idx, s)| (day_idx, session_idx, s.file_path.clone()))
        })
        .filter(|(_, _, path)| seen.insert(path.clone()))
        .collect()
}

/// Sessions in the first-run head commit. `daily_groups` is newest-first,
/// so the head covers the most recent (most-searched) sessions and the
/// early handle makes search usable within seconds of a cold start.
const FIRST_COMMIT_SESSIONS: usize = 200;

/// Transcript bytes parsed into `ParsedDoc`s before handing them to the
/// writer. Budgeted in bytes, not sessions: session sizes span four orders of
/// magnitude, so a session-count batch still holds most of the corpus whenever
/// the large transcripts land in one batch. A single over-budget file forms its
/// own batch — nothing smaller is available.
const INDEX_PARSE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// Parse `tasks` in bounded chunks, writing each chunk before parsing the
/// next. Does not commit — the caller decides commit points, so bounding
/// memory costs no extra segment flushes.
fn index_in_chunks(
    writer: &IndexWriter,
    fields: &Fields,
    tasks: &[(usize, usize, PathBuf)],
) -> anyhow::Result<()> {
    for chunk in byte_budget_chunks(tasks, INDEX_PARSE_BUDGET_BYTES) {
        let parsed: Vec<ParsedDoc> = chunk
            .par_iter()
            .flat_map(|(day_idx, session_idx, path)| parse_session(path, *day_idx, *session_idx))
            .collect();
        write_docs(writer, fields, &parsed)?;
    }
    Ok(())
}

/// Split `tasks` into runs whose transcripts sum to at most `budget` bytes.
/// An unreadable file counts as zero rather than ending the run: the budget is
/// a memory guard, and a stat failure is not a reason to change batching.
fn byte_budget_chunks(
    tasks: &[(usize, usize, PathBuf)],
    budget: u64,
) -> Vec<&[(usize, usize, PathBuf)]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut acc = 0u64;
    for (i, (_, _, path)) in tasks.iter().enumerate() {
        let size = std::fs::metadata(path).map_or(0, |m| m.len());
        if i > start && acc.saturating_add(size) > budget {
            out.push(&tasks[start..i]);
            start = i;
            acc = 0;
        }
        acc = acc.saturating_add(size);
    }
    if start < tasks.len() {
        out.push(&tasks[start..]);
    }
    out
}

impl SearchIndex {
    pub fn update_or_build(daily_groups: &[DailyGroup]) -> anyhow::Result<Self> {
        Self::update_or_build_with_early(daily_groups, &|_| {})
    }

    /// Same as `update_or_build`, but on a full (first-run) build `early`
    /// receives a read handle right after the head chunk commits, so the
    /// caller can serve searches while the tail is still indexing. The
    /// early handle's snapshot is frozen — the caller must replace it
    /// with the returned final handle.
    pub fn update_or_build_with_early(
        daily_groups: &[DailyGroup],
        early: &dyn Fn(Self),
    ) -> anyhow::Result<Self> {
        let index_dir = Self::index_path()?;

        if !index_dir.exists() {
            return Self::build(daily_groups, early);
        }

        let manifest_path = index_dir.join("manifest.json");
        let manifest = match fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|data| serde_json::from_str::<Manifest>(&data).ok())
        {
            Some(m) if m.version == INDEX_VERSION => m,
            _ => return Self::build(daily_groups, early),
        };

        let current_files = build_file_map(daily_groups);

        // Empty manifest with non-empty data = corrupt (filtered groups
        // were saved at some point); rebuild instead of incrementally
        // re-adding every file every launch.
        if manifest.files.is_empty() && !current_files.is_empty() {
            return Self::build(daily_groups, early);
        }

        if manifest.files == current_files {
            return Self::open_existing(&index_dir);
        }

        Self::update_incremental(daily_groups, &index_dir, &manifest, &current_files)
    }

    /// Read-only: building walks every transcript and writing needs a lock
    /// another ccsight process may hold, so either leaves a caller on a
    /// request timeout with nothing. An out-of-date index is still returned —
    /// callers remap hits by `session_path`, as the TUI does — with how many
    /// sessions it has not caught up on.
    pub(crate) fn open_for_read(daily_groups: &[DailyGroup]) -> Option<(Self, usize)> {
        let index_dir = Self::index_path().ok()?;
        let manifest: Manifest = fs::read_to_string(index_dir.join("manifest.json"))
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())?;
        if manifest.version != INDEX_VERSION {
            return None;
        }
        let behind = changed_file_count(&manifest.files, &build_file_map(daily_groups));
        Self::open_existing(&index_dir).ok().map(|h| (h, behind))
    }

    fn build(daily_groups: &[DailyGroup], early: &dyn Fn(Self)) -> anyhow::Result<Self> {
        let index_dir = Self::index_path()?;
        Self::build_in(&index_dir, daily_groups, early, FIRST_COMMIT_SESSIONS)
    }

    /// `index_dir` and `first_chunk` are injectable so tests can build in a
    /// temp dir with a tiny head instead of touching the real index.
    /// A build under `index_dir` for tests outside this module, which must not
    /// reach the developer's real index.
    #[cfg(test)]
    pub(crate) fn build_for_test(
        index_dir: &Path,
        daily_groups: &[DailyGroup],
    ) -> anyhow::Result<Self> {
        Self::build_in(index_dir, daily_groups, &|_| {}, FIRST_COMMIT_SESSIONS)
    }

    fn build_in(
        index_dir: &Path,
        daily_groups: &[DailyGroup],
        early: &dyn Fn(Self),
        first_chunk: usize,
    ) -> anyhow::Result<Self> {
        if index_dir.exists() {
            // Another ccsight process holding the writer lock is mid-build or
            // mid-update; deleting the directory under it corrupts its index.
            // The probe is dropped before the delete, so no lock outlives it.
            use tantivy::directory::Directory;
            let busy = tantivy::directory::MmapDirectory::open(index_dir).is_ok_and(|d| {
                d.acquire_lock(&tantivy::directory::INDEX_WRITER_LOCK)
                    .is_err()
            });
            if busy {
                anyhow::bail!("search index is being written by another process");
            }
            let _ = fs::remove_dir_all(index_dir);
        }
        fs::create_dir_all(index_dir)?;

        let (schema, fields) = Self::create_schema();
        let dir = tantivy::directory::MmapDirectory::open(index_dir)?;
        let index = Index::open_or_create(dir, schema)?;
        register_tokenizer(&index);

        let mut writer: IndexWriter = index.writer(500_000_000)?;
        install_merge_policy(&writer);

        let tasks = collect_tasks(daily_groups);
        let (head, tail) = tasks.split_at(tasks.len().min(first_chunk));
        index_in_chunks(&writer, &fields, head)?;
        writer.commit()?;
        if !tail.is_empty() {
            // Early snapshot covers the head (newest sessions) while the
            // tail is still indexing; skipped when this commit was final.
            if let Ok(handle) = Self::open_existing(index_dir) {
                early(handle);
            }
            index_in_chunks(&writer, &fields, tail)?;
            writer.commit()?;
        }
        // Block until merge threads finish; without this, writer drops
        // mid-merge and pending segments leak across reloads. One-time
        // cost; faster steady-state.
        writer.wait_merging_threads()?;

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let query_parser = QueryParser::for_index(&index, vec![fields.text]);

        Self::save_manifest(daily_groups, index_dir)?;

        Ok(Self {
            reader,
            fields,
            query_parser,
        })
    }

    fn open_existing(index_dir: &Path) -> anyhow::Result<Self> {
        let (schema, fields) = Self::create_schema();
        let dir = tantivy::directory::MmapDirectory::open(index_dir)?;
        let index = Index::open_or_create(dir, schema)?;
        register_tokenizer(&index);

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let query_parser = QueryParser::for_index(&index, vec![fields.text]);

        Ok(Self {
            reader,
            fields,
            query_parser,
        })
    }

    fn update_incremental(
        daily_groups: &[DailyGroup],
        index_dir: &Path,
        old_manifest: &Manifest,
        current_files: &HashMap<String, u64>,
    ) -> anyhow::Result<Self> {
        let mut to_delete: Vec<String> = Vec::new();
        let mut to_add_paths: Vec<String> = Vec::new();

        for (path, old_mtime) in &old_manifest.files {
            match current_files.get(path) {
                None => to_delete.push(path.clone()),
                Some(new_mtime) if new_mtime != old_mtime => {
                    to_delete.push(path.clone());
                    to_add_paths.push(path.clone());
                }
                _ => {}
            }
        }
        for path in current_files.keys() {
            if !old_manifest.files.contains_key(path) {
                to_add_paths.push(path.clone());
            }
        }

        let (schema, fields) = Self::create_schema();
        let dir = tantivy::directory::MmapDirectory::open(index_dir)?;
        let index = Index::open_or_create(dir, schema)?;
        register_tokenizer(&index);

        let mut writer: IndexWriter = index.writer(500_000_000)?;
        install_merge_policy(&writer);

        for path in &to_delete {
            writer.delete_term(Term::from_field_text(fields.session_path, path));
        }

        let session_map: HashMap<String, (usize, usize)> = daily_groups
            .iter()
            .enumerate()
            .flat_map(|(day_idx, group)| {
                group
                    .sessions
                    .iter()
                    .filter(|s| !s.is_subagent)
                    .enumerate()
                    .map(move |(session_idx, s)| {
                        (
                            s.file_path.to_string_lossy().to_string(),
                            (day_idx, session_idx),
                        )
                    })
            })
            .collect();

        let add_tasks: Vec<(usize, usize, PathBuf)> = to_add_paths
            .iter()
            .filter_map(|path| {
                let (day_idx, session_idx) = session_map.get(path)?;
                Some((*day_idx, *session_idx, PathBuf::from(path)))
            })
            .collect();

        index_in_chunks(&writer, &fields, &add_tasks)?;
        writer.commit()?;
        // Same merge-threads barrier as `build` — see the comment there.
        // This is the hot path (called every 30s on reload) so the win
        // compounds: each reload finishes its merges before the next one
        // starts, preventing the 339-segment accumulation we hit before.
        writer.wait_merging_threads()?;

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let query_parser = QueryParser::for_index(&index, vec![fields.text]);

        Self::save_manifest(daily_groups, index_dir)?;

        Ok(Self {
            reader,
            fields,
            query_parser,
        })
    }

    pub fn search(&self, query_str: &str, limit: usize, snippet_len: usize) -> Vec<SearchResult> {
        self.search_between(query_str, limit, snippet_len, None, None)
    }

    /// Matches whose message was written between the inclusive local dates.
    /// The bound sits inside the query, so `limit` counts in-range messages and
    /// a session's out-of-range hits can't crowd out its in-range one.
    pub fn search_between(
        &self,
        query_str: &str,
        limit: usize,
        snippet_len: usize,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
    ) -> Vec<SearchResult> {
        let text_query = match self.query_parser.parse_query(query_str) {
            Ok(q) => q,
            Err(_) => {
                let escaped = regex::escape(query_str);
                match self.query_parser.parse_query(&format!("/.*{escaped}.*/")) {
                    Ok(q) => q,
                    Err(_) => return Vec::new(),
                }
            }
        };
        let query: Box<dyn Query> = if from.is_none() && to.is_none() {
            text_query
        } else {
            let (lower, upper) = local_day_bounds(from, to);
            let term = |ts| Term::from_field_i64(self.fields.msg_ts, ts);
            let range = RangeQuery::new(lower.map(term), upper.map(term));
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, text_query),
                (Occur::Must, Box::new(range)),
            ]))
        };

        let searcher = self.reader.searcher();
        // tantivy 0.26 occasionally panics inside `phrase_scorer` when the
        // query is mutated rapidly (e.g., a user editing the search box). The
        // panic kills the TUI, so we catch it here and treat it as "no
        // results" — the next keystroke will issue a fresh query.
        let search_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())
        }));
        let Ok(Ok(top_docs)) = search_result else {
            return Vec::new();
        };

        // Keyed by path: a document keeps the position it was indexed at, and
        // an incremental update leaves unchanged files at stale positions, so
        // two sessions can share one.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut results = Vec::new();

        for (_score, doc_address) in top_docs {
            let Ok(doc) = searcher.doc::<TantivyDocument>(doc_address) else {
                continue;
            };

            let day_idx = get_u64(&doc, self.fields.day_idx) as usize;
            let session_idx = get_u64(&doc, self.fields.session_idx) as usize;
            let session_path = get_str(&doc, self.fields.session_path);

            if !seen.insert(session_path.clone()) {
                continue;
            }

            let role = get_str(&doc, self.fields.role);
            let text = get_str(&doc, self.fields.text);
            let message_date = get_i64(&doc, self.fields.msg_ts)
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                .map(|t| t.with_timezone(&Local).date_naive());
            let snippet = extract_snippet(&text, query_str, snippet_len);

            results.push(SearchResult {
                day_idx,
                session_idx,
                snippet: Some(format!("[{role}] {snippet}")),
                match_type: SearchMatchType::Content,
                // The (day_idx, session_idx) pair was captured at index build
                // time. After a project/period filter shrinks the live
                // daily_groups, the caller must remap using this path before
                // pushing the result into `state.search_results`.
                session_path: Some(session_path),
                message_date,
            });
        }

        results
    }

    pub fn clear_index() -> anyhow::Result<()> {
        let index_dir = Self::index_path()?;
        if index_dir.exists() {
            fs::remove_dir_all(&index_dir)?;
        }
        Ok(())
    }

    fn create_schema() -> (Schema, Fields) {
        let mut schema_builder = Schema::builder();
        let session_path = schema_builder.add_text_field("session_path", STRING | STORED);
        let day_idx = schema_builder.add_u64_field("day_idx", STORED);
        let session_idx = schema_builder.add_u64_field("session_idx", STORED);
        let role = schema_builder.add_text_field("role", STRING | STORED);

        let text_indexing = TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let text_options = TextOptions::default()
            .set_indexing_options(text_indexing)
            .set_stored();
        let text = schema_builder.add_text_field("text", text_options);
        let msg_ts = schema_builder.add_i64_field("msg_ts", INDEXED | STORED | FAST);

        let schema = schema_builder.build();
        let fields = Fields {
            session_path,
            day_idx,
            session_idx,
            role,
            text,
            msg_ts,
        };
        (schema, fields)
    }

    fn index_path() -> anyhow::Result<PathBuf> {
        super::index_dir()
    }

    fn save_manifest(daily_groups: &[DailyGroup], index_dir: &Path) -> anyhow::Result<()> {
        let manifest = Manifest {
            version: INDEX_VERSION,
            files: build_file_map(daily_groups),
        };
        let manifest_path = index_dir.join("manifest.json");
        let json = serde_json::to_string(&manifest)?;
        fs::write(manifest_path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_map(entries: &[(&str, u64)]) -> HashMap<String, u64> {
        entries
            .iter()
            .map(|(p, m)| ((*p).to_string(), *m))
            .collect()
    }

    #[test]
    fn a_moved_mtime_or_a_new_session_counts_as_behind() {
        let old = file_map(&[("a.jsonl", 10), ("b.jsonl", 20)]);
        let moved = file_map(&[("a.jsonl", 10), ("b.jsonl", 99)]);
        assert_eq!(changed_file_count(&old, &moved), 1);
        let added = file_map(&[("a.jsonl", 10), ("b.jsonl", 20), ("c.jsonl", 5)]);
        assert_eq!(changed_file_count(&old, &added), 1);
        // A removed session has nothing left to catch up on.
        let removed = file_map(&[("a.jsonl", 10)]);
        assert_eq!(changed_file_count(&old, &removed), 0);
    }

    #[test]
    fn a_multi_day_session_is_indexed_once_at_its_newest_day() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let newer = chrono::Local::now().date_naive();
        let older = newer - chrono::Duration::days(1);
        let at = |p: &str| {
            let mut s = make_session("proj", None, None);
            s.file_path = PathBuf::from(p);
            s
        };
        let groups = vec![
            make_daily_group(newer, vec![at("/s/multi")]),
            make_daily_group(older, vec![at("/s/old"), at("/s/multi")]),
        ];
        let tasks = collect_tasks(&groups);
        let multi: Vec<_> = tasks
            .iter()
            .filter(|(_, _, p)| p == Path::new("/s/multi"))
            .collect();
        assert_eq!(multi.len(), 1, "indexed once per active day");
        assert_eq!((multi[0].0, multi[0].1), (0, 0), "not at its newest day");
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn a_build_leaves_an_index_alone_while_another_writer_holds_it() {
        let index_dir = std::env::temp_dir().join(format!("ccsight-locked-{}", std::process::id()));
        let _ = fs::remove_dir_all(&index_dir);
        SearchIndex::build_in(&index_dir, &[], &|_| {}, FIRST_COMMIT_SESSIONS).unwrap();

        let (schema, _) = SearchIndex::create_schema();
        let dir = tantivy::directory::MmapDirectory::open(&index_dir).unwrap();
        let held = Index::open_or_create(dir, schema).unwrap();
        let writer: IndexWriter = held.writer(15_000_000).unwrap();

        assert!(
            SearchIndex::build_in(&index_dir, &[], &|_| {}, FIRST_COMMIT_SESSIONS).is_err(),
            "a build must not start under another writer"
        );
        assert!(
            index_dir.join("meta.json").exists(),
            "the held index was deleted"
        );

        drop(writer);
        assert!(SearchIndex::build_in(&index_dir, &[], &|_| {}, FIRST_COMMIT_SESSIONS).is_ok());
        let _ = fs::remove_dir_all(&index_dir);
    }

    #[test]
    fn an_identical_map_is_not_behind() {
        let m = file_map(&[("a.jsonl", 10), ("b.jsonl", 20)]);
        assert_eq!(changed_file_count(&m, &m), 0);
    }

    #[test]
    fn test_index_path() {
        let path = SearchIndex::index_path().unwrap();
        assert!(path.ends_with(std::path::Path::new(".ccsight").join("index")));
    }

    #[test]
    fn test_create_schema() {
        let (schema, _fields) = SearchIndex::create_schema();
        assert!(schema.get_field("text").is_ok());
        assert!(schema.get_field("role").is_ok());
        assert!(schema.get_field("day_idx").is_ok());
        assert!(schema.get_field("session_idx").is_ok());
        assert!(schema.get_field("session_path").is_ok());
    }

    // `build_in`, not `build`: the latter resolves the real user index, and
    // building empty groups there replaces it with an index holding nothing.
    #[test]
    fn test_build_with_empty_groups() {
        let groups: Vec<DailyGroup> = vec![];
        let index_dir = std::env::temp_dir().join(format!("ccsight-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&index_dir);
        let index =
            SearchIndex::build_in(&index_dir, &groups, &|_| {}, FIRST_COMMIT_SESSIONS).unwrap();
        let results = index.search("test", 10, 50);
        assert!(results.is_empty());
        let _ = fs::remove_dir_all(&index_dir);
    }

    // Session sizes span orders of magnitude, so the guard has to be a byte
    // budget: a session-count batch still holds most of the corpus whenever the
    // large transcripts happen to fall in one batch.
    #[test]
    fn byte_budget_chunks_bound_each_batch_and_keep_every_task() {
        let dir = std::env::temp_dir().join(format!("ccsight-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, bytes: usize| -> PathBuf {
            let p = dir.join(name);
            std::fs::write(&p, vec![b'x'; bytes]).unwrap();
            p
        };
        // The first entry is over budget on its own: without the "at least one
        // task per batch" guard that produces an empty leading batch.
        let sizes = [500usize, 100, 100, 100, 500, 100];
        let tasks: Vec<(usize, usize, PathBuf)> = sizes
            .iter()
            .enumerate()
            .map(|(i, b)| (0usize, i, make(&format!("f{i}"), *b)))
            .collect();

        let chunks = byte_budget_chunks(&tasks, 250);
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, tasks.len(), "a task went missing");
        assert!(
            chunks.iter().all(|c| !c.is_empty()),
            "an empty batch would flush nothing"
        );
        for c in &chunks {
            let bytes: u64 = c
                .iter()
                .map(|(_, _, p)| std::fs::metadata(p).map_or(0, |m| m.len()))
                .sum();
            assert!(
                bytes <= 250 || c.len() == 1,
                "only a single over-budget file may exceed the budget"
            );
        }
        assert!(chunks.len() > 1, "the budget has to split this input");

        assert!(
            byte_budget_chunks(&[], 250).is_empty(),
            "no tasks means no batches"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The indexer parses in bounded batches so a cold build does not hold the
    // whole corpus of message text at once. Every session still has to reach
    // the index — a dropped batch would be invisible except as a search miss.
    #[test]
    fn every_session_is_indexed_across_parse_chunk_boundaries() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};

        let dir = std::env::temp_dir().join(format!("ccsight-chunkidx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Enough sessions that the byte budget splits them several times over.
        let count = 37usize;
        let mut sessions = Vec::with_capacity(count);
        for i in 0..count {
            let p = dir.join(format!("s{i}.jsonl"));
            std::fs::write(
                &p,
                format!(
                    r#"{{"type":"user","uuid":"u{i}","timestamp":"2026-01-10T10:00:00Z","sessionId":"s{i}","message":{{"role":"user","content":"needle{i} shared"}}}}"#
                ),
            )
            .unwrap();
            let mut sess = make_session("p", None, None);
            sess.file_path = p;
            sessions.push(sess);
        }
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(); // lint-ok: date-literal
        let groups = vec![make_daily_group(date, sessions)];

        let index_dir = dir.join("index");
        let index =
            SearchIndex::build_in(&index_dir, &groups, &|_| {}, FIRST_COMMIT_SESSIONS).unwrap();

        assert_eq!(
            index.search("shared", count + 10, 50).len(),
            count,
            "a parse batch went missing between chunks"
        );
        for i in [0, count / 2, count - 1] {
            assert_eq!(
                index.search(&format!("needle{i}"), 10, 50).len(),
                1,
                "session {i} is not searchable"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_sessions_sharing_a_stale_position_both_come_back() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let dir = std::env::temp_dir().join(format!("ccsight-stalepos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = |name: &str| {
            let p = dir.join(format!("{name}.jsonl"));
            std::fs::write(
                &p,
                format!(
                    r#"{{"type":"user","uuid":"{name}","timestamp":"2026-01-10T10:00:00Z","message":{{"role":"user","content":"needle"}}}}"#
                ),
            )
            .unwrap();
            let mut s = make_session("p", None, None);
            s.file_path = p;
            s
        };
        let day = |n: u32| NaiveDate::from_ymd_opt(2026, 1, n).unwrap(); // lint-ok: date-literal
        let index_dir = dir.join("index");
        let a = session("a");
        // `a` is indexed at (0, 0); a newer day then takes position 0 and its
        // new session `b` is indexed there too, while unchanged `a` keeps it.
        drop(
            SearchIndex::build_in(
                &index_dir,
                &[make_daily_group(day(10), vec![a.clone()])],
                &|_| {},
                FIRST_COMMIT_SESSIONS,
            )
            .unwrap(),
        );
        let later = vec![
            make_daily_group(day(11), vec![session("b")]),
            make_daily_group(day(10), vec![a]),
        ];
        let manifest: Manifest = serde_json::from_str(
            &std::fs::read_to_string(index_dir.join("manifest.json")).unwrap(),
        )
        .unwrap();
        let index =
            SearchIndex::update_incremental(&later, &index_dir, &manifest, &build_file_map(&later))
                .unwrap();
        assert_eq!(index.search("needle", 10, 50).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_date_bound_keeps_only_messages_written_inside_it() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let dir = std::env::temp_dir().join(format!("ccsight-msgdate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let day = |n: u32| NaiveDate::from_ymd_opt(2026, 1, n).unwrap(); // lint-ok: date-literal
        let line = |uuid: &str, d: NaiveDate| {
            let at = d
                .and_hms_opt(12, 0, 0)
                .and_then(|t| t.and_local_timezone(Local).earliest())
                .unwrap()
                .with_timezone(&chrono::Utc)
                .to_rfc3339();
            format!(
                r#"{{"type":"user","uuid":"{uuid}","timestamp":"{at}","message":{{"role":"user","content":"needle"}}}}"#
            )
        };
        let write = |name: &str, lines: &[String]| {
            let p = dir.join(name);
            std::fs::write(&p, lines.join("\n")).unwrap();
            let mut s = make_session("p", None, None);
            s.file_path = p;
            s
        };
        // `a` matches on days 1 and 3 (twice on 3); `b` only on day 2.
        let a = write(
            "a.jsonl",
            &[line("a1", day(1)), line("a3", day(3)), line("a3b", day(3))],
        );
        let b = write("b.jsonl", &[line("b2", day(2))]);
        let groups = vec![
            make_daily_group(day(3), vec![a.clone()]),
            make_daily_group(day(2), vec![b.clone()]),
            make_daily_group(day(1), vec![a.clone()]),
        ];
        let index =
            SearchIndex::build_in(&dir.join("index"), &groups, &|_| {}, FIRST_COMMIT_SESSIONS)
                .unwrap();
        let hits = |from, to, limit| {
            index
                .search_between("needle", limit, 50, from, to)
                .into_iter()
                .map(|r| (r.session_path.unwrap_or_default(), r.message_date))
                .collect::<Vec<_>>()
        };
        let path = |s: &crate::aggregator::SessionInfo| s.file_path.to_string_lossy().into_owned();

        assert_eq!(
            hits(Some(day(1)), Some(day(1)), 10),
            vec![(path(&a), Some(day(1)))]
        );
        // One slot, and `a` holds more matches — the bound must still admit `b`.
        assert_eq!(
            hits(Some(day(2)), Some(day(2)), 1),
            vec![(path(&b), Some(day(2)))]
        );
        assert_eq!(hits(Some(day(3)), None, 10), vec![(path(&a), Some(day(3)))]);
        assert_eq!(hits(None, None, 10).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn early_handle_serves_head_before_tail_is_indexed() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};

        let dir = std::env::temp_dir().join(format!("ccsight-earlyidx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write_session = |name: &str, word: &str| -> PathBuf {
            let p = dir.join(name);
            std::fs::write(
                &p,
                format!(
                    r#"{{"type":"user","uuid":"u-{word}","timestamp":"2026-01-10T10:00:00Z","sessionId":"s-{word}","message":{{"role":"user","content":"{word}"}}}}"#
                ),
            )
            .unwrap();
            p
        };
        let head_file = write_session("head.jsonl", "alphaword");
        let tail_file = write_session("tail.jsonl", "betaword");

        let mut head_session = make_session("p", None, None);
        head_session.file_path = head_file;
        let mut tail_session = make_session("p", None, None);
        tail_session.file_path = tail_file;
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(); // lint-ok: date-literal
        let groups = vec![make_daily_group(date, vec![head_session, tail_session])];

        // (head hits, tail hits) as seen by the early snapshot.
        let early_seen: std::sync::Mutex<Option<(usize, usize)>> = std::sync::Mutex::new(None);
        let index_dir = dir.join("index");
        let final_index = SearchIndex::build_in(
            &index_dir,
            &groups,
            &|handle| {
                let head = handle.search("alphaword", 10, 50).len();
                let tail = handle.search("betaword", 10, 50).len();
                *early_seen.lock().unwrap() = Some((head, tail));
            },
            1,
        )
        .unwrap();

        assert_eq!(
            *early_seen.lock().unwrap(),
            Some((1, 0)),
            "early snapshot must contain the head chunk only"
        );
        assert_eq!(final_index.search("alphaword", 10, 50).len(), 1);
        assert_eq!(final_index.search("betaword", 10, 50).len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
