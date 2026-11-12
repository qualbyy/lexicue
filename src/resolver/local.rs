//! Layer 2: local BM25 + edit-distance search over shell history and an offline
//! command dictionary (tldr markdown is optional; the index works without it).
//!
//! Context rule: only `cwd` and marker names may influence scoring, capped by
//! `[general].context_bytes` (<= 30). The screen buffer is never read here.

use crate::config::ResolverSettings;
use crate::contracts::{
    Candidate, CandidateSource, CmdId, CommandResolver, CwdContext, ProjectMarker, ResolveError,
    ResolveRequest, SafetyReport, Trigger,
};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

fn cmd_id(text: &str) -> CmdId {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .map(|s| s.trim_matches(|c| c == '-' || c == '_').to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn marker_keywords(marker: ProjectMarker) -> &'static [&'static str] {
    match marker {
        ProjectMarker::Cargo => &["cargo", "rustc", "rust", "crate"],
        ProjectMarker::Node => &["npm", "node", "npx", "yarn", "pnpm"],
        ProjectMarker::Git => &["git"],
        ProjectMarker::Python => &["python", "python3", "pip", "pytest", "uv"],
        ProjectMarker::Go => &["go"],
        ProjectMarker::Make => &["make"],
        ProjectMarker::Docker => &["docker", "compose"],
        ProjectMarker::Unknown => &[],
    }
}

/// One indexed command example (a history line or a tldr snippet).
#[derive(Clone, Debug)]
pub struct Doc {
    pub command: String,
    pub description: String,
    pub source: CandidateSource,
}

pub struct LocalIndex {
    docs: Vec<Doc>,
    /// Document frequency per lowercased token.
    doc_freq: HashMap<String, u32>,
    avg_len: f32,
}

impl LocalIndex {
    /// `dictionary_dirs` may be missing; history alone is a valid index.
    pub fn build(history: &[String], dictionary_dirs: &[PathBuf]) -> Result<Self, LocalError> {
        let mut docs = Vec::new();
        let mut seen_history = HashSet::new();

        for line in history {
            let trimmed = line.trim();
            // Skip multi-line or giant script blobs so they never hijack short queries.
            if !trimmed.is_empty()
                && !trimmed.contains('\n')
                && trimmed.len() <= 240
                && seen_history.insert(trimmed.to_string())
            {
                docs.push(Doc {
                    command: trimmed.to_string(),
                    description: trimmed.to_string(),
                    source: CandidateSource::History,
                });
            }
        }

        for root in dictionary_dirs {
            if !root.exists() {
                continue;
            }
            let candidates = [
                root.clone(),
                root.join("pages").join("common"),
                root.join("pages").join("linux"),
                root.join("common"),
                root.join("linux"),
            ];
            for dir in candidates {
                if dir.is_dir() {
                    collect_tldr_dir(&dir, &mut docs, 0);
                }
            }
        }

        let mut idx = Self {
            docs,
            doc_freq: HashMap::new(),
            avg_len: 0.0,
        };
        idx.recompute_stats();
        Ok(idx)
    }

    fn recompute_stats(&mut self) {
        self.doc_freq.clear();
        if self.docs.is_empty() {
            self.avg_len = 0.0;
            return;
        }
        let mut total_len = 0usize;
        for doc in &self.docs {
            let tokens = tokenize(&format!("{} {}", doc.command, doc.description));
            total_len += tokens.len();
            let unique: HashSet<String> = tokens.into_iter().collect();
            for tok in unique {
                *self.doc_freq.entry(tok).or_insert(0) += 1;
            }
        }
        self.avg_len = (total_len as f32) / (self.docs.len() as f32);
    }

    /// BM25 (`k1=1.2`, `b=0.75`) with a `strsim` tie-break for near-miss tokens.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(usize, f32)> {
        if self.docs.is_empty() || limit == 0 {
            return Vec::new();
        }
        let q_tokens = tokenize(query);
        if q_tokens.is_empty() {
            return Vec::new();
        }

        const K1: f32 = 1.2;
        const B: f32 = 0.75;
        let n_docs = self.docs.len() as f32;
        let avg_dl = self.avg_len.max(1.0);
        let q_lower = query.trim().to_lowercase();

        let mut scored = Vec::new();
        for (doc_idx, doc) in self.docs.iter().enumerate() {
            let d_tokens = tokenize(&format!("{} {}", doc.command, doc.description));
            let dl = d_tokens.len().max(1) as f32;
            let mut tf_map: HashMap<&str, usize> = HashMap::new();
            for tok in &d_tokens {
                *tf_map.entry(tok.as_str()).or_insert(0) += 1;
            }

            let mut score = 0.0f32;
            for qt in &q_tokens {
                if let Some(&tf) = tf_map.get(qt.as_str()) {
                    let df = *self.doc_freq.get(qt).unwrap_or(&0) as f32;
                    let idf = ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.1);
                    let tf_f = tf as f32;
                    let denom = tf_f + K1 * (1.0 - B + B * (dl / avg_dl));
                    score += idf * (tf_f * (K1 + 1.0)) / denom;
                } else if qt.len() >= 2 && d_tokens.iter().any(|dt| dt.starts_with(qt.as_str())) {
                    score += 0.65;
                } else {
                    let best_sim = d_tokens
                        .iter()
                        .map(|dt| strsim::jaro_winkler(qt, dt) as f32)
                        .fold(0.0f32, f32::max);
                    if best_sim >= 0.84 {
                        score += best_sim * 0.45;
                    }
                }
            }

            if score > 0.0 {
                let cmd_lower = doc.command.to_lowercase();
                if cmd_lower.starts_with(&q_lower) {
                    score += 6.0;
                }
                let cmd_tokens = tokenize(&doc.command);
                if cmd_tokens.first() == q_tokens.first() {
                    score += 3.0;
                }
                let cmd_sim = strsim::jaro_winkler(&q_lower, &cmd_lower) as f32;
                score += cmd_sim * 0.05;
                scored.push((doc_idx, score));
            }
        }

        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(limit);
        scored
    }

    pub fn add_history(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.contains('\n') || trimmed.len() > 240 {
            return;
        }
        if self
            .docs
            .iter()
            .any(|d| d.source == CandidateSource::History && d.command == trimmed)
        {
            return;
        }
        self.docs.push(Doc {
            command: trimmed.to_string(),
            description: trimmed.to_string(),
            source: CandidateSource::History,
        });
        self.recompute_stats();
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Seed common Linux/developer command continuations so fresh shells still offer useful hints.
    pub fn add_builtin_docs(&mut self) {
        let existing: HashSet<String> = self.docs.iter().map(|d| d.command.clone()).collect();
        let mut added = false;
        for &(command, description) in BUILTIN_DOCS {
            if !existing.contains(command) {
                self.docs.push(Doc {
                    command: command.to_string(),
                    description: description.to_string(),
                    source: CandidateSource::Dictionary,
                });
                added = true;
            }
        }
        if added {
            self.recompute_stats();
        }
    }
}

const BUILTIN_DOCS: &[(&str, &str)] = &[
    ("sysctl -a", "show all kernel parameters"),
    ("sysctl -n {{key}}", "read one kernel parameter value"),
    (
        "sysctl -w {{key}}={{value}}",
        "set kernel parameter at runtime",
    ),
    ("sysctl -p", "reload settings from /etc/sysctl.conf"),
    (
        "sysctl --system",
        "load settings from all system sysctl.d dirs",
    ),
    (
        "systemctl status {{service}}",
        "show systemd service status",
    ),
    ("systemctl restart {{service}}", "restart a systemd service"),
    ("systemctl list-units --failed", "list failed systemd units"),
    ("journalctl -xe", "jump to recent system log errors"),
    (
        "journalctl -u {{service}} -f",
        "follow logs for a systemd unit",
    ),
    ("ss -tulpn", "list listening TCP/UDP sockets and processes"),
    ("ip addr", "show network interfaces and addresses"),
    ("ip route", "show kernel routing table"),
    ("git status -sb", "short branch and working tree status"),
    ("git log --oneline -n 15", "compact recent commit history"),
    ("git diff --stat", "summary of changed files"),
    (
        "git switch -c {{branch}}",
        "create and switch to a new branch",
    ),
    ("cargo test", "run Rust test suite"),
    ("cargo clippy --all-targets", "run Rust linter checks"),
    ("docker ps", "list running containers"),
    (
        "docker compose up -d",
        "start compose services in background",
    ),
    (
        "find . -maxdepth 3 -name '{{pattern}}'",
        "search files by name",
    ),
    (
        "du -sh * | sort -rh | head -20",
        "largest items in current dir",
    ),
    ("df -h", "disk space usage by filesystem"),
    ("ps aux --sort=-%mem | head -15", "top processes by memory"),
    ("tar -xzvf {{archive.tar.gz}}", "extract gzip tarball"),
    (
        "tar -czvf {{archive.tar.gz}} {{path}}",
        "create gzip tarball",
    ),
    (
        "rsync -avh --progress {{src}} {{dst}}",
        "sync files with progress",
    ),
    (
        "curl -fsSL {{url}}",
        "fetch URL silently following redirects",
    ),
    ("chmod +x {{file}}", "make file executable"),
];

fn collect_tldr_dir(dir: &Path, docs: &mut Vec<Doc>, depth: usize) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                parse_tldr_markdown(&content, docs);
            }
        } else if path.is_dir() && depth == 0 {
            collect_tldr_dir(&path, docs, depth + 1);
        }
    }
}

fn parse_tldr_markdown(content: &str, docs: &mut Vec<Doc>) {
    let mut page_title = String::new();
    let mut last_desc = String::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if let Some(title) = line.strip_prefix("# ") {
            page_title = title.trim().to_string();
        } else if let Some(desc) = line.strip_prefix("- ") {
            last_desc = desc.trim().trim_end_matches(':').to_string();
        } else if line.starts_with('`') && line.ends_with('`') && line.len() > 2 {
            let cmd = line.trim_matches('`').trim();
            if !cmd.is_empty() {
                let description = if last_desc.is_empty() {
                    page_title.clone()
                } else if page_title.is_empty() {
                    last_desc.clone()
                } else {
                    format!("{page_title}: {last_desc}")
                };
                docs.push(Doc {
                    command: cmd.to_string(),
                    description,
                    source: CandidateSource::Dictionary,
                });
            }
        }
    }
}

pub struct LocalResolver {
    settings: ResolverSettings,
    index: LocalIndex,
}

impl LocalResolver {
    pub fn new(settings: ResolverSettings) -> Result<Self, LocalError> {
        let history = load_shell_history(200);
        let mut index = LocalIndex::build(&history, &settings.dictionary_dirs)?;
        index.add_builtin_docs();
        Ok(Self { settings, index })
    }

    /// Called when history grows or an rc/dictionary file changes.
    #[allow(dead_code)] // Shortcut: реиндекс по истории пока не вызывается.
    pub fn rebuild(&mut self, history: &[String]) -> Result<(), LocalError> {
        self.index = LocalIndex::build(history, &self.settings.dictionary_dirs)?;
        Ok(())
    }

    /// +`history_bonus` when a marker token appears in the command or description.
    pub fn context_score(&self, doc: &Doc, ctx: &CwdContext) -> f32 {
        if ctx.markers.is_empty() || self.settings.history_bonus == 0.0 {
            return 0.0;
        }
        let haystack = format!("{} {}", doc.command, doc.description).to_lowercase();
        let hay_tokens: Vec<String> = tokenize(&haystack);
        for marker in &ctx.markers {
            for kw in marker_keywords(*marker) {
                if hay_tokens.iter().any(|t| t == kw) || haystack.contains(kw) {
                    return self.settings.history_bonus;
                }
            }
        }
        0.0
    }
}

impl CommandResolver for LocalResolver {
    fn id(&self) -> &'static str {
        "local"
    }

    fn priority(&self) -> u8 {
        20
    }

    fn resolve(&mut self, req: &ResolveRequest) -> Result<Vec<Candidate>, ResolveError> {
        for line in &req.history_tail {
            self.index.add_history(line);
        }
        let query = req
            .raw_input
            .trim()
            .strip_prefix("??")
            .unwrap_or(req.raw_input.trim())
            .trim();
        if query.is_empty() || (req.trigger == Trigger::Explicit && !query.is_ascii()) {
            return Ok(Vec::new());
        }

        let limit = req.limit.max(1).min(self.settings.max_candidates.max(1));
        let hits = self.index.search(query, self.index.len().max(limit));
        let mut candidates: Vec<Candidate> = hits
            .into_iter()
            .filter_map(|(doc_idx, bm25)| {
                let doc = self.index.docs.get(doc_idx)?;
                let bonus = self.context_score(doc, &req.context)
                    + if doc.source == CandidateSource::History {
                        0.3
                    } else {
                        0.0
                    };
                let note = if doc.description.is_empty() || doc.description == doc.command {
                    None
                } else {
                    Some(doc.description.clone())
                };
                Some(Candidate {
                    id: cmd_id(&doc.command),
                    template: doc.command.clone(),
                    rendered: doc.command.clone(),
                    source: doc.source,
                    score: bm25 + bonus,
                    safety: SafetyReport::default(),
                    note,
                })
            })
            .collect();

        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(limit);
        Ok(candidates)
    }

    fn resolve_live(&mut self, req: &ResolveRequest) -> Vec<Candidate> {
        for line in &req.history_tail {
            self.index.add_history(line);
        }
        let cleaned = req
            .raw_input
            .trim_start()
            .strip_prefix("??")
            .unwrap_or(req.raw_input.trim_start());
        let trimmed = cleaned.trim();
        if trimmed.len() < 2 || !trimmed.is_ascii() || req.raw_input.trim_start().starts_with("??")
        {
            return Vec::new();
        }

        let trailing_space = cleaned.ends_with(char::is_whitespace);
        let q_lower = trimmed.to_lowercase();
        let q_parts: Vec<&str> = trimmed.split_whitespace().collect();
        let has_space = q_parts.len() > 1 || trailing_space;
        let q_cmd = q_parts[0].to_lowercase();
        let limit = req.limit.max(1).min(self.settings.max_candidates.max(1));

        let mut candidates = Vec::new();
        for doc in &self.index.docs {
            let doc_lower = doc.command.to_lowercase();
            let doc_parts: Vec<&str> = doc.command.split_whitespace().collect();
            if doc_parts.is_empty() {
                continue;
            }
            let doc_cmd = doc_parts[0].to_lowercase();

            let mut score = if has_space {
                // Once `$0` is separated by a space, never suggest unrelated commands.
                if doc_cmd != q_cmd {
                    continue;
                }
                if doc_lower.starts_with(&q_lower) && doc.command.len() > trimmed.len() {
                    10.0
                } else {
                    let completed_count = if trailing_space {
                        q_parts.len()
                    } else {
                        q_parts.len().saturating_sub(1)
                    };
                    if doc_parts.len() <= completed_count {
                        continue;
                    }
                    let prefix_ok =
                        (1..completed_count).all(|i| doc_parts[i].eq_ignore_ascii_case(q_parts[i]));
                    if !prefix_ok {
                        continue;
                    }
                    if trailing_space {
                        7.5
                    } else {
                        let typed_last = q_parts[q_parts.len() - 1].to_lowercase();
                        let cand_tok = doc_parts[q_parts.len() - 1];
                        // Don't offer a raw `{{slot}}` template over an argument the user already typed.
                        if cand_tok.starts_with("{{")
                            || !cand_tok.to_lowercase().starts_with(&typed_last)
                        {
                            continue;
                        }
                        8.0
                    }
                }
            } else if doc_lower.starts_with(&q_lower) {
                8.5
            } else if q_lower.len() >= 3 && doc_cmd.starts_with(&q_cmd) {
                7.0
            } else if q_lower.len() >= 3 && strsim::jaro_winkler(&doc_cmd, &q_cmd) >= 0.86 {
                4.5
            } else {
                continue;
            };

            if doc.source == CandidateSource::History {
                score += 2.0;
            }
            score += self.context_score(doc, &req.context);

            let note = if doc.description.is_empty() || doc.description == doc.command {
                None
            } else {
                Some(doc.description.clone())
            };
            candidates.push(Candidate {
                id: cmd_id(&doc.command),
                template: doc.command.clone(),
                rendered: doc.command.clone(),
                source: doc.source,
                score,
                safety: SafetyReport::default(),
                note,
            });
        }

        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.rendered.len().cmp(&b.rendered.len()))
        });
        candidates.truncate(limit);
        candidates
    }
}

fn load_shell_history(limit: usize) -> Vec<String> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for name in [".zsh_history", ".bash_history"] {
        let path = home.join(name);
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines().rev().take(limit) {
            let clean = if let Some(rest) = line.strip_prefix(": ") {
                rest.split_once(';').map(|(_, cmd)| cmd).unwrap_or(line)
            } else {
                line
            };
            let trimmed = clean.trim();
            if !trimmed.is_empty() {
                lines.push(trimmed.to_string());
            }
        }
    }
    lines.reverse();
    lines
}

#[derive(Debug, thiserror::Error)]
// Shortcut: ошибки индекса не всплывают — локальный резолвер всегда что-то отдаёт.
#[allow(dead_code)]
pub enum LocalError {
    #[error("index io error: {0}")]
    Io(String),
    #[error("dictionary parse error: {0}")]
    Parse(String),
    #[error("index is empty")]
    EmptyIndex,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Trigger;
    use tempfile::tempdir;

    fn test_settings(dict_dirs: Vec<PathBuf>) -> ResolverSettings {
        ResolverSettings {
            aliases_rc: Vec::new(),
            user_aliases: PathBuf::from("/tmp/nonexistent_aliases.toml"),
            dictionary_dirs: dict_dirs,
            context_markers: vec!["Cargo.toml".into()],
            max_candidates: 8,
            history_bonus: 1.5,
        }
    }

    #[test]
    fn searches_history_and_optional_tldr_with_bm25_and_fuzzy() {
        let dir = tempdir().unwrap();
        let common = dir.path().join("pages").join("common");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::write(
            common.join("du.md"),
            "# du\n\n- Show disk usage of files sorted by size:\n`du -ah . | sort -rh | head -20`\n",
        )
        .unwrap();

        let history = vec![
            "git status -sb".to_string(),
            "cargo test --workspace".to_string(),
        ];
        let mut resolver =
            LocalResolver::new(test_settings(vec![dir.path().to_path_buf()])).unwrap();
        resolver.rebuild(&history).unwrap();
        assert_eq!(resolver.index.len(), 3);

        // Fuzzy typo "gti stat" should still find git status.
        let req = ResolveRequest {
            raw_input: "gti stat".into(),
            context: CwdContext::default(),
            history_tail: Vec::new(),
            trigger: Trigger::AfterMiss,
            limit: 8,
        };
        let hits = resolver.resolve(&req).unwrap();
        assert!(hits.iter().any(|c| c.rendered == "git status -sb"));

        // Description query from tldr dictionary.
        let req2 = ResolveRequest {
            raw_input: "disk usage sorted size".into(),
            context: CwdContext::default(),
            history_tail: Vec::new(),
            trigger: Trigger::AfterMiss,
            limit: 8,
        };
        let hits2 = resolver.resolve(&req2).unwrap();
        assert_eq!(hits2[0].source, CandidateSource::Dictionary);
        assert!(hits2[0].rendered.starts_with("du -ah"));
    }

    #[test]
    fn context_markers_add_history_bonus_and_cap_results() {
        let mut resolver = LocalResolver::new(test_settings(Vec::new())).unwrap();
        let history: Vec<String> = (0..12)
            .map(|i| format!("echo test line {i}"))
            .chain(std::iter::once("cargo test".into()))
            .collect();
        resolver.rebuild(&history).unwrap();

        let doc_cargo = Doc {
            command: "cargo test".into(),
            description: "run rust tests".into(),
            source: CandidateSource::History,
        };
        let doc_other = Doc {
            command: "pytest".into(),
            description: "run python tests".into(),
            source: CandidateSource::History,
        };
        let cargo_ctx = CwdContext {
            markers: vec![ProjectMarker::Cargo],
            ..Default::default()
        };
        assert_eq!(resolver.context_score(&doc_cargo, &cargo_ctx), 1.5);
        assert_eq!(resolver.context_score(&doc_other, &cargo_ctx), 0.0);

        let req = ResolveRequest {
            raw_input: "test".into(),
            context: cargo_ctx,
            history_tail: Vec::new(),
            trigger: Trigger::AfterMiss,
            limit: 4,
        };
        let hits = resolver.resolve(&req).unwrap();
        assert!(hits.len() <= 4);
        assert_eq!(hits[0].rendered, "cargo test");
    }

    #[test]
    fn builtin_continuations_suggest_sysctl_and_prefix_matches() {
        let mut resolver = LocalResolver::new(test_settings(Vec::new())).unwrap();
        let req = ResolveRequest {
            raw_input: "sysctl".into(),
            context: CwdContext::default(),
            history_tail: Vec::new(),
            trigger: Trigger::AfterMiss,
            limit: 6,
        };
        let hits = resolver.resolve(&req).unwrap();
        assert!(hits.iter().any(|c| c.rendered == "sysctl -a"));
        assert!(hits.iter().any(|c| c.rendered.starts_with("sysctl -n")));

        let prefix_req = ResolveRequest {
            raw_input: "sysc".into(),
            context: CwdContext::default(),
            history_tail: Vec::new(),
            trigger: Trigger::AfterMiss,
            limit: 6,
        };
        let prefix_hits = resolver.resolve(&prefix_req).unwrap();
        assert!(prefix_hits
            .iter()
            .any(|c| c.rendered.starts_with("sysctl ")));

        // Live completion after space (`systemctl restart incy`) must never surface unrelated `$0` commands.
        resolver.index.add_history("killall -9 incy");
        resolver.index.add_history("sudo incy");
        let live_req = ResolveRequest {
            raw_input: "systemctl restart incy".into(),
            context: CwdContext::default(),
            history_tail: Vec::new(),
            trigger: Trigger::AfterMiss,
            limit: 8,
        };
        let live_hits = resolver.resolve_live(&live_req);
        assert!(!live_hits.iter().any(|c| c.rendered.contains("killall")));
        assert!(!live_hits.iter().any(|c| c.rendered.contains("sudo")));
        assert!(!live_hits.iter().any(|c| c.rendered.contains("{{service}}")));
    }

    #[test]
    fn cyrillic_prose_and_multiline_history_do_not_hijack_local_hints() {
        let mut resolver = LocalResolver::new(test_settings(Vec::new())).unwrap();
        let big_python = "python3 -c \"print('html')\nprint('done')\"";
        resolver.index.add_history(big_python);
        resolver
            .index
            .add_history("mv ~/Documents/*.html ~/Downloads/");

        let nl_live = ResolveRequest {
            raw_input: "переместить любой html файл из папки документы".into(),
            context: CwdContext::default(),
            history_tail: vec![big_python.into()],
            trigger: Trigger::AfterMiss,
            limit: 6,
        };
        assert!(resolver.resolve_live(&nl_live).is_empty());

        let nl_explicit = ResolveRequest {
            trigger: Trigger::Explicit,
            ..nl_live
        };
        assert!(resolver.resolve(&nl_explicit).unwrap().is_empty());
    }
}
