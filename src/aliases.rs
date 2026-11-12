//! User alias table: explicit `~/.config/termby/aliases.toml` plus read-only
//! import from `~/.bashrc` / `~/.zshrc`. Never sources or executes rc files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where an alias came from; user config overrides rc imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AliasOrigin {
    Bashrc(PathBuf),
    Zshrc(PathBuf),
    UserConfig(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasEntry {
    pub name: String,
    pub expansion: String,
    pub origin: AliasOrigin,
}

/// Immutable after load; rebuilt on file change or SIGHUP.
#[derive(Debug, Default)]
pub struct AliasTable {
    entries: BTreeMap<String, AliasEntry>,
}

impl AliasTable {
    /// Merge rc imports first, then user config (user wins). Missing rc files are skipped;
    /// a missing user alias file is created empty.
    pub fn load(rc_files: &[PathBuf], user_config: &Path) -> Result<Self, AliasError> {
        let mut table = Self::default();

        // Precedence: user file > zshrc > bashrc. Import bashrc before zshrc, then user config.
        let mut ordered_rc: Vec<&PathBuf> = rc_files.iter().collect();
        ordered_rc.sort_by_key(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.contains("zsh") {
                1u8
            } else {
                0u8
            }
        });

        for path in ordered_rc {
            if !path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(path)
                .map_err(|e| AliasError::Io(format!("{}: {e}", path.display())))?;
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let origin = if name.contains("zsh") {
                AliasOrigin::Zshrc(path.clone())
            } else {
                AliasOrigin::Bashrc(path.clone())
            };
            for entry in Self::parse_shell_aliases(&text, origin) {
                table.insert(entry);
            }
        }

        if !user_config.exists() {
            if let Some(parent) = user_config.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| AliasError::Io(format!("{}: {e}", parent.display())))?;
            }
            std::fs::write(user_config, "version = 1\n\n[aliases]\n")
                .map_err(|e| AliasError::Io(format!("{}: {e}", user_config.display())))?;
            return Ok(table);
        }

        let user_text = std::fs::read_to_string(user_config)
            .map_err(|e| AliasError::Io(format!("{}: {e}", user_config.display())))?;
        let user_entries = Self::parse_user_config(
            &user_text,
            AliasOrigin::UserConfig(user_config.to_path_buf()),
        )?;
        for entry in user_entries {
            table.insert(entry);
        }
        Ok(table)
    }

    /// Parse `alias name='command'` / `alias name=command` lines; ignore everything else.
    pub fn parse_shell_aliases(text: &str, origin: AliasOrigin) -> Vec<AliasEntry> {
        let mut out = Vec::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some(rest) = line.strip_prefix("alias") else {
                continue;
            };
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let rest = rest.trim_start();
            let Some((name, raw_val)) = rest.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_whitespace) {
                continue;
            }
            let expansion = parse_shell_value(raw_val.trim_start());
            if expansion.is_empty() {
                continue;
            }
            out.push(AliasEntry {
                name: name.to_string(),
                expansion,
                origin: origin.clone(),
            });
        }
        out
    }

    /// Parse the `[aliases]` table of `aliases.toml`.
    pub fn parse_user_config(
        text: &str,
        origin: AliasOrigin,
    ) -> Result<Vec<AliasEntry>, AliasError> {
        #[derive(serde::Deserialize)]
        struct UserAliasesFile {
            #[serde(default = "default_version")]
            version: u32,
            #[serde(default)]
            aliases: BTreeMap<String, String>,
        }
        fn default_version() -> u32 {
            1
        }

        let parsed: UserAliasesFile = toml::from_str(text).map_err(|e| AliasError::Parse {
            line: 1,
            message: e.to_string(),
        })?;
        if parsed.version != 1 {
            return Err(AliasError::Parse {
                line: 1,
                message: format!("unsupported aliases.toml version: {}", parsed.version),
            });
        }
        Ok(parsed
            .aliases
            .into_iter()
            .filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty())
            .map(|(name, expansion)| AliasEntry {
                name,
                expansion,
                origin: origin.clone(),
            })
            .collect())
    }

    pub fn insert(&mut self, entry: AliasEntry) {
        self.entries.insert(entry.name.clone(), entry);
    }

    pub fn get(&self, name: &str) -> Option<&AliasEntry> {
        self.entries.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &AliasEntry> + '_ {
        self.entries.values()
    }
}

fn parse_shell_value(raw: &str) -> String {
    if let Some(after_quote) = raw.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = after_quote.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\'' {
                // Handle bash/zsh idiom '\'' inside single quotes.
                if after_quote.contains("'\\''") {
                    // Check if immediately followed by \''
                    let rem: String = chars.clone().collect();
                    if let Some(rest) = rem.strip_prefix("\\''") {
                        out.push('\'');
                        let _ = rest;
                        chars.next();
                        chars.next();
                        chars.next();
                        continue;
                    }
                }
                break;
            }
            out.push(ch);
        }
        return out;
    }

    if let Some(after_quote) = raw.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = after_quote.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '"' => break,
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                _ => out.push(ch),
            }
        }
        return out;
    }

    // Unquoted value: stop at unescaped whitespace or inline `#` comment.
    let mut out = String::new();
    let mut escaped = false;
    for ch in raw.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch.is_whitespace() || ch == '#' {
            break;
        }
        out.push(ch);
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum AliasError {
    #[error("alias file io error: {0}")]
    Io(String),
    #[error("alias parse error at line {line}: {message}")]
    Parse { line: usize, message: String },
}
