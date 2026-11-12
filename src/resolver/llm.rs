//! Layer 3: LLM fallback (Ollama or DeepSeek), strictly opt-in by trigger.
//!
//! Rules:
//! - Runs only when `req.trigger == Explicit`, or on `AfterMiss` if
//!   `[llm].enabled` and no L1/local candidate was produced.
//! - The prompt contains the masked description + cwd/marker context only.
//! - Every parsed candidate is masked, safety-analyzed, and written to the cache
//!   before being returned; a repeated query resolves from cache with zero cost.
//! - Secrets are masked and never unmasked: the model may not reconstruct them.

use crate::config::{LlmProviderKind, LlmSettings};
use crate::contracts::{
    Candidate, CandidateSource, CmdId, CommandResolver, ResolveError, ResolveRequest, SafetyReport,
    Trigger, CONTEXT_BUDGET_BYTES,
};
use crate::input::safety::{self, SafetyPolicy};
use crate::resolver::cache::ResolveCache;
use regex::Regex;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

fn cmd_id(text: &str) -> CmdId {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn provider_name(kind: LlmProviderKind) -> &'static str {
    match kind {
        LlmProviderKind::Ollama => "ollama",
        LlmProviderKind::DeepSeek => "deepseek",
        LlmProviderKind::Gemini => "gemini",
        LlmProviderKind::Anthropic => "anthropic",
        LlmProviderKind::OpenAi => "openai",
    }
}

pub const DEFAULT_SYSTEM_INSTRUCTIONS: &str = "\
1. Every `command` MUST be one complete, self-contained single-line command without newlines; never return a partial variable assignment alone and never split steps of one task across multiple array entries.\n\
2. Always double-quote paths/variables (\"$f\") so spaces and non-ASCII filenames work.\n\
3. Never use unguarded brace globs like *.{jpg,png} because zsh aborts with `no matches found`; use `find <dir> -maxdepth 1 -type f \\( -iname '*.ext1' -o ... \\)` sorted by mtime (`-printf '%T@\\t%p\\n' | sort -nr | head -n1 | cut -f2-` on Linux) or `ls -t` without failing brace globs.\n\
4. For standard Desktop/Downloads/Documents/Pictures directories on Linux, prefer `\"$(xdg-user-dir DOWNLOAD 2>/dev/null || echo ~/Downloads)\"` (likewise DOCUMENTS, PICTURES, DESKTOP) so localized folder names (e.g. ~/Загрузки, ~/Документы) resolve automatically.\n\
5. Prefer clean, portable POSIX/GNU pipelines; check arguments with `--` before filenames where supported.\n\
6. Use {{arg}} only if the user omitted a specific required value.";

fn expand_tilde_path(raw: &str) -> std::path::PathBuf {
    let trimmed = raw.trim();
    if let Some(home) = dirs::home_dir() {
        if trimmed == "~" {
            return home;
        }
        if let Some(rest) = trimmed.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    std::path::PathBuf::from(trimmed)
}

/// Input with secrets replaced by stable `<SECRET_n>` tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskedText {
    pub text: String,
    pub redactions: usize,
}

pub struct LlmResolver {
    settings: LlmSettings,
    cache: ResolveCache,
    agent: ureq::Agent,
    /// Same TLS stack without a proxy. Used as a fallback when a stale/broken
    /// `HTTPS_PROXY` closes the CONNECT/TLS handshake ("unexpected end of file").
    direct_agent: ureq::Agent,
    proxy_active: bool,
    max_cache_entries: usize,
}

impl LlmResolver {
    pub fn new(settings: LlmSettings, mut cache: ResolveCache) -> Self {
        cache.set_provider_model(provider_name(settings.provider), &settings.model);
        let (eff, _) = Self::effective_instructions(&settings);
        if eff == DEFAULT_SYSTEM_INSTRUCTIONS {
            cache.set_instructions_tag("");
        } else {
            use sha2::{Digest, Sha256};
            let tag = hex::encode(&Sha256::digest(eff.as_bytes())[..4]);
            cache.set_instructions_tag(&tag);
        }
        let timeout = Duration::from_millis(settings.timeout_ms.max(500));
        let base = || {
            ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout(timeout)
                .timeout_read(timeout)
                .timeout_write(timeout)
                // POSTs are not idempotent and a pooled socket that the peer
                // closed during idle surfaces as a confusing transport error.
                .max_idle_connections(0)
        };
        let proxy_url = [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty());
        let mut proxy_active = false;
        let mut builder = base();
        if let Some(proxy_url) = &proxy_url {
            if let Ok(proxy) = ureq::Proxy::new(proxy_url.trim()) {
                builder = builder.proxy(proxy);
                proxy_active = true;
            }
        }
        let agent = builder.build();
        let direct_agent = base().build();
        Self {
            settings,
            cache,
            agent,
            direct_agent,
            proxy_active,
            max_cache_entries: 2000,
        }
    }

    pub fn with_max_cache_entries(mut self, max_entries: usize) -> Self {
        self.max_cache_entries = max_entries.max(1);
        self
    }

    /// Lightweight connectivity + auth check against the configured provider.
    pub fn test_connection(mut settings: LlmSettings) -> Result<u128, ResolveError> {
        settings.timeout_ms = settings.timeout_ms.max(10_000);
        let probe_path = std::env::temp_dir().join(format!(
            "lexicue_probe_{}_{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let dummy_cache = ResolveCache::open(&probe_path)
            .map_err(|e| ResolveError::Unavailable(format!("probe cache: {e}")))?;
        let probe = Self::new(settings, dummy_cache);
        let start = std::time::Instant::now();
        let prompt = "Reply with JSON only: [{\"command\":\"pwd\",\"note\":\"ok\"}]";
        probe.complete(prompt)?;
        Ok(start.elapsed().as_millis())
    }

    /// Masks `KEY=value`, `--token`, bearer headers, URLs with credentials, and
    /// high-entropy tokens. Deterministic: same input -> same tokens.
    pub fn mask(input: &str) -> MaskedText {
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut redactions = 0usize;

        let mut token_for = |secret: &str| -> String {
            if let Some(existing) = seen.get(secret) {
                return existing.clone();
            }
            redactions += 1;
            let label = format!("<SECRET_{redactions}>");
            seen.insert(secret.to_string(), label.clone());
            label
        };

        let mut text = input.to_string();

        // 1. URLs with embedded userinfo: scheme://user:pass@host
        let url_re = Regex::new(r"(?i)([a-z][a-z0-9+.-]*://)([^/\s:@]+:[^/\s@]+)(@)").unwrap();
        text = url_re
            .replace_all(&text, |caps: &regex::Captures<'_>| {
                let tok = token_for(&caps[2]);
                format!("{}{}{}", &caps[1], tok, &caps[3])
            })
            .into_owned();

        // 2. Authorization Bearer/Basic headers
        let bearer_re = Regex::new(r"(?i)\b(Bearer|Basic)\s+([A-Za-z0-9._~+/=-]{8,})").unwrap();
        text = bearer_re
            .replace_all(&text, |caps: &regex::Captures<'_>| {
                let tok = token_for(&caps[2]);
                format!("{} {tok}", &caps[1])
            })
            .into_owned();

        // 3. KEY=value for sensitive variable names
        let kv_re = Regex::new(
            r"(?i)\b([A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASS|AUTH|CREDENTIAL)[A-Z0-9_]*)=([^\s]+)",
        )
        .unwrap();
        text = kv_re
            .replace_all(&text, |caps: &regex::Captures<'_>| {
                let val = &caps[2];
                if val.starts_with("<SECRET_") {
                    return caps[0].to_string();
                }
                let tok = token_for(val);
                format!("{}={tok}", &caps[1])
            })
            .into_owned();

        // 4. CLI flags like --token xyz / --password=xyz / --api-key xyz
        let flag_re =
            Regex::new(r"(?i)(--(?:token|password|passwd|secret|api-key|apikey))(?:\s+|=)([^\s]+)")
                .unwrap();
        text = flag_re
            .replace_all(&text, |caps: &regex::Captures<'_>| {
                let val = &caps[2];
                if val.starts_with("<SECRET_") {
                    return caps[0].to_string();
                }
                let tok = token_for(val);
                let sep = if caps[0].contains('=') { "=" } else { " " };
                format!("{}{sep}{tok}", &caps[1])
            })
            .into_owned();

        // 5. High-entropy / known-prefix standalone tokens (sk-..., ghp_..., AKIA..., or 24+ mixed chars)
        let prefix_re = Regex::new(r"\b(?:sk-[A-Za-z0-9_-]{12,}|gh[pousr]_[A-Za-z0-9]{16,}|xox[baprs]-[A-Za-z0-9-]{12,}|AKIA[0-9A-Z]{16})\b").unwrap();
        text = prefix_re
            .replace_all(&text, |caps: &regex::Captures<'_>| token_for(&caps[0]))
            .into_owned();

        MaskedText { text, redactions }
    }

    /// Resolves effective custom instructions: `system_prompt_file` -> `system_prompt` -> `DEFAULT_SYSTEM_INSTRUCTIONS`.
    /// Returns `(instructions, optional_warning)`.
    pub fn effective_instructions(settings: &LlmSettings) -> (String, Option<String>) {
        let file_raw = settings.system_prompt_file.trim();
        let mut warning = None;
        if !file_raw.is_empty() {
            let path = expand_tilde_path(file_raw);
            match std::fs::read_to_string(&path) {
                Ok(content) if !content.trim().is_empty() => {
                    return (content.trim().to_string(), None);
                }
                Ok(_) => {
                    warning = Some("Prompt file is empty — using fallback".to_string());
                }
                Err(_) => {
                    warning = Some("Prompt file not found — using fallback".to_string());
                }
            }
        }
        let inline = settings.system_prompt.trim();
        if !inline.is_empty() {
            return (inline.to_string(), warning);
        }
        (DEFAULT_SYSTEM_INSTRUCTIONS.to_string(), warning)
    }

    /// Structured JSON-only prompt using built-in default rules.
    #[allow(dead_code)] // Shortcut: отладочный рендер промпта, в рантайме не нужен.
    pub fn build_prompt(req: &ResolveRequest, masked: &MaskedText) -> String {
        Self::build_prompt_with_instructions(req, masked, DEFAULT_SYSTEM_INSTRUCTIONS)
    }

    /// Structured JSON-only prompt with caller-provided instructions + fixed JSON output contract.
    pub fn build_prompt_with_instructions(
        req: &ResolveRequest,
        masked: &MaskedText,
        instructions: &str,
    ) -> String {
        let limit = req.limit.clamp(1, 5);
        let markers: String = req
            .context
            .markers
            .iter()
            .map(|m| format!("{m:?}").to_lowercase())
            .collect::<Vec<_>>()
            .join(",");
        let cwd_tail = req
            .context
            .cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let mut ctx_compact = if markers.is_empty() {
            cwd_tail.to_string()
        } else {
            format!("{cwd_tail}:{markers}")
        };
        if ctx_compact.len() > CONTEXT_BUDGET_BYTES {
            // Truncate on a char boundary: byte offsets can split a glyph.
            let mut cut = CONTEXT_BUDGET_BYTES;
            while cut > 0 && !ctx_compact.is_char_boundary(cut) {
                cut -= 1;
            }
            ctx_compact.truncate(cut);
        }

        format!(
            "You translate a user's terminal request into exact, ready-to-run shell commands.\n\
             Environment: OS={os} shell={shell} ctx={ctx}\n\
             Instructions:\n\
             {instructions}\n\
             Output contract: Return at most {limit} candidates as a strict JSON array of {{\"command\":\"...\",\"note\":\"...\"}} (keep `note` <= 60 chars, use {{{{arg}}}} only if a required parameter was omitted). JSON only, no markdown fences or prose.\n\
             User intent: {intent}",
            os = req.context.os,
            shell = req.context.shell,
            ctx = ctx_compact,
            instructions = instructions.trim(),
            limit = limit,
            intent = masked.text,
        )
    }

    /// Accepts a JSON array of `{"command": "...", "note": "..."}` or fenced JSON.
    pub fn parse_response(raw: &str, limit: usize) -> Result<Vec<Candidate>, ResolveError> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum RawRow {
            Obj {
                command: String,
                #[serde(default)]
                note: Option<String>,
            },
            Str(String),
        }

        let trimmed = raw.trim();
        let unfenced = if let Some(rest) = trimmed.strip_prefix("```") {
            let after_lang = rest.split_once('\n').map(|(_, body)| body).unwrap_or(rest);
            after_lang
                .rsplit_once("```")
                .map(|(b, _)| b)
                .unwrap_or(after_lang)
                .trim()
        } else {
            trimmed
        };

        let json_slice = match (unfenced.find('['), unfenced.rfind(']')) {
            (Some(start), Some(end)) if end >= start => &unfenced[start..=end],
            _ => unfenced,
        };

        let items: Vec<RawRow> =
            serde_json::from_str(json_slice).map_err(|e| ResolveError::Parse(e.to_string()))?;

        let mut out = Vec::new();
        for (idx, item) in items.into_iter().take(limit.max(1)).enumerate() {
            let (cmd, note) = match item {
                RawRow::Obj { command, note } => (command, note),
                RawRow::Str(command) => (command, None),
            };
            let clean = cmd.trim().to_string();
            if clean.is_empty() {
                continue;
            }
            out.push(Candidate {
                id: cmd_id(&clean),
                template: clean.clone(),
                rendered: clean,
                source: CandidateSource::Llm,
                score: (0.90 - (idx as f32) * 0.05).max(0.1),
                safety: SafetyReport::default(),
                note,
            });
        }
        Ok(out)
    }

    fn resolve_api_key(&self) -> Result<String, ResolveError> {
        let direct = self.settings.api_key.trim();
        if !direct.is_empty() {
            return Ok(direct.to_string());
        }
        std::env::var(&self.settings.api_key_env)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or(ResolveError::NotConfigured)
    }

    /// Provider-specific HTTP call with `timeout_ms`.
    fn complete(&self, prompt: &str) -> Result<String, ResolveError> {
        match self.complete_with(&self.agent, prompt) {
            Ok(reply) => Ok(reply),
            Err(first) if self.proxy_active && matches!(&first, ResolveError::Network(_)) => {
                // Some local HTTP proxies terminate CONNECT/TLS with a bare EOF.
                // Retry the same request directly; keep the original error if
                // the network genuinely requires the proxy.
                self.complete_with(&self.direct_agent, prompt)
                    .map_err(|_| first)
            }
            Err(err) => Err(err),
        }
    }

    fn complete_with(&self, agent: &ureq::Agent, prompt: &str) -> Result<String, ResolveError> {
        match self.settings.provider {
            LlmProviderKind::Ollama => {
                let url = format!(
                    "{}/api/generate",
                    self.settings.endpoint.trim_end_matches('/')
                );
                let body = serde_json::json!({
                    "model": self.settings.model,
                    "prompt": prompt,
                    "stream": false,
                });
                let resp = agent.post(&url).send_json(body).map_err(map_ureq_err)?;
                let json: serde_json::Value = resp
                    .into_json()
                    .map_err(|e| ResolveError::Parse(e.to_string()))?;
                json.get("response")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        ResolveError::Parse("missing `response` field from Ollama".into())
                    })
            }
            LlmProviderKind::DeepSeek | LlmProviderKind::OpenAi => {
                let api_key = self.resolve_api_key()?;
                let base = self.settings.endpoint.trim_end_matches('/');
                let url = if base.ends_with("/chat/completions") {
                    base.to_string()
                } else if base.ends_with("/v1") {
                    format!("{base}/chat/completions")
                } else {
                    format!("{base}/v1/chat/completions")
                };

                let body = serde_json::json!({
                    "model": self.settings.model,
                    "messages": [{"role": "user", "content": prompt}],
                    "temperature": 0.1,
                });
                let resp = agent
                    .post(&url)
                    .set("Authorization", &format!("Bearer {api_key}"))
                    .send_json(body)
                    .map_err(map_ureq_err)?;
                let json: serde_json::Value = resp
                    .into_json()
                    .map_err(|e| ResolveError::Parse(e.to_string()))?;
                json.pointer("/choices/0/message/content")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        ResolveError::Parse("missing `choices[0].message.content`".into())
                    })
            }
            LlmProviderKind::Gemini => {
                let api_key = self.resolve_api_key()?;
                let base = self.settings.endpoint.trim_end_matches('/');
                let model = self.settings.model.trim();
                let url = if base.contains(":generateContent") {
                    base.to_string()
                } else if base.ends_with("/v1beta") || base.ends_with("/v1") {
                    format!("{base}/models/{model}:generateContent")
                } else {
                    format!("{base}/v1beta/models/{model}:generateContent")
                };
                let body = serde_json::json!({
                    "contents": [{"parts": [{"text": prompt}]}],
                    "generationConfig": {"temperature": 0.1}
                });
                let resp = agent
                    .post(&url)
                    .set("x-goog-api-key", &api_key)
                    .send_json(body)
                    .map_err(map_ureq_err)?;
                let json: serde_json::Value = resp
                    .into_json()
                    .map_err(|e| ResolveError::Parse(e.to_string()))?;
                json.pointer("/candidates/0/content/parts/0/text")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        ResolveError::Parse("missing `candidates[0].content.parts[0].text`".into())
                    })
            }
            LlmProviderKind::Anthropic => {
                let api_key = self.resolve_api_key()?;
                let base = self.settings.endpoint.trim_end_matches('/');
                let url = if base.ends_with("/messages") {
                    base.to_string()
                } else if base.ends_with("/v1") {
                    format!("{base}/messages")
                } else {
                    format!("{base}/v1/messages")
                };
                let body = serde_json::json!({
                    "model": self.settings.model,
                    "max_tokens": 512,
                    "messages": [{"role": "user", "content": prompt}],
                });
                let resp = agent
                    .post(&url)
                    .set("x-api-key", &api_key)
                    .set("anthropic-version", "2023-06-01")
                    .send_json(body)
                    .map_err(map_ureq_err)?;
                let json: serde_json::Value = resp
                    .into_json()
                    .map_err(|e| ResolveError::Parse(e.to_string()))?;
                json.pointer("/content/0/text")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| ResolveError::Parse("missing `content[0].text`".into()))
            }
        }
    }
}

fn map_ureq_err(err: ureq::Error) -> ResolveError {
    match err {
        ureq::Error::Transport(t) => {
            let msg = t.to_string();
            if msg.to_lowercase().contains("timed out") || msg.to_lowercase().contains("timeout") {
                ResolveError::Timeout
            } else {
                ResolveError::Network(msg)
            }
        }
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            ResolveError::Network(format!("HTTP {code}: {body}"))
        }
    }
}

impl CommandResolver for LlmResolver {
    fn id(&self) -> &'static str {
        "llm"
    }

    fn priority(&self) -> u8 {
        90
    }

    fn resolve(&mut self, req: &ResolveRequest) -> Result<Vec<Candidate>, ResolveError> {
        let should_run = req.trigger == Trigger::Explicit || self.settings.enabled;
        if !should_run {
            return Ok(Vec::new());
        }

        let trigger_prefix = self.settings.explicit_trigger.as_str();
        let raw_clean = req
            .raw_input
            .trim()
            .strip_prefix(trigger_prefix)
            .unwrap_or(req.raw_input.trim())
            .trim();
        if raw_clean.is_empty() {
            return Ok(Vec::new());
        }

        let masked = Self::mask(raw_clean);

        // Always check local cache first ($0 repeat, zero network).
        if let Some(entry) = self.cache.lookup(&masked.text, &req.context) {
            let _ = self.cache.persist();
            return Ok(vec![entry.to_candidate(cmd_id(&entry.template))]);
        }

        let (instructions, _) = Self::effective_instructions(&self.settings);
        let prompt = Self::build_prompt_with_instructions(req, &masked, &instructions);
        let reply = self.complete(&prompt)?;
        let limit = req.limit.min(self.settings.max_candidates).max(1);
        let mut cands = Self::parse_response(&reply, limit)?;

        let policy = SafetyPolicy::default();
        let prov = provider_name(self.settings.provider);
        for cand in &mut cands {
            cand.safety = safety::analyze(&cand.rendered, &policy, &req.context);
        }

        if let Some(best) = cands.first() {
            let _ = self
                .cache
                .put(&masked.text, &req.context, best, prov, &self.settings.model);
            self.cache.prune(self.max_cache_entries);
            let _ = self.cache.persist();
        }

        Ok(cands)
    }

    fn resolve_live(&mut self, req: &ResolveRequest) -> Vec<Candidate> {
        let trigger_prefix = self.settings.explicit_trigger.as_str();
        let raw_clean = req
            .raw_input
            .trim()
            .strip_prefix(trigger_prefix)
            .unwrap_or(req.raw_input.trim())
            .trim();
        if raw_clean.len() < 2 {
            return Vec::new();
        }
        let masked = Self::mask(raw_clean);
        if let Some(entry) = self.cache.lookup(&masked.text, &req.context) {
            return vec![entry.to_candidate(cmd_id(&entry.template))];
        }
        Vec::new()
    }

    fn clear_cache(&mut self) -> usize {
        self.cache.clear().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{CwdContext, ProjectMarker};
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn masks_secrets_deterministically() {
        let input = "curl -H 'Authorization: Bearer my_super_secret_token_123' https://alice:hunter2@api.example.com API_KEY=sk-1234567890abcdef1234 --token tok_abcdef";
        let m1 = LlmResolver::mask(input);
        let m2 = LlmResolver::mask(input);
        assert_eq!(m1, m2);
        assert!(!m1.text.contains("my_super_secret_token_123"));
        assert!(!m1.text.contains("hunter2"));
        assert!(!m1.text.contains("sk-1234567890abcdef1234"));
        assert!(!m1.text.contains("tok_abcdef"));
        assert!(m1.redactions >= 4);
    }

    #[test]
    fn cache_key_normalization_and_corrupt_line_recovery() {
        let dir = tempdir().unwrap();
        let cache_file = dir.path().join("cache.jsonl");
        std::fs::write(&cache_file, "{bad json line\n").unwrap();

        let ctx_cargo = CwdContext {
            cwd: PathBuf::from("/tmp/repo-a"),
            markers: vec![ProjectMarker::Cargo, ProjectMarker::Git],
            ..Default::default()
        };
        let ctx_cargo_reordered = CwdContext {
            cwd: PathBuf::from("/tmp/repo-b"),
            markers: vec![ProjectMarker::Git, ProjectMarker::Cargo],
            ..Default::default()
        };

        let k1 = ResolveCache::normalize_key("??  Show   Big Files ", &ctx_cargo, "ollama", "qwen");
        let k2 =
            ResolveCache::normalize_key("show big files", &ctx_cargo_reordered, "ollama", "qwen");
        assert_eq!(k1, k2);

        let mut cache = ResolveCache::open(&cache_file).unwrap();
        let cand = Candidate {
            id: 42,
            template: "du -ah . | sort -rh | head -20".into(),
            rendered: "du -ah . | sort -rh | head -20".into(),
            source: CandidateSource::Llm,
            score: 0.9,
            safety: SafetyReport::default(),
            note: Some("largest files".into()),
        };
        cache
            .put("?? show big files", &ctx_cargo, &cand, "ollama", "qwen")
            .unwrap();
        cache.persist().unwrap();

        // Reload from disk and resolve identical intent in different directory with same markers.
        let mut reopened = ResolveCache::open(&cache_file).unwrap();
        reopened.set_provider_model("ollama", "qwen");
        let hit = reopened
            .lookup("show big files", &ctx_cargo_reordered)
            .unwrap();
        assert_eq!(hit.template, "du -ah . | sort -rh | head -20");
        assert_eq!(hit.to_candidate(1).source, CandidateSource::Cache);
    }

    #[test]
    fn repeated_explicit_query_resolves_from_cache_without_network() {
        let dir = tempdir().unwrap();
        let cache_file = dir.path().join("cache.jsonl");
        let ctx = CwdContext {
            cwd: PathBuf::from("/tmp/proj"),
            markers: vec![ProjectMarker::Cargo],
            shell: "/bin/zsh".into(),
            os: "linux".into(),
        };

        let mut cache = ResolveCache::open(&cache_file).unwrap();
        let cand = Candidate {
            id: 7,
            template: "cargo test --workspace".into(),
            rendered: "cargo test --workspace".into(),
            source: CandidateSource::Llm,
            score: 0.9,
            safety: SafetyReport::default(),
            note: None,
        };
        cache
            .put(
                "run all rust tests",
                &ctx,
                &cand,
                "ollama",
                "qwen2.5-coder:3b",
            )
            .unwrap();
        cache.persist().unwrap();

        let reopened = ResolveCache::open(&cache_file).unwrap();
        // Unreachable endpoint proves zero network calls occur on cache hit.
        let settings = LlmSettings {
            enabled: false,
            provider: LlmProviderKind::Ollama,
            model: "qwen2.5-coder:3b".into(),
            endpoint: "http://127.0.0.1:1".into(),
            api_key: String::new(),
            api_key_env: "DEEPSEEK_API_KEY".into(),
            timeout_ms: 500,
            explicit_trigger: "??".into(),
            max_candidates: 5,
            system_prompt: String::new(),
            system_prompt_file: String::new(),
        };
        let mut llm = LlmResolver::new(settings, reopened);
        let req = ResolveRequest {
            raw_input: "?? run all rust tests".into(),
            context: ctx,
            history_tail: Vec::new(),
            trigger: Trigger::Explicit,
            limit: 5,
        };
        let hits = llm.resolve(&req).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, CandidateSource::Cache);
        assert_eq!(hits[0].rendered, "cargo test --workspace");
    }

    #[test]
    fn parses_fenced_json_and_degrades_on_provider_error() {
        let raw = "```json\n[{\"command\":\"find . -size +10M\",\"note\":\"big files\"}]\n```";
        let parsed = LlmResolver::parse_response(raw, 5).unwrap();
        assert_eq!(parsed[0].rendered, "find . -size +10M");
        assert_eq!(parsed[0].source, CandidateSource::Llm);

        let dir = tempdir().unwrap();
        let cache = ResolveCache::open(&dir.path().join("cache.jsonl")).unwrap();
        let settings = LlmSettings {
            enabled: true,
            provider: LlmProviderKind::DeepSeek,
            model: "deepseek-chat".into(),
            endpoint: "http://127.0.0.1:1".into(),
            api_key: String::new(),
            api_key_env: "TERMBY_TEST_MISSING_KEY_XYZ".into(),
            timeout_ms: 500,
            explicit_trigger: "??".into(),
            max_candidates: 5,
            system_prompt: String::new(),
            system_prompt_file: String::new(),
        };
        let mut llm = LlmResolver::new(settings, cache);
        let req = ResolveRequest {
            raw_input: "?? list open ports".into(),
            context: CwdContext::default(),
            history_tail: Vec::new(),
            trigger: Trigger::Explicit,
            limit: 5,
        };
        assert!(matches!(
            llm.resolve(&req),
            Err(ResolveError::NotConfigured)
        ));
    }

    #[test]
    fn custom_instructions_inline_and_file_precedence() {
        let dir = tempdir().unwrap();
        let prompt_path = dir.path().join("custom_prompt.txt");
        std::fs::write(&prompt_path, "Prefer fd and rg over find and grep.").unwrap();

        let mut settings = LlmSettings {
            enabled: true,
            provider: LlmProviderKind::Ollama,
            model: "qwen2.5-coder:3b".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            api_key: String::new(),
            api_key_env: String::new(),
            timeout_ms: 25000,
            explicit_trigger: "??".into(),
            max_candidates: 5,
            system_prompt: "Inline custom rule".into(),
            system_prompt_file: prompt_path.display().to_string(),
        };

        let (eff, warn) = LlmResolver::effective_instructions(&settings);
        assert!(warn.is_none());
        assert_eq!(eff, "Prefer fd and rg over find and grep.");

        settings.system_prompt_file = dir.path().join("missing.txt").display().to_string();
        let (eff2, warn2) = LlmResolver::effective_instructions(&settings);
        assert!(warn2.is_some());
        assert_eq!(eff2, "Inline custom rule");

        settings.system_prompt_file.clear();
        settings.system_prompt.clear();
        let (eff3, warn3) = LlmResolver::effective_instructions(&settings);
        assert!(warn3.is_none());
        assert!(eff3.contains("xdg-user-dir"));
    }
}
