use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, OwnedValue, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{LowerCaser, NgramTokenizer, TextAnalyzer};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term, doc};

use crate::aggregator::DailyGroup;
use crate::domain::{EntryType, Role};
use crate::parser::JsonlParser;
use crate::search::{SearchMatchType, SearchResult, extract_snippet};

const INDEX_VERSION: u32 = 5;
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
}

struct ParsedDoc {
    session_path: String,
    day_idx: u64,
    session_idx: u64,
    role: String,
    text: String,
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
            })
        })
        .collect()
}

fn write_docs(writer: &IndexWriter, fields: &Fields, docs: &[ParsedDoc]) -> anyhow::Result<()> {
    for d in docs {
        writer.add_document(doc!(
            fields.session_path => d.session_path.clone(),
            fields.day_idx => d.day_idx,
            fields.session_idx => d.session_idx,
            fields.role => d.role.clone(),
            fields.text => d.text.clone(),
        ))?;
    }
    Ok(())
}

fn collect_tasks(daily_groups: &[DailyGroup]) -> Vec<(usize, usize, PathBuf)> {
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
        .collect()
}

impl SearchIndex {
    pub fn update_or_build(daily_groups: &[DailyGroup]) -> anyhow::Result<Self> {
        let index_dir = Self::index_path()?;

        if !index_dir.exists() {
            return Self::build(daily_groups);
        }

        let manifest_path = index_dir.join("manifest.json");
        let manifest = match fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|data| serde_json::from_str::<Manifest>(&data).ok())
        {
            Some(m) if m.version == INDEX_VERSION => m,
            _ => return Self::build(daily_groups),
        };

        let current_files = build_file_map(daily_groups);

        // Empty manifest with non-empty data = corrupt (filtered groups
        // were saved at some point); rebuild instead of incrementally
        // re-adding every file every launch.
        if manifest.files.is_empty() && !current_files.is_empty() {
            return Self::build(daily_groups);
        }

        if manifest.files == current_files {
            return Self::open_existing(&index_dir);
        }

        Self::update_incremental(daily_groups, &index_dir, &manifest, &current_files)
    }

    fn build(daily_groups: &[DailyGroup]) -> anyhow::Result<Self> {
        let index_dir = Self::index_path()?;

        if index_dir.exists() {
            let _ = fs::remove_dir_all(&index_dir);
        }
        fs::create_dir_all(&index_dir)?;

        let (schema, fields) = Self::create_schema();
        let dir = tantivy::directory::MmapDirectory::open(&index_dir)?;
        let index = Index::open_or_create(dir, schema)?;
        register_tokenizer(&index);

        let mut writer: IndexWriter = index.writer(500_000_000)?;

        let tasks = collect_tasks(daily_groups);
        let parsed: Vec<ParsedDoc> = tasks
            .par_iter()
            .flat_map(|(day_idx, session_idx, path)| parse_session(path, *day_idx, *session_idx))
            .collect();

        write_docs(&writer, &fields, &parsed)?;
        writer.commit()?;

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let query_parser = QueryParser::for_index(&index, vec![fields.text]);

        Self::save_manifest(daily_groups, &index_dir)?;

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

        let parsed: Vec<ParsedDoc> = add_tasks
            .par_iter()
            .flat_map(|(day_idx, session_idx, path)| parse_session(path, *day_idx, *session_idx))
            .collect();

        write_docs(&writer, &fields, &parsed)?;
        writer.commit()?;

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
        let query = match self.query_parser.parse_query(query_str) {
            Ok(q) => q,
            Err(_) => {
                let escaped = regex::escape(query_str);
                match self.query_parser.parse_query(&format!("/.*{escaped}.*/")) {
                    Ok(q) => q,
                    Err(_) => return Vec::new(),
                }
            }
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

        let mut seen: HashMap<(usize, usize), bool> = HashMap::new();
        let mut results = Vec::new();

        for (_score, doc_address) in top_docs {
            let Ok(doc) = searcher.doc::<TantivyDocument>(doc_address) else {
                continue;
            };

            let day_idx = get_u64(&doc, self.fields.day_idx) as usize;
            let session_idx = get_u64(&doc, self.fields.session_idx) as usize;
            let session_path = get_str(&doc, self.fields.session_path);

            if seen.contains_key(&(day_idx, session_idx)) {
                continue;
            }
            seen.insert((day_idx, session_idx), true);

            let role = get_str(&doc, self.fields.role);
            let text = get_str(&doc, self.fields.text);
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

        let schema = schema_builder.build();
        let fields = Fields {
            session_path,
            day_idx,
            session_idx,
            role,
            text,
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

    #[test]
    fn test_index_path() {
        let path = SearchIndex::index_path().unwrap();
        assert!(path.to_string_lossy().contains(".ccsight/index"));
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

    #[test]
    fn test_build_with_empty_groups() {
        let groups: Vec<DailyGroup> = vec![];
        let index = SearchIndex::build(&groups).unwrap();
        let results = index.search("test", 10, 50);
        assert!(results.is_empty());
    }
}
