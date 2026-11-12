//! TOML configuration schema.
//!
//! File resolution order:
//! 1. explicit `--config <path>`;
//! 2. `$XDG_CONFIG_HOME/lexicue/config.toml` (fallback `~/.config/lexicue/config.toml`);
//! 3. legacy `$XDG_CONFIG_HOME/termby/config.toml` (fallback `~/.config/termby/config.toml`);
//! 4. compiled-in defaults (written to disk on first run).
//!
//! A leading `~/` in every path field is expanded by the loader, not by serde.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Root config object; mirrors `config.example.toml` one-to-one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub general: GeneralSettings,
    pub resolver: ResolverSettings,
    pub llm: LlmSettings,
    pub cache: CacheSettings,
    pub guard: GuardSettings,
    pub safety: SafetySettings,
    pub theme: ThemeSection,
    #[serde(default)]
    pub island: IslandSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralSettings {
                history_limit: 200,
                context_bytes: crate::contracts::CONTEXT_BUDGET_BYTES,
                shell: None,
                scrollback_lines: 2000,
                language: UiLanguage::En,
            },
            resolver: ResolverSettings {
                aliases_rc: vec![PathBuf::from("~/.bashrc"), PathBuf::from("~/.zshrc")],
                user_aliases: PathBuf::from("~/.config/lexicue/aliases.toml"),
                dictionary_dirs: vec![
                    PathBuf::from("~/.cache/tldr"),
                    PathBuf::from("~/.local/share/tldr"),
                ],
                context_markers: vec![
                    "Cargo.toml".into(),
                    "package.json".into(),
                    "go.mod".into(),
                    "pyproject.toml".into(),
                    "Makefile".into(),
                    "Dockerfile".into(),
                ],
                max_candidates: 8,
                history_bonus: 1.5,
            },
            llm: LlmSettings {
                enabled: false,
                provider: LlmProviderKind::Ollama,
                model: "qwen2.5-coder:3b".into(),
                endpoint: "http://127.0.0.1:11434".into(),
                api_key: String::new(),
                api_key_env: "DEEPSEEK_API_KEY".into(),
                timeout_ms: 25000,
                explicit_trigger: "??".into(),
                max_candidates: 5,
                system_prompt: String::new(),
                system_prompt_file: String::new(),
            },
            cache: CacheSettings {
                path: PathBuf::from("~/.cache/lexicue/cache.jsonl"),
                max_entries: 2000,
            },
            guard: GuardSettings {
                tui_titles: [
                    "vim", "nvim", "vi", "htop", "top", "less", "more", "man", "ssh", "tmux",
                    "fzf", "lazygit",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                password_prompt_markers: ["password:", "passphrase:", "[sudo]", "Enter passphrase"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                force_passthrough_keys: (1..=12).map(|n| format!("f{n}")).collect(),
            },
            safety: SafetySettings {
                confirm_dangerous: true,
                blocked_patterns: [
                    "rm -rf /",
                    "dd if=.* of=/dev/",
                    "mkfs\\.",
                    "chmod -R 777 /",
                    ":(){ :|:& };:",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                system_paths: [
                    "/", "/etc", "/usr", "/bin", "/sbin", "/boot", "/lib", "/var", "/home",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
            },
            theme: ThemeSection {
                background: "#252527".into(),
                foreground: "#e4e4e4".into(),
                accent: "#d4d4d4".into(),
                warning: "#d9a05b".into(),
                danger: "#e06c75".into(),
                selection: "#363639".into(),
                overlay_bg: "#2c2c2f".into(),
                font_size: 14.0,
                prompt_icon: PromptIcon::Arrow,
            },
            island: IslandSettings::default(),
        }
    }
}

fn expand_tilde(path: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(home) = dirs::home_dir() else {
        return path.to_path_buf();
    };
    if raw == "~" {
        return home;
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.join(rest);
    }
    path.to_path_buf()
}

/// Writes `body` to `path` with owner-only permissions (0600), also fixing the
/// mode of a pre-existing world-readable file.
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = open_private(path)?;
    file.write_all(body.as_bytes())?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(unix)]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::create(path)
}

impl Config {
    /// Read + validate. `None` uses the XDG path. Creates the file with defaults if absent.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let (mut cfg, _) =
            Self::load_inner(path, &Self::config_path(), &Self::legacy_config_path())?;

        if cfg.general.context_bytes > crate::contracts::CONTEXT_BUDGET_BYTES {
            return Err(ConfigError::Invalid(format!(
                "general.context_bytes ({}) exceeds limit ({})",
                cfg.general.context_bytes,
                crate::contracts::CONTEXT_BUDGET_BYTES
            )));
        }

        crate::theme::ThemePalette::from_section(&cfg.theme)
            .map_err(|e| ConfigError::Invalid(e.to_string()))?;

        cfg.resolver.aliases_rc = cfg
            .resolver
            .aliases_rc
            .iter()
            .map(|p| expand_tilde(p))
            .collect();
        cfg.resolver.user_aliases = expand_tilde(&cfg.resolver.user_aliases);
        cfg.resolver.dictionary_dirs = cfg
            .resolver
            .dictionary_dirs
            .iter()
            .map(|p| expand_tilde(p))
            .collect();
        cfg.cache.path = expand_tilde(&cfg.cache.path);
        cfg.island.clamp();

        Ok(cfg)
    }

    /// Internal loader separating IO resolution from path defaults for deterministic testing.
    /// Returns `(config, loaded_from_legacy)`.
    pub fn load_inner(
        explicit: Option<&Path>,
        new_path: &Path,
        legacy_path: &Path,
    ) -> Result<(Self, bool), ConfigError> {
        if let Some(p) = explicit {
            let raw = std::fs::read_to_string(p)
                .map_err(|e| ConfigError::Io(format!("{}: {e}", p.display())))?;
            let cfg = toml::from_str::<Config>(&raw)
                .map_err(|e| ConfigError::Parse(format!("{}: {e}", p.display())))?;
            return Ok((cfg, false));
        }

        if new_path.exists() {
            let raw = std::fs::read_to_string(new_path)
                .map_err(|e| ConfigError::Io(format!("{}: {e}", new_path.display())))?;
            let cfg = toml::from_str::<Config>(&raw)
                .map_err(|e| ConfigError::Parse(format!("{}: {e}", new_path.display())))?;
            return Ok((cfg, false));
        }

        if legacy_path.exists() {
            let raw = std::fs::read_to_string(legacy_path)
                .map_err(|e| ConfigError::Io(format!("{}: {e}", legacy_path.display())))?;
            let mut cfg = toml::from_str::<Config>(&raw)
                .map_err(|e| ConfigError::Parse(format!("{}: {e}", legacy_path.display())))?;
            Self::remap_legacy_defaults(&mut cfg);
            return Ok((cfg, true));
        }

        let default_cfg = Config::default();
        if let Some(parent) = new_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Io(format!("{}: {e}", parent.display())))?;
        }
        let serialized =
            toml::to_string_pretty(&default_cfg).map_err(|e| ConfigError::Parse(e.to_string()))?;
        write_private(new_path, &serialized)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", new_path.display())))?;
        Ok((default_cfg, false))
    }

    /// Remaps known default paths from legacy `termby` to `lexicue` without altering custom user paths.
    fn remap_legacy_defaults(cfg: &mut Config) {
        let legacy_aliases_tilde = PathBuf::from("~/.config/termby/aliases.toml");
        let legacy_cache_tilde = PathBuf::from("~/.cache/termby/cache.jsonl");

        if cfg.resolver.user_aliases == legacy_aliases_tilde
            || cfg
                .resolver
                .user_aliases
                .ends_with(".config/termby/aliases.toml")
        {
            cfg.resolver.user_aliases = PathBuf::from("~/.config/lexicue/aliases.toml");
        }

        if cfg.cache.path == legacy_cache_tilde
            || cfg.cache.path.ends_with(".cache/termby/cache.jsonl")
        {
            cfg.cache.path = PathBuf::from("~/.cache/lexicue/cache.jsonl");
        }
    }

    /// Canonical config path used when `--config` is not given ($XDG_CONFIG_HOME/lexicue/config.toml).
    pub fn config_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("lexicue").join("config.toml");
            }
        }
        dirs::config_dir()
            .unwrap_or_else(|| expand_tilde(Path::new("~/.config")))
            .join("lexicue")
            .join("config.toml")
    }

    /// Legacy config path checked as fallback ($XDG_CONFIG_HOME/termby/config.toml).
    pub fn legacy_config_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("termby").join("config.toml");
            }
        }
        dirs::config_dir()
            .unwrap_or_else(|| expand_tilde(Path::new("~/.config")))
            .join("termby")
            .join("config.toml")
    }

    /// Cache file path from `[cache].path`, with `~/` expanded.
    pub fn cache_path(&self) -> PathBuf {
        expand_tilde(&self.cache.path)
    }

    /// Write current config to `path` (or XDG default when `None`).
    ///
    /// Config may hold `[llm].api_key`, so it is written owner-only (0600).
    pub fn save(&self, path: Option<&Path>) -> Result<(), ConfigError> {
        let target = path.map(PathBuf::from).unwrap_or_else(Self::config_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Io(format!("{}: {e}", parent.display())))?;
        }
        let body = toml::to_string_pretty(self).map_err(|e| ConfigError::Parse(e.to_string()))?;
        write_private(&target, &body)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", target.display())))
    }

    /// Effective login shell: `[general].shell` or `$SHELL` or `/bin/sh`.
    pub fn shell(&self) -> String {
        self.general
            .shell
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                std::env::var("SHELL")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "/bin/sh".to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UiLanguage {
    #[default]
    En,
    Ru,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneralSettings {
    pub history_limit: usize,
    /// Bytes of cwd/marker context allowed to reach resolvers; must be <= 30.
    pub context_bytes: usize,
    pub shell: Option<String>,
    pub scrollback_lines: usize,
    #[serde(default)]
    pub language: UiLanguage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolverSettings {
    /// Shell rc files scanned read-only for aliases (never executed).
    pub aliases_rc: Vec<PathBuf>,
    /// termby-owned alias file; wins over imported rc aliases.
    pub user_aliases: PathBuf,
    /// tldr markdown roots (`pages/common`, `pages/linux` are read if present).
    pub dictionary_dirs: Vec<PathBuf>,
    /// Filenames that upgrade a directory to a project-marker context.
    pub context_markers: Vec<String>,
    pub max_candidates: usize,
    /// Score bonus for commands that match the detected project markers.
    pub history_bonus: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmSettings {
    /// Master switch; explicit `??` still works when a provider is configured.
    pub enabled: bool,
    pub provider: LlmProviderKind,
    pub model: String,
    pub endpoint: String,
    /// Optional direct API key saved from Settings (takes precedence over `api_key_env`).
    #[serde(default)]
    pub api_key: String,
    /// Env var holding the key when `api_key` is empty.
    pub api_key_env: String,
    pub timeout_ms: u64,
    /// Prefix that forces this layer, default `??`.
    pub explicit_trigger: String,
    pub max_candidates: usize,
    /// Optional inline custom instructions overriding the built-in shell rules.
    #[serde(default)]
    pub system_prompt: String,
    /// Optional path (supports `~/`) to a custom prompt instructions file; takes precedence over `system_prompt`.
    #[serde(default)]
    pub system_prompt_file: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProviderKind {
    Ollama,
    DeepSeek,
    Gemini,
    Anthropic,
    #[serde(rename = "openai")]
    OpenAi,
}

impl LlmProviderKind {
    /// Returns `(default_endpoint, default_model, default_api_key_env)` for preset switching.
    pub fn defaults(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Ollama => ("http://127.0.0.1:11434", "qwen2.5-coder:3b", ""),
            Self::DeepSeek => (
                "https://api.deepseek.com",
                "deepseek-chat",
                "DEEPSEEK_API_KEY",
            ),
            Self::Gemini => (
                "https://generativelanguage.googleapis.com/v1beta",
                "gemini-2.5-flash",
                "GEMINI_API_KEY",
            ),
            Self::Anthropic => (
                "https://api.anthropic.com",
                "claude-sonnet-4-20250514",
                "ANTHROPIC_API_KEY",
            ),
            Self::OpenAi => ("https://api.openai.com", "gpt-4o-mini", "OPENAI_API_KEY"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheSettings {
    pub path: PathBuf,
    pub max_entries: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuardSettings {
    /// Window titles that force passthrough even without alternate screen.
    pub tui_titles: Vec<String>,
    /// Substrings on the last visible line that mean "password prompt".
    pub password_prompt_markers: Vec<String>,
    /// Keys always forwarded untouched (raw byte names, e.g. "f1").
    pub force_passthrough_keys: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SafetySettings {
    pub confirm_dangerous: bool,
    /// Extra regexes; the built-in set is always active.
    pub blocked_patterns: Vec<String>,
    /// Paths whose overwrite is always `Dangerous`.
    pub system_paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptIcon {
    #[default]
    Arrow,
    Spark,
    Bolt,
    Cat,
    Ghost,
    Rocket,
    Lambda,
    Chaos,
}

/// Raw hex palette; converted to `theme::ThemePalette` once at startup.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThemeSection {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub warning: String,
    pub danger: String,
    pub selection: String,
    pub overlay_bg: String,
    pub font_size: f32,
    #[serde(default)]
    pub prompt_icon: PromptIcon,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct IslandSettings {
    pub enabled: bool,
    pub timer_threshold_ms: u64,
    pub status_ttl_ms: u64,
    pub port_ttl_secs: u64,
    pub port_revalidate_secs: u64,
    pub max_ports: usize,
    pub max_snippets: usize,
    pub snippets: SnippetSettings,
}

impl Default for IslandSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            timer_threshold_ms: 2000,
            status_ttl_ms: 4000,
            port_ttl_secs: 45,
            port_revalidate_secs: 5,
            max_ports: 4,
            max_snippets: 3,
            snippets: SnippetSettings::default(),
        }
    }
}

impl IslandSettings {
    pub fn clamp(&mut self) {
        self.max_ports = self.max_ports.clamp(1, 4);
        self.max_snippets = self.max_snippets.clamp(1, 3);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SnippetSettings {
    pub cargo: Vec<String>,
    pub node: Vec<String>,
    pub go: Vec<String>,
    pub python: Vec<String>,
    pub make: Vec<String>,
    pub docker: Vec<String>,
    pub git: Vec<String>,
}

impl Default for SnippetSettings {
    fn default() -> Self {
        Self {
            cargo: vec![
                "cargo test".into(),
                "cargo run".into(),
                "cargo clippy".into(),
            ],
            node: vec![
                "npm test".into(),
                "npm run dev".into(),
                "npm run build".into(),
            ],
            go: vec![
                "go test ./...".into(),
                "go run .".into(),
                "go vet ./...".into(),
            ],
            python: vec![
                "pytest".into(),
                "python -m compileall .".into(),
                "pip check".into(),
            ],
            make: vec!["make".into(), "make test".into(), "make check".into()],
            docker: vec![
                "docker compose up".into(),
                "docker build .".into(),
                "docker ps".into(),
            ],
            git: vec![
                "git status".into(),
                "git diff".into(),
                "git log --oneline -n 5".into(),
            ],
        }
    }
}

impl SnippetSettings {
    pub fn for_marker(&self, marker: crate::contracts::ProjectMarker) -> &[String] {
        match marker {
            crate::contracts::ProjectMarker::Cargo => &self.cargo,
            crate::contracts::ProjectMarker::Node => &self.node,
            crate::contracts::ProjectMarker::Go => &self.go,
            crate::contracts::ProjectMarker::Python => &self.python,
            crate::contracts::ProjectMarker::Make => &self.make,
            crate::contracts::ProjectMarker::Docker => &self.docker,
            crate::contracts::ProjectMarker::Git => &self.git,
            crate::contracts::ProjectMarker::Unknown => &[],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config io error: {0}")]
    Io(String),
    #[error("config parse error: {0}")]
    Parse(String),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_existing_new_config() {
        let dir = tempdir().unwrap();
        let new_path = dir.path().join("lexicue").join("config.toml");
        let legacy_path = dir.path().join("termby").join("config.toml");
        std::fs::create_dir_all(new_path.parent().unwrap()).unwrap();

        let mut sample = Config::default();
        sample.general.history_limit = 4321;
        std::fs::write(&new_path, toml::to_string_pretty(&sample).unwrap()).unwrap();

        let (loaded, from_legacy) = Config::load_inner(None, &new_path, &legacy_path).unwrap();
        assert_eq!(loaded.general.history_limit, 4321);
        assert!(!from_legacy);
    }

    #[test]
    fn falls_back_to_legacy_and_remaps_defaults() {
        let dir = tempdir().unwrap();
        let new_path = dir.path().join("lexicue").join("config.toml");
        let legacy_path = dir.path().join("termby").join("config.toml");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();

        let legacy_toml = r##"
[general]
history_limit = 777
context_bytes = 30
scrollback_lines = 1000

[resolver]
aliases_rc = []
user_aliases = "~/.config/termby/aliases.toml"
dictionary_dirs = []
context_markers = []
max_candidates = 5
history_bonus = 1.0

[llm]
enabled = false
provider = "ollama"
model = "qwen"
endpoint = "http://localhost:11434"
api_key = ""
api_key_env = ""
timeout_ms = 10000
explicit_trigger = "??"
max_candidates = 3
system_prompt = ""
system_prompt_file = ""

[cache]
path = "~/.cache/termby/cache.jsonl"
max_entries = 500

[guard]
tui_titles = []
password_prompt_markers = []
force_passthrough_keys = []

[safety]
confirm_dangerous = true
blocked_patterns = []
system_paths = []

[theme]
background = "#000000"
foreground = "#ffffff"
accent = "#00ff00"
warning = "#ffff00"
danger = "#ff0000"
selection = "#333333"
overlay_bg = "#111111"
font_size = 13.0
"##;
        std::fs::write(&legacy_path, legacy_toml).unwrap();

        let (loaded, from_legacy) = Config::load_inner(None, &new_path, &legacy_path).unwrap();
        assert_eq!(loaded.general.history_limit, 777);
        assert!(from_legacy);
        assert_eq!(
            loaded.resolver.user_aliases,
            PathBuf::from("~/.config/lexicue/aliases.toml")
        );
        assert_eq!(
            loaded.cache.path,
            PathBuf::from("~/.cache/lexicue/cache.jsonl")
        );
        assert!(!new_path.exists());
    }

    #[test]
    fn creates_new_defaults_when_both_absent() {
        let dir = tempdir().unwrap();
        let new_path = dir.path().join("lexicue").join("config.toml");
        let legacy_path = dir.path().join("termby").join("config.toml");

        let (loaded, from_legacy) = Config::load_inner(None, &new_path, &legacy_path).unwrap();
        assert_eq!(loaded.general.history_limit, 200);
        assert!(!from_legacy);
        assert!(new_path.exists());
    }

    #[test]
    fn explicit_path_bypasses_defaults_and_fallback() {
        let dir = tempdir().unwrap();
        let explicit = dir.path().join("custom.toml");
        let new_path = dir.path().join("lexicue").join("config.toml");
        let legacy_path = dir.path().join("termby").join("config.toml");

        let mut custom = Config::default();
        custom.general.history_limit = 999;
        std::fs::write(&explicit, toml::to_string_pretty(&custom).unwrap()).unwrap();

        let (loaded, from_legacy) =
            Config::load_inner(Some(&explicit), &new_path, &legacy_path).unwrap();
        assert_eq!(loaded.general.history_limit, 999);
        assert!(!from_legacy);
    }

    pub mod island {
        use super::*;

        #[test]
        fn round_trip_loads_config_without_island_section() {
            let toml_str = r##"
[general]
history_limit = 100
context_bytes = 25
scrollback_lines = 500

[resolver]
aliases_rc = []
user_aliases = "~/.config/lexicue/aliases.toml"
dictionary_dirs = []
context_markers = ["Cargo.toml"]
max_candidates = 5
history_bonus = 1.0

[llm]
enabled = false
provider = "ollama"
model = "qwen"
endpoint = "http://localhost:11434"
api_key = ""
api_key_env = ""
timeout_ms = 10000
explicit_trigger = "??"
max_candidates = 3
system_prompt = ""
system_prompt_file = ""

[cache]
path = "~/.cache/lexicue/cache.jsonl"
max_entries = 500

[guard]
tui_titles = []
password_prompt_markers = []
force_passthrough_keys = []

[safety]
confirm_dangerous = true
blocked_patterns = []
system_paths = []

[theme]
background = "#000000"
foreground = "#ffffff"
accent = "#00ff00"
warning = "#ffff00"
danger = "#ff0000"
selection = "#333333"
overlay_bg = "#111111"
font_size = 13.0
"##;
            let loaded: Config = toml::from_str(toml_str).unwrap();
            assert!(loaded.island.enabled);
            assert_eq!(loaded.island.timer_threshold_ms, 2000);
            assert_eq!(loaded.island.status_ttl_ms, 4000);
            assert_eq!(loaded.island.port_ttl_secs, 45);
            assert_eq!(loaded.island.port_revalidate_secs, 5);
            assert_eq!(loaded.island.max_ports, 4);
            assert_eq!(loaded.island.max_snippets, 3);
            assert_eq!(
                loaded
                    .island
                    .snippets
                    .for_marker(crate::contracts::ProjectMarker::Cargo),
                &["cargo test", "cargo run", "cargo clippy"]
            );
        }

        #[test]
        fn clamps_island_settings() {
            let mut island = IslandSettings {
                max_ports: 99,
                max_snippets: 0,
                ..IslandSettings::default()
            };
            island.clamp();
            assert_eq!(island.max_ports, 4);
            assert_eq!(island.max_snippets, 1);
        }
    }
}
