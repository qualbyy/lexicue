# Lexicue

Linux-first GUI terminal with intent-based command resolution (`??` / `Alt+Space`), passthrough TUI guard, and floating command island.

## Build (Linux)

System dependencies for `eframe` (Wayland / X11 / OpenGL):
- Debian/Ubuntu: `libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev libgtk-3-dev`
- Arch Linux: `libxkbcommon libxcb openssl gtk3`

```bash
cargo build --release
```

Binary output: `target/release/lexicue`.

## CLI Usage

```text
lexicue [OPTIONS] [-e <COMMAND> [ARGS...]]
  -d, --working-dir <DIR>   Start shell in DIR
      --config <PATH>       Use custom config.toml
  -e, -- <COMMAND> [ARGS]   Run COMMAND instead of default shell
  -h, --help                Print help
  -V, --version             Print version
```

## Keybindings

- `Ctrl+Shift+C` / `Ctrl+Shift+V` — Copy / Paste (middle-click pastes primary selection)
- `Ctrl+Shift+F` — Search scrollback and screen
- `Shift+PageUp` / `Shift+PageDown` — Scroll history
- `Alt+Space` or `?? <query>` — Intent command resolver
- `Ctrl+L` (or island bar) — Quick launcher / settings

## Desktop Integration

```bash
install -Dm755 target/release/lexicue ~/.local/bin/lexicue
install -Dm644 assets/lexicue.desktop ~/.local/share/applications/lexicue.desktop
```

## Configuration

See [`config.example.toml`](config.example.toml). Default path: `~/.config/lexicue/config.toml`.
