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

> Note: `cargo test` at the workspace root only tests the root `rterm` package
> (0 tests). Use `cargo test --workspace` to actually run the crate tests.

That `clippy` invocation lints the `debug` profile only. When you touch
`#[cfg(...)]`-gated code, also run
`cargo clippy -p <crate> --release --all-targets -- -D warnings`: anything
referenced *only* from a `debug_assertions` block — constants, `use` imports,
helpers — becomes dead code in a `release` build, and `-D warnings` rejects it.
CI does not catch this, so check it by hand.

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
- **Config persistence**: `~/.config/rterm/` on Linux, `dirs::config_dir()/rterm/` on other platforms. Files: `config.toml` (app prefs, grouped into functional-domain TOML sections `[connection]`/`[terminal]`/`[appearance]`/`[logging]`/`[updates]`/`[security]`/`[window]`; legacy flat configs are auto-migrated on load with a `config.toml.bak` backup), `sessions.toml` (session configs with encrypted credentials). All of these paths are resolved through `rterm-config/src/paths.rs` — see **Dev sandbox** below.
- **Log output**: platform cache dir (`~/.cache/rterm/logs/` on Linux), daily rotation, 7-day retention. All log messages should be in English.
- **i18n**: `rust-i18n` with `locales/en.yml` and `locales/zh-CN.yml`. Use `t!("key")` macro (re-exported from crate root). Never hardcode Chinese or English strings in `rterm-gui` — always use translation keys.
- **Master password modes**: Mode 0 (default, random DEK in system keychain, no sync), Mode 1 (master password derived DEK, syncable). Check `App::can_enable_sync()` to gate sync features.
- **Terminal**: powered by `alacritty_terminal` with `russh` PTY. CWD tracking via OSC 7 escape sequence; falls back gracefully when unavailable.
- **File dialogs**: local upload/download pickers use `rfd::AsyncFileDialog` native dialogs (in `app::session` / `app::sftp`), not iced window events.

## Dev sandbox

`debug` builds (`cargo run`, `cargo test`) never touch the user's real config.
All state is redirected into `.dev/` at the workspace root (git-ignored):

```
.dev/config/config.toml      instead of ~/.config/rterm/config.toml
.dev/config/sessions.toml    instead of ~/.config/rterm/sessions.toml
.dev/cache/known_hosts       instead of ~/.cache/rterm/known_hosts
.dev/cache/logs/             instead of ~/.cache/rterm/logs/
```

The keyring entry is isolated as well (`service=rterm-dev` instead of `rterm`). This
matters: if a sandbox were to re-initialise the vault against the real keyring entry,
it would overwrite the real DEK and permanently break the credentials in the real
`sessions.toml`. That damage is irreversible.

- **Single entry point**: `crates/rterm-config/src/paths.rs` is the only module allowed
  to call `dirs::`. Route any new persistent state through it instead of calling `dirs::`
  directly, otherwise the sandbox silently develops holes.
- **Compile-time routing, not env vars**: the switch is `debug_assertions`, so
  `cargo build --release` and packaged binaries are unaffected and keep using system
  directories. There is no environment variable to remember.
- **`cargo run --release` uses the REAL config.** Use it only to verify production
  behaviour, never for ordinary development.
- **Tests** isolate via `paths::set_test_root(Some(tmp))`, not `XDG_CONFIG_HOME`.
  Any test that touches the filesystem must take that root *and* hold the relevant
  module lock, or it will race other tests through the shared global root.

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
