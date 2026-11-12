//! Persistent resolver cache: `$XDG_CACHE_HOME/termby/cache.jsonl`, one JSON
//! object per line, append-only until `prune`.
//!
//! A hit is a pure `$0` resolution: no network, no LLM. Keys ignore the exact cwd
//! and use the project-marker class so the same intent in the same kind of repo
//! reuses the entry.
//!
//! Line schema (also documented in ARCHITECTURE.md):
//! `{"key","query","template","source","provider","model","created_at","hits","risk"}`

use crate::contracts::{
    Candidate, CandidateSource, CmdId, CwdContext, ProjectMarker, RiskLevel, SafetyReport,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CacheEntry {
    /// `sha256(normalized_query | marker_class | provider:model)`.
    pub key: String,
    /// Normalized query text, for debugging and prune reports only.
    pub query: String,
    pub template: String,
    pub source: CandidateSource,
    pub provider: String,
    pub model: String,
    /// Unix seconds.
    pub created_at: i64,
    pub hits: u32,
    pub risk: RiskLevel,
}

impl CacheEntry {
    /// Rehydrate as an overlay row; source is always `CandidateSource::Cache`.
    pub fn to_candidate(&self, id: CmdId) -> Candidate {
        Candidate {
            id,
            template: self.template.clone(),
            rendered: self.template.clone(),
            source: CandidateSource::Cache,
            score: 0.95,
            safety: SafetyReport {
                level: self.risk,
                reasons: Vec::new(),
            },
            note: Some(format!("cached ({}/{})", self.provider, self.model)),
        }
    }
}

pub struct ResolveCache {
    path: PathBuf,
    entries: HashMap<String, CacheEntry>,
    dirty: bool,
    provider: String,
    model: String,
    instructions_tag: String,
}

fn normalize_query(raw_input: &str) -> String {
    let trimmed = raw_input.trim();
    let stripped = trimmed.strip_prefix("??").unwrap_or(trimmed).trim();
    stripped
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn marker_name(m: ProjectMarker) -> &'static str {
    match m {
        ProjectMarker::Cargo => "cargo",
        ProjectMarker::Node => "node",
        ProjectMarker::Git => "git",
        ProjectMarker::Python => "python",
        ProjectMarker::Go => "go",
        ProjectMarker::Make => "make",
        ProjectMarker::Docker => "docker",
        ProjectMarker::Unknown => "unknown",
    }
}

fn marker_class(ctx: &CwdContext) -> String {
    if ctx.markers.is_empty() {
        return "<none>".to_string();
    }
    let mut names: Vec<&'static str> = ctx.markers.iter().copied().map(marker_name).collect();
    names.sort_unstable();
    names.dedup();
    names.join(",")
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl ResolveCache {
    /// Legacy cache path ($XDG_CACHE_HOME/termby/cache.jsonl).
    pub fn legacy_cache_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("termby").join("cache.jsonl");
            }
        }
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("~/.cache"))
            .join("termby")
            .join("cache.jsonl")
    }

    /// Open target cache with one-time migration from legacy path if new file is absent.
    pub fn open_with_migration(new_path: &Path, legacy_path: &Path) -> Result<Self, CacheError> {
        if !new_path.exists() && legacy_path.exists() {
            let mut legacy_cache = Self::open(legacy_path)?;
            legacy_cache.path = new_path.to_path_buf();
            legacy_cache.dirty = true;
            legacy_cache.persist()?;
            return Ok(legacy_cache);
        }
        Self::open(new_path)
    }

    /// Corrupt lines are skipped and logged; a missing file is an empty cache.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        let mut entries = HashMap::new();
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .map_err(|e| CacheError::Io(format!("{}: {e}", path.display())))?;
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<CacheEntry>(trimmed) {
                    Ok(entry) => {
                        entries.insert(entry.key.clone(), entry);
                    }
                    Err(e) => {
                        eprintln!(
                            "lexicue cache: skipping corrupt line {} in {}: {e}",
                            idx + 1,
                            path.display()
                        );
                    }
                }
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            entries,
            dirty: false,
            provider: String::new(),
            model: String::new(),
            instructions_tag: String::new(),
        })
    }

    pub fn set_provider_model(&mut self, provider: &str, model: &str) {
        self.provider = provider.to_string();
        self.model = model.to_string();
    }

    pub fn set_instructions_tag(&mut self, tag: &str) {
        self.instructions_tag = tag.to_string();
    }

    pub fn clear(&mut self) -> Result<usize, CacheError> {
        let count = self.entries.len();
        self.entries.clear();
        self.dirty = true;
        self.persist()?;
        Ok(count)
    }

    /// Increments `hits` in memory (`dirty`), returns the entry without touching disk.
    pub fn lookup(&mut self, raw_input: &str, ctx: &CwdContext) -> Option<CacheEntry> {
        let key = Self::normalize_key_tagged(
            raw_input,
            ctx,
            &self.provider,
            &self.model,
            &self.instructions_tag,
        );
        let entry = self.entries.get_mut(&key)?;
        entry.hits = entry.hits.saturating_add(1);
        entry.created_at = now_unix_secs();
        self.dirty = true;
        Some(entry.clone())
    }

    /// Upsert by normalized key. `persist` is called by the caller after a successful run.
    pub fn put(
        &mut self,
        raw_input: &str,
        ctx: &CwdContext,
        candidate: &Candidate,
        provider: &str,
        model: &str,
    ) -> Result<(), CacheError> {
        if self.provider.is_empty() && self.model.is_empty() {
            self.set_provider_model(provider, model);
        }
        let key =
            Self::normalize_key_tagged(raw_input, ctx, provider, model, &self.instructions_tag);
        let entry = CacheEntry {
            key: key.clone(),
            query: normalize_query(raw_input),
            template: candidate.template.clone(),
            source: candidate.source,
            provider: provider.to_string(),
            model: model.to_string(),
            created_at: now_unix_secs(),
            hits: 1,
            risk: candidate.safety.level,
        };
        self.entries.insert(key, entry);
        self.dirty = true;
        Ok(())
    }

    /// Owner-only, exclusive create: a prepared symlink cannot redirect the write.
    fn open_new_private(path: &std::path::Path) -> std::io::Result<std::fs::File> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(path)
    }

    /// Rewrite the JSONL file atomically (temp file + rename).
    pub fn persist(&mut self) -> Result<(), CacheError> {
        if !self.dirty && self.path.exists() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CacheError::Io(format!("{}: {e}", parent.display())))?;
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let tmp_name = format!(
            "{}.tmp.{}.{:08x}",
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("cache.jsonl"),
            std::process::id(),
            nonce
        );
        let tmp_path = self.path.with_file_name(tmp_name);

        let mut buf = String::new();
        let mut sorted: Vec<&CacheEntry> = self.entries.values().collect();
        sorted.sort_by(|a, b| a.key.cmp(&b.key));
        for entry in sorted {
            let line =
                serde_json::to_string(entry).map_err(|e| CacheError::Parse(e.to_string()))?;
            buf.push_str(&line);
            buf.push('\n');
        }
        {
            use std::io::Write;
            let mut file = Self::open_new_private(&tmp_path)
                .map_err(|e| CacheError::Io(format!("{}: {e}", tmp_path.display())))?;
            if let Err(e) = file.write_all(buf.as_bytes()) {
                drop(file);
                let _ = std::fs::remove_file(&tmp_path);
                return Err(CacheError::Io(format!("{}: {e}", tmp_path.display())));
            }
        }
        if let Err(e) = std::fs::rename(&tmp_path, &self.path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(CacheError::Io(format!("{}: {e}", self.path.display())));
        }
        self.dirty = false;
        Ok(())
    }

    /// Keep the `max_entries` most recently used entries.
    pub fn prune(&mut self, max_entries: usize) {
        if self.entries.len() <= max_entries {
            return;
        }
        let mut items: Vec<CacheEntry> = self.entries.values().cloned().collect();
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.hits.cmp(&a.hits))
                .then_with(|| a.key.cmp(&b.key))
        });
        items.truncate(max_entries);
        self.entries = items.into_iter().map(|e| (e.key.clone(), e)).collect();
        self.dirty = true;
    }

    /// Lowercase, collapse whitespace, strip a leading `??`, then SHA-256 over the fields.
    #[allow(dead_code)] // Shortcut: используется только тестами ключей.
    pub fn normalize_key(raw_input: &str, ctx: &CwdContext, provider: &str, model: &str) -> String {
        Self::normalize_key_tagged(raw_input, ctx, provider, model, "")
    }

    pub fn normalize_key_tagged(
        raw_input: &str,
        ctx: &CwdContext,
        provider: &str,
        model: &str,
        instructions_tag: &str,
    ) -> String {
        let q = normalize_query(raw_input);
        let mc = marker_class(ctx);
        let payload = if instructions_tag.is_empty() {
            format!("{q}|{mc}|{provider}:{model}")
        } else {
            format!("{q}|{mc}|{provider}:{model}|{instructions_tag}")
        };
        let digest = Sha256::digest(payload.as_bytes());
        hex::encode(digest)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache io error: {0}")]
    Io(String),
    #[error("cache parse error: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_varies_by_instructions_tag_and_clear_empties() {
        let ctx = CwdContext::default();
        let a = ResolveCache::normalize_key_tagged("list files", &ctx, "openai", "gpt", "");
        let b = ResolveCache::normalize_key_tagged("list files", &ctx, "openai", "gpt", "deadbeef");
        assert_ne!(a, b);

        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache = ResolveCache::open(&dir.path().join("cache.jsonl")).expect("open");
        let cand = Candidate {
            id: 1,
            template: "ls".into(),
            rendered: "ls".into(),
            source: CandidateSource::Llm,
            score: 0.9,
            safety: SafetyReport {
                level: RiskLevel::Safe,
                reasons: Vec::new(),
            },
            note: None,
        };
        cache
            .put("list files", &ctx, &cand, "openai", "gpt")
            .expect("put");
        assert!(cache.lookup("list files", &ctx).is_some());
        assert_eq!(cache.clear().expect("clear"), 1);
        assert!(cache.lookup("list files", &ctx).is_none());
    }
}
