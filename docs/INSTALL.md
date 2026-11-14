# Installing on a different PC

Wand Lite is a single self-contained Windows executable. There is no installer
framework, no runtime to pre-install, and no Visual C++ redistributable needed.

The one thing it cannot ship is **Wand's trainer runtime** — that belongs to the
Wand desktop app and must come from your own legitimate installation. See
[What you need from Wand](#what-you-need-from-wand) below.

---

## Option A — prebuilt release (no developer tools)

1. Go to the repository's **Releases** page and download
   `wand-lite-rust-<version>-win64.zip`.
2. Extract it anywhere.
3. In that folder, run:

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\install.ps1
   ```

   That copies the executable to `%LOCALAPPDATA%\Programs\WandLite` and adds a
   **Wand Lite** Start Menu shortcut. Add `-DesktopShortcut` if you want a
   desktop icon too.

4. Launch **Wand Lite** from the Start Menu.

To remove it later:

```powershell
powershell -ExecutionPolicy Bypass -File .\uninstall.ps1
# add -RemoveData to also delete cached catalog/artwork/preferences
```

### Portable use

You can skip the installer entirely: just run `wand-rebuild-ui.exe` from the
extracted folder. It writes only to `%LOCALAPPDATA%\WandRebuild`
(catalog, artwork, preferences, trainer log).

---

## Option B — build from a repository checkout

Requires Windows 10/11 x64 and the Rust MSVC toolchain.

```powershell
git clone https://github.com/dru5ud0/wand-lite-rust.git
cd wand-lite-rust
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

If Rust is missing the script tries `winget install Rustlang.Rustup`, falling back
to downloading `rustup-init.exe`. To build manually instead:

```powershell
cd rust
cargo build --package wand-rebuild-ui --release
# output: rust\target\release\wand-rebuild-ui.exe
```

`rust/.cargo/config.toml` enables a static C runtime, so the produced `.exe` is
portable to machines without the VC++ redistributable.

---

## Option D — debloat: adopt an existing Wand install, then drop Electron

This is the migration path when Wand (or a previous Wand Lite Core build) is
already on the machine. `debloat.ps1` does four things:

1. Finds the **newest** complete Wand application directory (or `-WandPath`).
2. Copies its backend into `%LOCALAPPDATA%\WandRebuild\backend`:

   ```
   backend\runtime\app-12.30.1\trainerlib\   TrainerLib_*.dll, CELib_*.dll, ...
   backend\trainers\                          Trainer_<id>_<hash>.dll
   backend\blueprints\                        copy of the Local Storage LevelDB
   backend\backend.json                       provenance manifest
   ```

3. Installs Wand Lite and verifies `--self-check` reports
   `Backend source: adopted …`.
4. Optionally uninstalls the Electron Wand app (`-RemoveWand`).

```powershell
# preview
powershell -ExecutionPolicy Bypass -File .\debloat.ps1 -DryRun

# adopt + install
powershell -ExecutionPolicy Bypass -File .\debloat.ps1

# adopt + install + uninstall Wand
powershell -ExecutionPolicy Bypass -File .\debloat.ps1 -RemoveWand

# refresh after a Wand update
powershell -ExecutionPolicy Bypass -File .\debloat.ps1 -Force
```

Notes:

- The adopted files are **your own licensed copies**, copied locally. Nothing is
  uploaded or redistributed.
- Once adoption succeeds, `%LOCALAPPDATA%\Wand\app-*` is no longer read. The
  client prefers the adopted backend and only falls back to a live Wand install
  when no adopted backend exists.
- Wand's cached data in `%APPDATA%\wemod` is left untouched by `-RemoveWand`.
- Verify at any time: `wand-rebuild-ui.exe --self-check` prints
  `Backend source: adopted <version> (<n> trainers cached)`.

---

## What you need from Wand

Wand Lite is a **client** for the trainer runtime that the official Wand app
installs. Before using it on a new PC:

1. Install the official **Wand** desktop app and sign in.
2. For each game you want, open that game's **trainer page in Wand once**. This
   caches the trainer binary and its blueprint on disk:
   - `%APPDATA%\wemod\App\trainers\Trainer_<id>_<hash>.dll`
   - `%APPDATA%\wemod\Local Storage\leveldb`
3. Close Wand. It is not needed while using Wand Lite.

What Wand Lite reads from your installation at runtime (read-only):

| Path | Purpose |
|---|---|
| `%LOCALAPPDATA%\Wand\app-<version>\...\trainerlib\TrainerLib_x64.dll` | injectable runtime |
| `%LOCALAPPDATA%\Wand\app-<version>\...\trainerlib\CELib_x64.dll` | runtime helper |
| `%APPDATA%\wemod\App\trainers\Trainer_*.dll` | the trainer itself |
| `%APPDATA%\wemod\Local Storage\leveldb` | trainer blueprints (cheat names/targets) |

If a trainer is not cached, Wand Lite says so instead of failing silently.

---

## Copying an existing setup to a new PC

If you already had it working on another machine, copy these to the new one:

1. `%LOCALAPPDATA%\Programs\WandLite\` — or just re-run the release zip
2. `%LOCALAPPDATA%\WandRebuild\` — optional: catalog cache, artwork, preferences
   (including your hotkey bindings)

Wand's own trainer cache is not portable — re-open the trainer pages in Wand on
the new machine instead.

---

## First run checklist

```
[ ] Official Wand app installed and signed in once
[ ] Trainer page opened in Wand for each wanted game, then Wand closed
[ ] Game started
[ ] Wand Lite started -> game selected -> "Load trainer"
[ ] Modifications shown and toggles confirmed
```

Diagnostics if something is wrong:

```powershell
& "$env:LOCALAPPDATA\Programs\WandLite\wand-rebuild-ui.exe" --self-check
Get-Content "$env:LOCALAPPDATA\WandRebuild\trainer.log" -Tail 40
```

`--self-check` reports the detected Wand runtime, installed games, catalog size
and whether the trainer for the test title resolves.

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "An installed Wand runtime was not found" | Wand app not installed, or installed to a non-standard location |
| "Trainer N is not downloaded yet" | open that game's trainer page in Wand once |
| "…is not running" | start the game first; the button enables itself within ~1 s |
| "restart the game, then load the trainer again" | one trainer session per game launch; unload requires a game restart |
| Load trainer does nothing | the status line under the button states the reason |
| SmartScreen warning | unsigned build; choose "More info" → "Run anyway" |
