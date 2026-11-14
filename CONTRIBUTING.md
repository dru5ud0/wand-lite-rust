# Contributing

Thanks for looking at the project. A few ground rules keep it publishable.

## Hard rules

1. **Never commit third-party binaries or data.** No Wand/WeMod executables, no
   trainer DLLs, no `catalog.json`, no downloaded game artwork, no copied
   application code. `.gitignore` blocks the known paths; do not work around it.
2. **Never commit credentials.** No tokens, cookies, session files or account
   data — not even redacted samples. The app reads Wand's cache read-only and
   must never log or transmit secrets.
3. **Do not add per-game trainer logic.** Trainers are discovered from
   blueprints and driven through the generic transport. Game-specific behaviour
   belongs in the trainer binary, not in this client.

## Building and testing

Requires Windows 10/11, the Rust MSVC toolchain and the Microsoft C++ build
tools / Windows SDK.

```powershell
cd rust
cargo test --workspace --release
cargo build --package wand-rebuild-ui --release
```

Tests must pass with **no installed Wand runtime present** — they use synthetic
fixtures, temporary directories and loopback/mock transports. A test that needs
a real trainer session belongs behind an ignored/example target, not in
`cargo test`.

Useful diagnostics:

```powershell
.\target\release\wand-rebuild-ui.exe --self-check
.\target\release\wand-rebuild-ui.exe --trainer-self-test   # game must be running
cargo run --release --example hotkey_probe
cargo run --release --example legacy_session -- --probe
```

## Code layout

| Path | Responsibility |
|---|---|
| `rust/core/src/catalog.rs`, `steam.rs`, `art` (ui) | library discovery, catalog, artwork |
| `rust/core/src/game_locator.rs`, `process_monitor.rs` | install dir → running process, game version |
| `rust/core/src/trainer_blueprint.rs` | blueprint parsing, backend + version selection |
| `rust/core/src/named_pipe.rs`, `legacy_pipe.rs` | `trainerlib` transport |
| `rust/core/src/trainer_inject.rs` | Win32 injection and bootstrap struct |
| `rust/core/src/trainer_session.rs` | session state machine |
| `rust/core/src/hotkeys.rs` | global keyboard hook and capture |
| `rust/core/src/tophat_*` | Tophat gRPC/FlatBuffers transport |
| `rust/ui/src/*` | Slint app state, trainer panel, presentation logic |

## Reporting bugs

Include the output of `--self-check`, the relevant lines from
`%LOCALAPPDATA%\WandRebuild\trainer.log`, and the Wand runtime version. Do not
paste tokens or full Local Storage dumps.

## Style

`cargo fmt` and `cargo clippy --workspace` before sending a pull request.
Prefer small, typed modules with tests over large untyped changes.
