//! Layer 1: exact argv[0] lookup in `$PATH`, shell builtins, and aliases.
//!
//! The first whitespace-delimited token is checked, so `git status` resolves via
//! `git`; a single-token exact match is returned directly without an overlay.
//! Alias expansions become templates and are shown in the overlay before running
//! unless the alias is single-token and Safe.

use crate::aliases::AliasTable;
use crate::contracts::{
    Candidate, CandidateSource, CmdId, CommandResolver, ResolveError, ResolveRequest, SafetyReport,
};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

fn cmd_id(text: &str) -> CmdId {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            return (meta.permissions().mode() & 0o111) != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub struct ExactResolver {
    aliases: AliasTable,
    path_dirs: Vec<PathBuf>,
    /// Cached basenames of executable files in `path_dirs`, rebuilt on `refresh`.
    path_commands: HashSet<String>,
    builtins: HashSet<String>,
}

impl ExactResolver {
    pub fn new(aliases: AliasTable, path_dirs: Vec<PathBuf>) -> Self {
        let builtins = BUILTINS.iter().map(|s| (*s).to_string()).collect();
        let mut resolver = Self {
            aliases,
            path_dirs,
            path_commands: HashSet::new(),
            builtins,
        };
        resolver.refresh();
        resolver
    }

    /// Re-scan `$PATH` plus the builtin list (bash/zsh core set).
    pub fn refresh(&mut self) {
        self.path_commands.clear();
        for dir in &self.path_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if is_executable_file(&path) {
                    if let Some(name) = entry.file_name().to_str() {
                        self.path_commands.insert(name.to_string());
                    }
                }
            }
        }
    }

    /// Expands an alias into a candidate whose template may carry the user's args.
    pub fn alias_candidate(&self, name: &str, rest: &str) -> Option<Candidate> {
        let entry = self.aliases.get(name)?;
        let rendered = if rest.trim().is_empty() {
            entry.expansion.clone()
        } else {
            format!("{} {}", entry.expansion, rest.trim())
        };
        Some(Candidate {
            id: cmd_id(&rendered),
            template: rendered.clone(),
            rendered,
            source: CandidateSource::Alias,
            score: 1.0,
            safety: SafetyReport::default(),
            note: Some(format!("alias: {} = {}", entry.name, entry.expansion)),
        })
    }

    /// `Some` only for `argv[0]` present in PATH or builtins.
    pub fn exact_candidate(&self, argv0: &str) -> Option<Candidate> {
        if argv0.is_empty() {
            return None;
        }
        let source = if argv0.contains('/') {
            if is_executable_file(Path::new(argv0)) {
                CandidateSource::Exact
            } else {
                return None;
            }
        } else if self.is_builtin(argv0) {
            CandidateSource::Builtin
        } else if self.path_commands.contains(argv0) {
            CandidateSource::Exact
        } else {
            return None;
        };

        Some(Candidate {
            id: cmd_id(argv0),
            template: argv0.to_string(),
            rendered: argv0.to_string(),
            source,
            score: 1.0,
            safety: SafetyReport::default(),
            note: None,
        })
    }

    pub fn is_builtin(&self, name: &str) -> bool {
        self.builtins.contains(name)
    }
}

impl CommandResolver for ExactResolver {
    fn id(&self) -> &'static str {
        "exact"
    }

    fn priority(&self) -> u8 {
        10
    }

    fn resolve(&mut self, req: &ResolveRequest) -> Result<Vec<Candidate>, ResolveError> {
        let (argv0, rest) = split_argv0(&req.raw_input);
        if argv0.is_empty() {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        if let Some(alias_cand) = self.alias_candidate(argv0, rest) {
            out.push(alias_cand);
        }
        if let Some(mut exact_cand) = self.exact_candidate(argv0) {
            if !rest.is_empty() {
                let full = format!("{argv0} {rest}");
                exact_cand.id = cmd_id(&full);
                exact_cand.template = full.clone();
                exact_cand.rendered = full;
            }
            out.push(exact_cand);
        }
        Ok(out)
    }

    fn resolve_live(&mut self, req: &ResolveRequest) -> Vec<Candidate> {
        let trimmed = req.raw_input.trim();
        if trimmed.len() < 2 || trimmed.contains(char::is_whitespace) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for entry in self.aliases.iter() {
            if entry.name.starts_with(trimmed) {
                out.push(Candidate {
                    id: cmd_id(&entry.expansion),
                    template: entry.expansion.clone(),
                    rendered: entry.expansion.clone(),
                    source: CandidateSource::Alias,
                    score: if entry.name == trimmed { 0.96 } else { 0.85 },
                    safety: SafetyReport::default(),
                    note: Some(format!("alias: {}", entry.name)),
                });
            }
        }
        let mut cmds: Vec<&String> = self
            .builtins
            .iter()
            .chain(self.path_commands.iter())
            .filter(|c| c.starts_with(trimmed) && c.as_str() != trimmed)
            .collect();
        cmds.sort();
        cmds.dedup();
        for cmd in cmds.into_iter().take(req.limit.max(1)) {
            let source = if self.builtins.contains(cmd) {
                CandidateSource::Builtin
            } else {
                CandidateSource::Exact
            };
            out.push(Candidate {
                id: cmd_id(cmd),
                template: cmd.clone(),
                rendered: cmd.clone(),
                source,
                score: 0.72,
                safety: SafetyReport::default(),
                note: None,
            });
        }
        out
    }
}

/// Builtin names shared by bash and zsh; used for direct commands and safety notes.
pub const BUILTINS: &[&str] = &[
    "cd", "echo", "export", "alias", "unalias", "set", "unset", "source", "exit", "return", "type",
    "command", "pwd", "read", "printf", "test", "true", "false", "eval", "exec", "jobs", "fg",
    "bg", "wait", "history", "umask", "shift", "break", "continue",
];

/// Splits a raw input line into `(argv0, rest)` on the first whitespace run.
pub fn split_argv0(input: &str) -> (&str, &str) {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return ("", "");
    }
    match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim_start()),
        None => (trimmed, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aliases::AliasOrigin;
    use crate::contracts::{CwdContext, Trigger};
    use crate::resolver::ResolverChain;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn splits_argv0_and_rest() {
        assert_eq!(split_argv0("   git   status -s  "), ("git", "status -s"));
        assert_eq!(split_argv0("ls"), ("ls", ""));
        assert_eq!(split_argv0("   "), ("", ""));
    }

    #[test]
    fn parses_rc_aliases_without_executing_and_user_config_wins() {
        let dir = tempdir().unwrap();
        let bashrc = dir.path().join(".bashrc");
        let zshrc = dir.path().join(".zshrc");
        let user = dir.path().join("aliases.toml");

        std::fs::write(
            &bashrc,
            "$(touch should_never_run)\nalias ll='ls -la'\nalias gs='git status'\n",
        )
        .unwrap();
        std::fs::write(&zshrc, "alias ll='ls -lah'\nalias k=kubectl\n").unwrap();
        std::fs::write(&user, "version = 1\n[aliases]\nll = \"eza -la\"\n").unwrap();

        let table = AliasTable::load(&[zshrc, bashrc], &user).unwrap();
        assert!(!dir.path().join("should_never_run").exists());
        assert_eq!(table.get("ll").unwrap().expansion, "eza -la");
        assert!(matches!(
            table.get("ll").unwrap().origin,
            AliasOrigin::UserConfig(_)
        ));
        assert_eq!(table.get("k").unwrap().expansion, "kubectl");
        assert_eq!(table.get("gs").unwrap().expansion, "git status");
    }

    #[test]
    fn resolves_path_executables_builtins_and_aliases() {
        let dir = tempdir().unwrap();
        let bin_file = dir.path().join("mytool");
        std::fs::write(&bin_file, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin_file).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin_file, perms).unwrap();
        }

        let mut aliases = AliasTable::default();
        aliases.insert(crate::aliases::AliasEntry {
            name: "mt".into(),
            expansion: "mytool --fast".into(),
            origin: AliasOrigin::UserConfig(dir.path().join("aliases.toml")),
        });

        let mut resolver = ExactResolver::new(aliases, vec![dir.path().to_path_buf()]);
        assert_eq!(
            resolver.exact_candidate("mytool").unwrap().source,
            CandidateSource::Exact
        );
        assert_eq!(
            resolver.exact_candidate("cd").unwrap().source,
            CandidateSource::Builtin
        );
        assert!(resolver.exact_candidate("nonexistent_cmd_xyz").is_none());

        let req = ResolveRequest {
            raw_input: "mt --dry-run".into(),
            context: CwdContext::default(),
            history_tail: Vec::new(),
            trigger: Trigger::AfterMiss,
            limit: 8,
        };
        let cands = resolver.resolve(&req).unwrap();
        assert_eq!(cands[0].source, CandidateSource::Alias);
        assert_eq!(cands[0].rendered, "mytool --fast --dry-run");
    }

    #[test]
    fn resolver_chain_exact_hit_never_calls_later_stages() {
        struct SentinelStage(Arc<AtomicBool>);
        impl CommandResolver for SentinelStage {
            fn id(&self) -> &'static str {
                "sentinel"
            }
            fn priority(&self) -> u8 {
                20
            }
            fn resolve(&mut self, _req: &ResolveRequest) -> Result<Vec<Candidate>, ResolveError> {
                self.0.store(true, Ordering::SeqCst);
                Ok(Vec::new())
            }
        }

        let called = Arc::new(AtomicBool::new(false));
        let mut chain = ResolverChain::from_stages(
            vec![
                Box::new(ExactResolver::new(AliasTable::default(), Vec::new())),
                Box::new(SentinelStage(Arc::clone(&called))),
            ],
            8,
        );

        let req = ResolveRequest {
            raw_input: "echo hello".into(),
            context: CwdContext::default(),
            history_tail: Vec::new(),
            trigger: Trigger::AfterMiss,
            limit: 8,
        };
        let hit = chain.exact_hit(&req).unwrap();
        assert_eq!(hit.source, CandidateSource::Builtin);
        assert!(hit.source.may_execute_directly());
        assert!(!called.load(Ordering::SeqCst));
    }
}
