# Wand Lite Rust (unofficial)

A native Windows game library and trainer client inspired by a personal Wand Lite
Core layout. Rust + Slint, no Electron, no embedded browser. This project is **not
affiliated with Wand or WeMod**.

## What works

- Detects installed Steam games and one Microsoft Store title, No Man's Sky.
- Loads Wand's public catalog, filters installed games with listed trainers,
  searches titles, caches the catalog for offline use.
- Downloads game thumbnails into `%LOCALAPPDATA%\WandRebuild\art` at runtime.
- Responsive game grid or list, local search, favorites, A–Z and recently played
  ordering, game detail, game launch through Steam/Microsoft Store URIs.
- Detects a complete installed Wand runtime and verifies cached trainer binaries.
- **Native trainer activation and control through Wand's legacy (`trainerlib`)
  backend**: creates the `WeMod\Trainer_*` pipe family, injects
  `TrainerLib_{arch}.dll` into the running game, exchanges the recovered frame
  protocol, renders the trainer's modifications as native controls and sends
  value changes that the trainer acknowledges.

Verified live against Wand 12.30.1 and Stick It to the Stickman (trainer `50696`):

```
Trainer fully initialized.
god_mode set to 1 by client.
instant_kill set to 0 by client.
```

The trainer's own log confirms each change, and every `Value` frame receives a
`ValueResponse(result=0)`.

## What does not work yet

- The Tophat (`tophat`) backend has a verified codec and bounded gRPC transport but
  no live session; a title whose trainer selects Tophat is needed.
- Account sign-in and authenticated trainer metadata. The client instead reads
  Wand's own cached blueprints from its Local Storage (read-only) and falls back to
  a runtime-validated candidate list for the verified title.
- Hotkeys, in-game overlay, maps, clips, guides, voice.
- Generic game-process mapping for titles other than the verified one.

## Build on Windows

Install the [Rust MSVC toolchain](https://rust-lang.org/tools/install/) and
Microsoft C++ build tools/Windows SDK. From this folder:

```powershell
.\build.ps1
```

Or use Cargo directly:

```powershell
cargo test --workspace --release
cargo build --package wand-rebuild-ui --release
```

Run `target\release\wand-rebuild-ui.exe`. Run
`target\release\wand-rebuild-ui.exe --self-check` to print installed runtime,
catalog and artwork diagnostics without opening a window.

## Verifying a live trainer session

Start the game first, then either use the UI's **Load trainer** button or run the
headless harnesses:

```powershell
# Drives the core session directly and reports every frame.
cargo run --release --example legacy_session -- --probe --hold 10

# Exercises the exact UI panel path: resolve, start, toggle every mod, unload.
.\target\release\wand-rebuild-ui.exe --trainer-self-test
```

`--trainer-self-test` prints `trainer self-test PASSED` only when the trainer
confirmed every modification. Set `WANDR_SELFTEST_HOLD_MS` to keep the session live
for measurement, and `WAND_DEBUG_PIPES=1` to hex-dump pipe traffic.

The app stores only preferences, catalog and thumbnails under
`%LOCALAPPDATA%\WandRebuild`. It reads the installed Wand runtime, its trainer
cache and its Local Storage blueprints; it never writes to them and never reads
account tokens.

## Tests

`cargo test --workspace --release` runs 59 tests: bootstrap-struct layout and validation, PE export resolution, blueprint parsing, LevelDB extraction and version-based trainer selection, log framing, frame codecs, backend selection, session guards, pipe naming/greeting, a real Windows named-pipe transport round-trip, game/install resolution with helper-process filtering, slider clamping and hotkey labels, process discovery, Tophat codec/registry/transport, catalog, Steam and preferences.

A mock or codec test is never presented as proof of a live trainer operation; the
live session evidence is separate and recorded in `../BACKEND_MAP.md` and
`../docs/LEGACY_PROTOCOL.md`.

## Source and assets

The Rust and Slint code is MIT licensed; see `LICENSE`. Bundled Inter fonts have
their own SIL Open Font License in `ui/assets/fonts/LICENSE.txt`. Wand/WeMod
software, trainer binaries, game art and catalog data are **not included** in the
public source package. Users must obtain and use the installed Wand application
separately under its own terms.

The protocol modules document an observed Wand 12.30.1 installation and may not
work with other versions.
