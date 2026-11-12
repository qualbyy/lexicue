# Lexicue — Architecture & Implementation Route (v2 Dynamic Island)

Linux-first terminal GUI with exact + approximate command resolution and adaptive Dynamic Island.
One binary crate, clean modules, no VT/PTY/GUI engine written from scratch.

## Non-negotiable invariants

1. **Passthrough first.** Inside TUIs (alternate screen, raw mode) or during password
   prompts (ECHO off) no input is intercepted — every byte goes to the PTY, and the
   overlay/launcher is force-closed. Any positive signal ⇒ `InterceptDecision::Passthrough`.
2. **No silent execution.** Inexact/generated candidates never run from raw Enter.
   The overlay preview is mandatory; `Dangerous` needs a second explicit confirm.
3. **`$0` first.** exact PATH/builtin/alias → history/dictionary → cache → LLM.
   Any local/cache hit means zero network calls.
4. **Context budget.** Resolvers receive `cwd` + project markers (≤ 30 bytes), never
   the screen buffer.
5. **Adapter firewall.** `portable-pty` only in `src/pty/portable.rs`, `vt100` only in
   `src/pty/vt.rs`, `egui` only in `src/ui/*` + `src/app.rs`, `src/telemetry.rs` pure, `ureq` only in
   `src/resolver/llm.rs`.
6. **YAGNI.** No tabs, splits, plugin runtime, SSH multiplexing, or
   custom font rasterizer. Extension seams are `ThemePalette`,
   `CommandResolver`, and the cache schema.

## Module map

```
src/
├── main.rs              wire modules, parse --config, run eframe
├── app.rs               LexicueApp state machine (tick → draw → input → island actions)
├── config.rs            Config + sections ([island]), XDG load with legacy fallback
├── theme.rs             Rgb + ThemePalette (from config hex)
├── aliases.rs           rc import (read-only) + user aliases.toml
├── contracts.rs         shared types, CommandResolver, guard enums, IslandAction, KeyInput/UiIntent
├── telemetry.rs         pure CommandTracker, PortScanner, EnvContext, snippets_for
├── pty/
│   ├── mod.rs           PtyBackend / ScreenModel traits, GridSnapshot
│   ├── portable.rs      portable-pty adapter (pgid tracking)
│   ├── vt.rs            vt100 adapter (history tracking + capture_since)
│   ├── markers.rs       OSC 133 semantic prompt parser
│   └── guard.rs         PassthroughGuard (mode detection + decision)
├── resolver/
│   ├── mod.rs           ResolverChain (order, merge, dedup, cap)
│   ├── exact.rs         $PATH + builtins + aliases
│   ├── local.rs         BM25 + fuzzy over history/dictionary + context bonus
│   ├── cache.rs         JSONL cache ($0 repeat + legacy migration)
│   └── llm.rs           Ollama/DeepSeek + secret masking + cache write
├── input/
│   ├── placeholder.rs   {{arg}} Template / Slot
│   └── safety.rs        SafetyPolicy, analyze, ConfirmGate
└── ui/
    ├── mod.rs           draw root, Dynamic Island (collapsed/expanded/context/launcher)
    ├── grid.rs          fixed-cell monospace grid
    └── overlay.rs       candidates + preview + risk badge + slot editing
```

## Implementation route

Each step is self-contained, must compile with `todo!()` removed only in touched files,
and is verified by exactly one command. Do not start step N+1 before step N's command passes.

| Step | Target Files | Done Criteria | Verification Command |
|---|---|---|---|
| 1 | `Cargo.toml`, `src/main.rs`, `src/config.rs`, `src/theme.rs`, `src/pty/mod.rs`, `src/pty/portable.rs`, `src/pty/vt.rs`, `src/app.rs`, `src/ui/mod.rs`, `src/ui/grid.rs` | `cargo check` is clean. `cargo run -- --config <abs>/config.example.toml` opens one 1000×640 window, spawns `$SHELL` in `$PWD`, `echo hello` appears in the grid, window close kills the child. No resolver/overlay/guard bodies yet. | `cargo check` |
| 2 | `src/ui/mod.rs` (`translate_input`), `src/app.rs` (`tick`, `on_key`, `update`), `src/pty/portable.rs` (`resize`) | Typing, Enter, Backspace, arrows, Ctrl+C, and paste reach the shell; window resize updates PTY `cols/rows` and the grid; unit tests cover char/enter/tab/ctrl/paste translation. | `cargo test ui::` |
| 3 | `src/pty/guard.rs`, `src/pty/vt.rs` (`terminal_mode`, title), `src/app.rs` (guard wiring) | `vim`/`htop`/`less`/`ssh` force passthrough; `sudo` password line (ECHO off) forces passthrough; overlay closes on transition. Unit tests cover `is_tui_title`, `looks_like_password_prompt`, latch/reset. | `cargo test pty::guard` |
| 4 | `src/aliases.rs`, `src/resolver/mod.rs`, `src/resolver/exact.rs`, `src/config.rs` | Exact `argv[0]` hit in `$PATH`/builtins returns `Exact`; single-token input runs directly; aliases from bashrc/zshrc parsed without executing them; `aliases.toml` overrides imports; `ResolverChain::exact_hit` never calls later stages. | `cargo test resolver::exact` |
| 5 | `src/resolver/local.rs`, `src/ui/overlay.rs`, `src/app.rs` (`OpenCandidates`), `src/ui/mod.rs` (`draw`) | History + optional tldr index answers description queries via BM25+fuzzy; project markers add `history_bonus`; `??` or an L1 miss opens the overlay; Esc closes; selection/scroll capped at `max_candidates`. | `cargo test resolver::local` |
| 6 | `src/input/placeholder.rs`, `src/input/safety.rs`, `src/ui/overlay.rs` (preview + slots), `src/app.rs` (`accept_selected`) | `{{arg}}` slots editable via Tab; preview always shows rendered command; `rm -rf`, `dd`, `mkfs`, `sudo`, system-path writes flagged; generated candidates cannot execute without Enter; Dangerous requires second confirm; exact only runs directly when Safe. | `cargo test input::` |
| 7 | `src/resolver/llm.rs`, `src/resolver/cache.rs`, `src/resolver/mod.rs`, `src/app.rs` | Explicit `??` calls Ollama or DeepSeek with masked prompt; provider failure degrades to an overlay error (no panic); every LLM result persisted; identical second query resolves from `Cache` with zero network; atomic JSONL rewrite; `cargo test resolver::` includes cache key normalization + masking tests. | `cargo test resolver::` |

## Config schema

Path: `$XDG_CONFIG_HOME/termby/config.toml` (fallback `~/.config/termby/config.toml`).
Full example: `config.example.toml`. Sections: `[general]`, `[resolver]`, `[llm]`,
`[cache]`, `[guard]`, `[safety]`, `[theme]`. Hex colors are parsed once into
`theme::ThemePalette`; invalid hex is a startup error, not a fallback.

## Alias schema (`~/.config/termby/aliases.toml`)

```toml
version = 1
[aliases]
ll = "ls -la"
gst = "git status"
```

Precedence: user file > zshrc > bashrc. rc files are parsed as text
(`alias name='expansion'`); they are never sourced or executed. Paths in `[resolver]`
are reloaded on startup only in v1.

## Cache schema (`~/.cache/termby/cache.jsonl`)

One JSON object per line, append/atomic-rewrite via `prune`:

```json
{"key":"<sha256>","query":"show big files","template":"du -ah . | sort -rh | head -20","source":"llm","provider":"ollama","model":"qwen2.5-coder:3b","created_at":1730000000,"hits":2,"risk":"safe"}
```

- `key = sha256(lower(trim(query)) + "|" + marker_class + "|" + provider + ":" + model)`
- `marker_class` = sorted project-marker names (empty ⇒ `<none>`), so identical intents
  in the same repo type reuse an entry while different repos do not collide.
- Cache hits produce `CandidateSource::Cache`; they are displayed through the same
  overlay/confirm path as LLM candidates (never executed silently).
- Secrets are masked before the key/prompt is built and never stored.
- `prune(max_entries)` keeps most recently used; corrupt lines are skipped.

## Verification conventions

- Run from the crate root with full paths for config when needed.
- `cargo test resolver::` is the final regression for steps 4–7.
- Any change to a contract type requires updating the dependent `todo!()` signatures
  before running `cargo check`; do not add new crates or bump versions.
