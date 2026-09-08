# rterm — Agent Instructions

## Quick commands

```bash
cargo run                  # debug build & launch
cargo build --release      # release binary at target/release/rterm
cargo fmt                  # auto-format workspace (rustfmt default style)
cargo fmt --check          # verify formatting (not enforced by CI)
cargo clippy --workspace --all-targets -- -D warnings  # CI lint gate
cargo test                 # all unit tests (no external test harness)
```

After finishing code changes, run in order: `cargo fmt`, then
`cargo clippy --workspace --all-targets -- -D warnings`, then `cargo test`,
and verify `cargo fmt --check` is clean before committing.

## Workspace layout

```
crates/
  rterm-core    — SSH/SFTP session logic (russh, russh-sftp), no GUI
  rterm-gui     — iced-based 3-column UI (session panel, file manager, terminal)
  rterm-config  — TOML persistence (config.toml, sessions.toml) + session models
  rterm-crypto  — AES-256-GCM envelope encryption, Argon2id master-key vault
```

Entrypoint: `src/main.rs` → `rterm_gui::run()` (iced app).

## Architecture notes

- **GUI architecture**: `App` struct in `crates/rterm-gui/src/app/mod.rs` delegates all message handling to sub-modules via `routing::update()`. Each module (session, tabs, sftp, transfer, settings, hostkey, masterpw, updates) owns its own state and communicates via typed `Message` variants. Never mutate `App` fields directly from sub-modules.
- **Config persistence**: `~/.config/rterm/` on Linux, `dirs::config_dir()/rterm/` on other platforms. Files: `config.toml` (app prefs), `sessions.toml` (session configs with encrypted credentials).
- **Log output**: platform cache dir (`~/.cache/rterm/logs/` on Linux), daily rotation, 7-day retention. All log messages should be in English.
- **i18n**: `rust-i18n` with `locales/en.yml` and `locales/zh-CN.yml`. Use `t!("key")` macro (re-exported from crate root). Never hardcode Chinese or English strings in `rterm-gui` — always use translation keys.
- **Master password modes**: Mode 0 (default, random DEK in system keychain, no sync), Mode 1 (master password derived DEK, syncable). Check `App::can_enable_sync()` to gate sync features.
- **Terminal**: powered by `alacritty_terminal` with `russh` PTY. CWD tracking via OSC 7 escape sequence; falls back gracefully when unavailable.
- **File dialogs**: local upload/download pickers use `rfd::AsyncFileDialog` native dialogs (in `app::session` / `app::sftp`), not iced window events.

## Build quirks

- **Linux GUI deps required** for build: `libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev libfontconfig1-dev pkg-config`
- **Windows**: `build.rs` embeds icon via `winresource`. Missing icon file causes build failure.
- **Workspace lint**: `missing_docs = "deny"` — all public items must be documented.
- **No `rust-toolchain` file** — relies on system `stable` toolchain (matches CI).

## Packaging

```bash
cargo deb                         # Debian package (needs cargo-deb)
cargo generate-rpm               # RPM package (needs cargo-generate-rpm)
# Windows installer via Inno Setup:
# iscc /DMyAppVersion=... /DBinPath=... /DOs=windows /DArch=... packaging/windows/rterm.iss
```

## CI (for reference)

- `clippy --workspace --all-targets -- -D warnings` — hard lint gate
- `cargo test --target $TARGET` — per-platform test
- `cargo build --target $TARGET` — per-platform build
- Release workflow: cross-compile 6 platforms, upload zip/exe/deb/rpm artifacts
