//! Safety analysis and the no-silent-execution gate.
//!
//! Guarantee: a candidate whose [`CandidateSource`] is not exact-layer is never
//! executed from a single Enter without showing its rendered command. Dangerous
//! commands (even exact ones) require a second, explicit confirmation.
//!
//! Detection is lexical (shell-word split + regex), deliberately not a shell
//! parser. False positives are acceptable; false negatives on the built-in list
//! are not.

use crate::config::SafetySettings;
use crate::contracts::{
    Candidate, CandidateSource, CwdContext, RiskLevel, RiskReason, SafetyReport,
};
use crate::input::placeholder::Template;
use regex::Regex;

/// Compiled, reusable policy. Built once at startup and shared by preview + gate.
#[derive(Clone, Debug)]
pub struct SafetyPolicy {
    pub blocked_patterns: Vec<String>,
    pub system_paths: Vec<String>,
    /// `blocked_patterns` compiled once; call [`SafetyPolicy::recompile`] after
    /// mutating the pattern list directly.
    blocked_regexes: Vec<Regex>,
}

impl Default for SafetyPolicy {
    fn default() -> Self {
        let mut policy = Self {
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
            blocked_regexes: Vec::new(),
        };
        policy.recompile();
        policy
    }
}

impl SafetyPolicy {
    pub fn from_settings(settings: &SafetySettings) -> Self {
        let mut policy = Self {
            blocked_patterns: settings.blocked_patterns.clone(),
            system_paths: settings.system_paths.clone(),
            blocked_regexes: Vec::new(),
        };
        policy.recompile();
        policy
    }

    /// Compiles `blocked_patterns`; invalid patterns are skipped.
    pub fn recompile(&mut self) {
        self.blocked_regexes = self
            .blocked_patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();
    }
}

fn is_raw_device(path: &str) -> bool {
    if !path.starts_with("/dev/") {
        return false;
    }
    !matches!(
        path,
        "/dev/null" | "/dev/zero" | "/dev/stdin" | "/dev/stdout" | "/dev/stderr" | "/dev/tty"
    )
}

fn is_system_path(path: &str, system_paths: &[String]) -> bool {
    let clean = path.trim_end_matches('/');
    let norm = if clean.is_empty() && path.starts_with('/') {
        "/"
    } else {
        clean
    };
    for sys in system_paths {
        let sys_norm = sys.trim_end_matches('/');
        let sys_norm = if sys_norm.is_empty() && sys.starts_with('/') {
            "/"
        } else {
            sys_norm
        };
        if norm == sys_norm {
            return true;
        }
        if sys_norm != "/" && sys_norm != "/home" {
            if norm.starts_with(&format!("{sys_norm}/")) {
                return true;
            }
        }
    }
    false
}

fn push_unique(reasons: &mut Vec<RiskReason>, reason: RiskReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn split_segments(command: &str) -> Vec<Vec<String>> {
    // A newline terminates a command just like `;`, so split on it first.
    if command.contains(['\n', '\r']) {
        return command
            .split(['\n', '\r'])
            .flat_map(split_segments)
            .collect();
    }
    let words = match shlex::split(command) {
        Some(w) => w,
        None => command.split_whitespace().map(ToOwned::to_owned).collect(),
    };
    let mut segments = Vec::new();
    let mut cur = Vec::new();
    for w in words {
        if matches!(w.as_str(), ";" | "&&" | "||" | "|") {
            if !cur.is_empty() {
                segments.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(w);
        }
    }
    if !cur.is_empty() {
        segments.push(cur);
    }
    segments
}

/// Full lexical analysis: destructive verbs, `sudo`, system-path writes,
/// raw-device writes, recursive deletes, unbalanced quotes, empty placeholders.
pub fn analyze(command: &str, policy: &SafetyPolicy, _ctx: &CwdContext) -> SafetyReport {
    let mut reasons = Vec::new();
    let mut max_level = RiskLevel::Safe;

    let mut bump = |lvl: RiskLevel| {
        if lvl > max_level {
            max_level = lvl;
        }
    };

    if let Ok(tpl) = Template::parse(command) {
        if tpl.has_slots() && !tpl.is_complete() {
            push_unique(&mut reasons, RiskReason::EmptyPlaceholder);
            bump(RiskLevel::Warn);
        }
    } else if command.contains("{{") {
        push_unique(&mut reasons, RiskReason::EmptyPlaceholder);
        bump(RiskLevel::Warn);
    }

    if shlex::split(command).is_none() {
        push_unique(&mut reasons, RiskReason::UnbalancedQuotes);
        bump(RiskLevel::Warn);
    }

    // Check explicit blocked regex patterns (precompiled in `SafetyPolicy`).
    for re in &policy.blocked_regexes {
        if re.is_match(command) {
            push_unique(
                &mut reasons,
                RiskReason::DestructiveVerb {
                    verb: re.as_str().to_string(),
                    target: command.trim().to_string(),
                },
            );
            bump(RiskLevel::Dangerous);
        }
    }

    // Check raw redirections like `> /etc/passwd`, `2>/dev/sda`, `&>...`, `>|...`.
    let raw_tokens: Vec<&str> = command.split_whitespace().collect();
    for (idx, tok) in raw_tokens.iter().enumerate() {
        // Strip an optional fd number and `&` so every redirect form is seen.
        let t = tok.strip_prefix('&').unwrap_or(*tok);
        let t = t.trim_start_matches(|c: char| c.is_ascii_digit());
        if let Some(dup) = t.strip_prefix(">&") {
            if !dup.is_empty() && dup.chars().all(|c| c.is_ascii_digit()) {
                continue; // fd duplication such as `2>&1`: no file write.
            }
        }
        let target = if let Some(rest) = t.strip_prefix(">>") {
            if rest.is_empty() {
                raw_tokens.get(idx + 1).copied()
            } else {
                Some(rest)
            }
        } else if let Some(rest) = t.strip_prefix(">|") {
            if rest.is_empty() {
                raw_tokens.get(idx + 1).copied()
            } else {
                Some(rest)
            }
        } else if let Some(rest) = t.strip_prefix('>') {
            if rest.is_empty() {
                raw_tokens.get(idx + 1).copied()
            } else {
                Some(rest)
            }
        } else {
            None
        };
        if let Some(t) = target {
            if is_raw_device(t) {
                push_unique(
                    &mut reasons,
                    RiskReason::RawDeviceWrite {
                        path: t.to_string(),
                    },
                );
                bump(RiskLevel::Dangerous);
            } else if is_system_path(t, &policy.system_paths) {
                push_unique(
                    &mut reasons,
                    RiskReason::SystemPathWrite {
                        path: t.to_string(),
                    },
                );
                bump(RiskLevel::Dangerous);
            }
        }
    }

    for seg in split_segments(command) {
        let mut idx = 0;
        while idx < seg.len() {
            let tok = seg[idx].as_str();
            if matches!(tok, "sudo" | "doas" | "su" | "pkexec") {
                push_unique(&mut reasons, RiskReason::PrivilegeEscalation);
                bump(RiskLevel::Warn);
                idx += 1;
                continue;
            }
            break;
        }
        let Some(cmd) = seg.get(idx).map(String::as_str) else {
            continue;
        };
        let args = &seg[idx + 1..];
        let cmd_base = cmd.rsplit('/').next().unwrap_or(cmd);

        if cmd_base == "rm" {
            let recursive = args.iter().any(|a| {
                a == "--recursive"
                    || (a.starts_with('-')
                        && !a.starts_with("--")
                        && (a.contains('r') || a.contains('R')))
            });
            if recursive {
                push_unique(&mut reasons, RiskReason::RecursiveDelete);
                bump(RiskLevel::Dangerous);
            }
            for arg in args.iter().filter(|a| !a.starts_with('-')) {
                if is_system_path(arg, &policy.system_paths) {
                    push_unique(
                        &mut reasons,
                        RiskReason::SystemPathWrite { path: arg.clone() },
                    );
                    bump(RiskLevel::Dangerous);
                } else if recursive {
                    push_unique(
                        &mut reasons,
                        RiskReason::DestructiveVerb {
                            verb: "rm".into(),
                            target: arg.clone(),
                        },
                    );
                    bump(RiskLevel::Dangerous);
                }
            }
        } else if cmd_base == "dd" {
            let mut target = String::new();
            for arg in args {
                if let Some(of_val) = arg.strip_prefix("of=") {
                    target = of_val.to_string();
                    if is_raw_device(of_val) {
                        push_unique(
                            &mut reasons,
                            RiskReason::RawDeviceWrite {
                                path: of_val.to_string(),
                            },
                        );
                    } else if is_system_path(of_val, &policy.system_paths) {
                        push_unique(
                            &mut reasons,
                            RiskReason::SystemPathWrite {
                                path: of_val.to_string(),
                            },
                        );
                    }
                }
            }
            push_unique(
                &mut reasons,
                RiskReason::DestructiveVerb {
                    verb: "dd".into(),
                    target,
                },
            );
            bump(RiskLevel::Dangerous);
        } else if cmd_base.starts_with("mkfs")
            || matches!(cmd_base, "wipefs" | "fdisk" | "parted" | "shred")
        {
            let target = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .cloned()
                .unwrap_or_default();
            if is_raw_device(&target) {
                push_unique(
                    &mut reasons,
                    RiskReason::RawDeviceWrite {
                        path: target.clone(),
                    },
                );
            }
            push_unique(
                &mut reasons,
                RiskReason::DestructiveVerb {
                    verb: cmd_base.to_string(),
                    target,
                },
            );
            bump(RiskLevel::Dangerous);
        } else if matches!(
            cmd_base,
            "tee" | "cp" | "mv" | "chmod" | "chown" | "install" | "ln"
        ) {
            for arg in args.iter().filter(|a| !a.starts_with('-')) {
                if is_raw_device(arg) {
                    push_unique(
                        &mut reasons,
                        RiskReason::RawDeviceWrite { path: arg.clone() },
                    );
                    bump(RiskLevel::Dangerous);
                } else if is_system_path(arg, &policy.system_paths) {
                    push_unique(
                        &mut reasons,
                        RiskReason::SystemPathWrite { path: arg.clone() },
                    );
                    bump(RiskLevel::Dangerous);
                }
            }
        }
    }

    SafetyReport {
        level: max_level,
        reasons,
    }
}

/// Convenience wrapper for the overlay preview (one-line explanation per reason).
pub fn explain(report: &SafetyReport) -> Vec<String> {
    report
        .reasons
        .iter()
        .map(|r| match r {
            RiskReason::DestructiveVerb { verb, target } => {
                if target.is_empty() {
                    format!("Destructive command: {verb}")
                } else {
                    format!("Destructive command `{verb}` on `{target}`")
                }
            }
            RiskReason::PrivilegeEscalation => "Runs with elevated privileges (sudo/su)".into(),
            RiskReason::SystemPathWrite { path } => format!("Modifies system path: {path}"),
            RiskReason::RawDeviceWrite { path } => format!("Writes raw device: {path}"),
            RiskReason::RecursiveDelete => "Recursive deletion (rm -r/-rf)".into(),
            RiskReason::UnbalancedQuotes => "Unbalanced quotes in command".into(),
            RiskReason::EmptyPlaceholder => "Contains unfilled {{arg}} placeholder".into(),
        })
        .collect()
}

/// A command that passed analysis and is waiting for (or has just received) Enter.
#[derive(Clone, Debug)]
pub struct PendingCommand {
    pub rendered: String,
    #[allow(dead_code)] // Shortcut: источник показывается в островке, разводка в v2.
    pub source: CandidateSource,
    pub report: SafetyReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageOutcome {
    /// Exact + Safe: the caller may execute immediately.
    Executable,
    /// Overlay is shown; Enter is required.
    AwaitingUser,
    /// Exact + Dangerous with `confirm_dangerous`: overlay + typed confirm required.
    AwaitingDangerConfirm,
}

/// Single source of truth for "may this run now?".
pub struct ConfirmGate {
    pending: Option<PendingCommand>,
    confirm_dangerous: bool,
}

impl ConfirmGate {
    pub fn new(confirm_dangerous: bool) -> Self {
        Self {
            pending: None,
            confirm_dangerous,
        }
    }

    /// Stage a candidate. Never executes anything by itself.
    pub fn stage(&mut self, candidate: &Candidate) -> StageOutcome {
        let pending = PendingCommand {
            rendered: candidate.rendered.clone(),
            source: candidate.source,
            report: candidate.safety.clone(),
        };
        let outcome = if candidate.safety.level == RiskLevel::Dangerous && self.confirm_dangerous {
            StageOutcome::AwaitingDangerConfirm
        } else if candidate.source.may_execute_directly()
            && candidate.safety.level == RiskLevel::Safe
        {
            StageOutcome::Executable
        } else {
            StageOutcome::AwaitingUser
        };
        self.pending = Some(pending);
        outcome
    }

    /// Enter pressed on a staged candidate. `None` while a dangerous confirm is
    /// still required; the caller must then call [`ConfirmGate::force`].
    pub fn confirm(&mut self) -> Option<PendingCommand> {
        if self.requires_second_confirm() {
            return None;
        }
        self.pending.take()
    }

    /// Second explicit confirm (Ctrl+Enter) for a Dangerous command.
    pub fn force(&mut self) -> Option<PendingCommand> {
        self.pending.take()
    }

    pub fn cancel(&mut self) {
        self.pending = None;
    }

    pub fn pending(&self) -> Option<&PendingCommand> {
        self.pending.as_ref()
    }

    pub fn requires_second_confirm(&self) -> bool {
        self.confirm_dangerous
            && self
                .pending
                .as_ref()
                .is_some_and(|p| p.report.level == RiskLevel::Dangerous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(rendered: &str, source: CandidateSource, policy: &SafetyPolicy) -> Candidate {
        Candidate {
            id: 1,
            template: rendered.to_string(),
            rendered: rendered.to_string(),
            source,
            score: 1.0,
            safety: analyze(rendered, policy, &CwdContext::default()),
            note: None,
        }
    }

    #[test]
    fn flags_dangerous_and_warning_commands() {
        let policy = SafetyPolicy::default();

        let r_rm = analyze("rm -rf ./build", &policy, &CwdContext::default());
        assert_eq!(r_rm.level, RiskLevel::Dangerous);
        assert!(r_rm.reasons.contains(&RiskReason::RecursiveDelete));

        let r_dd = analyze(
            "dd if=img.iso of=/dev/sda bs=4M",
            &policy,
            &CwdContext::default(),
        );
        assert_eq!(r_dd.level, RiskLevel::Dangerous);
        assert!(r_dd.reasons.contains(&RiskReason::RawDeviceWrite {
            path: "/dev/sda".into()
        }));

        let r_mkfs = analyze("mkfs.ext4 /dev/sdb1", &policy, &CwdContext::default());
        assert_eq!(r_mkfs.level, RiskLevel::Dangerous);

        let r_sudo = analyze("sudo apt update", &policy, &CwdContext::default());
        assert_eq!(r_sudo.level, RiskLevel::Warn);
        assert!(r_sudo.reasons.contains(&RiskReason::PrivilegeEscalation));

        let r_sys = analyze("echo bad > /etc/hosts", &policy, &CwdContext::default());
        assert_eq!(r_sys.level, RiskLevel::Dangerous);
        assert!(r_sys.reasons.contains(&RiskReason::SystemPathWrite {
            path: "/etc/hosts".into()
        }));

        let r_safe = analyze("ls -la ./src", &policy, &CwdContext::default());
        assert_eq!(r_safe.level, RiskLevel::Safe);
    }

    #[test]
    fn confirm_gate_blocks_generated_and_requires_second_confirm_for_danger() {
        let policy = SafetyPolicy::default();
        let mut gate = ConfirmGate::new(true);

        // Exact + Safe -> Executable immediately.
        let safe_exact = cand("ls -la", CandidateSource::Exact, &policy);
        assert_eq!(gate.stage(&safe_exact), StageOutcome::Executable);
        assert!(gate.confirm().is_some());

        // Generated + Safe -> AwaitingUser (cannot execute silently without Enter).
        let safe_llm = cand("du -sh *", CandidateSource::Llm, &policy);
        assert_eq!(gate.stage(&safe_llm), StageOutcome::AwaitingUser);
        assert!(gate.confirm().is_some());

        // Dangerous -> requires force() after confirm() returns None.
        let danger_exact = cand("rm -rf /etc/foo", CandidateSource::Exact, &policy);
        assert_eq!(
            gate.stage(&danger_exact),
            StageOutcome::AwaitingDangerConfirm
        );
        assert!(gate.requires_second_confirm());
        assert!(gate.confirm().is_none());
        assert_eq!(gate.force().unwrap().rendered, "rm -rf /etc/foo");
    }
}
