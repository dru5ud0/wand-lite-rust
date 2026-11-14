# Wand 12.30.1 trainer backend map

Status as of 13 November 2026. This document distinguishes **confirmed** findings
(static inspection of the installed package plus a verified live session) from
**unknowns**. The authoritative evidence record for the legacy backend is
[docs/LEGACY_PROTOCOL.md](docs/LEGACY_PROTOCOL.md).

## Verified result

A native Rust client now completes a real trainer session against the installed
Wand runtime, without the Electron UI:

```
resolve trainer 50696 (trainerlib) -> create WeMod\Trainer_* pipes
  -> inject TrainerLib_x64.dll into the game process
  -> CreateRemoteThread(TrainerLib!Run, bootstrap struct)
  -> trainer logs "Trainer fully initialized." -> Ready frame
  -> send Value frames -> trainer replies ValueResponse(result=0)
  -> trainer logs "<target> set to <n> by client."
  -> unload by closing the pipes
```

Confirmed in-game trainer log lines from the live session:

```
Initialization started. PID: 1116, TID: ...
Game Version: -1655527146
Trainer DLL: ...\Trainer_50696_f0de8ff90c.dll
Creating NamedPipeMessenger with pipe: \\.\pipe\WeMod\Trainer_<ms>_Out
Connecting to client...
Loading trainer into the process...
[Mono] Intialized.
Compiling Money_Calc_AOB / Reroll_Calc_AOB / Health_Calc_AOB / Ammo_Dec_AOB
Allocating 5 variables...
Variable polling started.
Trainer fully initialized.
god_mode set to 1 by client.
instant_kill set to 0 by client.
```

## Two backends

| Backend | Transport | Selection | Status here |
|---|---|---|---|
| `trainerlib` (legacy) | `\\.\pipe\WeMod\Trainer_*` named pipes, `TrainerLib_{arch}.dll` injected into the game process | trainer metadata `loader: "trainerlib"` | **implemented and verified live** |
| `tophat` | local HTTP/2 gRPC `/tophat.TophatService`, FlatBuffers payloads, `tophat_service_{arch}.dll` | trainer metadata `loader: "tophat"` | codec + bounded unary transport implemented; no live session yet |
| `trainerlib-local` | trainer binary outside the account catalog | `loader: "trainerlib-local"` | resolved only |

The client registers both backends in `"cheats/trainer/index"`:
`[["trainerlib", ...],["tophat", ...]]`.

Stick It to the Stickman (Steam `2085540`) is **`trainerlib`**. This is confirmed
twice: by Wand's own telemetry recovered from its cache
(`title_id=97315 game_id=105784 trainer_id=50696 trainer_backend=trainerlib
game_version=2639440150 supported=true`) and by the live session above.

## Legacy backend contract (confirmed)

### 1. Pipe family

The **client is the pipe server**. Names: `\\.\pipe\WeMod\Trainer_<unix_millis>_`
plus `In`, `Out`, `Log`.

* `Out` is the **command channel**: the client writes `Value`/`Buffer` frames here.
* `In` is the **event channel**: the trainer writes `Loading`, `Ready`, `Value`,
  `ValueResponse` frames here.
* On the `Out` connection the client first writes the `In` pipe path as UTF-16LE
  (the "greeting"), which tells the trainer where to send events.
* `Log` carries UTF-16LE log records:
  `[level:u32][pad:u32][byte_len:u32][pad:u32][utf16le text]`,
  level 0=Verbose 1=Info 2=Warning 3=Error.

Both pipes are duplex but **must not be read and written from different threads on
a synchronous handle**: a blocked `ReadFile` deadlocks `WriteFile` on the same
handle. The implementation therefore reads only `In` and writes only `Out`.

### 2. Bootstrap struct

`CreateRemoteThread(TrainerLib_{arch}.dll!Run, struct)` where the struct is:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 512 | log pipe path, UTF-16LE |
| 512 | 256 | **`Out`** pipe path, UTF-16LE |
| 768 | 4 | flags (`SuspendProcess=1`, `ResumeProcess=2`, `HideThreads=4`, `MaskModules=8`, `UseExternalHost=16`) |
| 772 | 252 | reserved |
| 1024 | 512 | trainer DLL path, UTF-16LE |
| 1536 | 4 | target process id |
| 1540 | 4 | game version |
| 1544 | 4 | variable count |
| 1548 | 32*n | variable names, ASCII, one 32-byte slot each |
| end | 4 | trailing pad (total 1552 + 32*n) |

### 3. Frames

| Type | Size | Layout |
|---|---:|---|
| Loading (0) | 8 | `kind` |
| Ready (1) | 8 | `kind` |
| Value (2) | 56 | `kind`@0, `requestId`@8, `name` ascii[32]@16, `f64 value`@48 |
| ValueResponse (3) | 24 | `kind`@0, `requestId`@8, `result`@16 |
| Buffer (4) | 48+n | `kind`@0, `type` ascii[32]@8, `len`@40, payload@48 |

`requestId` starts at 1. `result == 0` means the trainer accepted the variable;
any other value is a rejection. The trainer additionally logs
`<target> set to <value> by client.` for every accepted change.

### 4. Variable names

The trainer's variable list is supplied by the client at start-up; the trainer
allocates one slot per name (`Allocating N variables...`). For trainer 50696 the
five names below were **accepted by the live trainer** (`ValueResponse.result=0`):

```
god_mode
add_money_when_spend
add_reroll_dice_when_use
ammo_never_decrease
instant_kill
```

These match the public trainer page's display names (God Mode, Add Money When
Spend, Add Reroll Dice When Use, Ammo Never Decrease, Instant Kill).

## Trainer metadata

The full blueprint (`blueprint.cheats[]` with `uuid`, `name`, `type`, `target`,
`args`, `category`, `hotkeys`, `parent`) is what the client uses to build both the
UI and the variable list. It is available from:

1. **Wand's own Local Storage LevelDB cache** (`%APPDATA%\wemod\Local Storage\leveldb`),
   which holds complete trainer entries for titles the user has opened. This is
   read read-only and never touches account tokens.
2. `GET /v3/games/{gameId}/trainer?gameVersions=…&locale=en-US&v=3`, which requires
   `Authorization: Bearer <access token>` and returns HTTP 401 anonymously.

Stickman's blueprint was initially absent from the cache, so the client started
from the five names above and validated them at runtime through the
`ValueResponse` oracle. Once Wand had cached the title, the client read the real
blueprint (`source: WandCache`) and it **matches those five targets exactly**,
which independently confirms the names rather than relying on the convention.

## Tophat backend (partially confirmed)

Methods on `/tophat.TophatService`: `LoadPlugin`, `UnloadPlugin`, `ExecuteCommand`,
`Heartbeat`, `Disconnect`, `StartStreamingConnection`. Payloads are FlatBuffers
inside gRPC framing. Recovered command payloads:

```
loadPlugin("trainerlib", pluginDllPath)
executeCommand("trainer_run_mod_json", {processId, gameVersion, trainerDllPath, initialVars})
executeCommand("trainer_push_client_req_from_json", {requestId, name, value})
executeCommand("trainer_stop_mod")
```

Streaming channels: `trainer_initializing`, `trainer_ready`, `trainer_status`,
`trainer_log`, `trainer_value_request`, `trainer_value_response`.
Port registry: `%APPDATA%\Wand\service-ports.json` (`pid`, `port`, `service`,
`target`, `started`).

## Remaining unknowns

* **One trainer session per game launch.** `TrainerLib`'s `Run` installs hooks and
  worker threads that outlive `FreeLibrary`, so a second session in the same game
  process never becomes ready. The client detects the already-loaded runtime and
  fails immediately with "restart the game" rather than waiting for the connect
  timeout. (Best-effort `FreeLibrary` retry is available behind
  `WANDR_FORCE_TRAINER_RELOAD=1` but is not reliable.)

* **Tophat live session is not possible on this machine today.** Every cached
  trainer blueprint here has `"loader":"trainerlib"` (19 entries, 0 `tophat`), and
  `%APPDATA%\Wand\service-ports.json` holds only a dead PID with no live listener.
  The full RPC and streaming contract is documented in
  `work/research/TOPHAT_FINDINGS.md`; a session needs a title whose trainer
  selects Tophat.
* **In-game effect is not confirmed.** The trainer acknowledges every change
  (`ValueResponse(result=0)`) and logs `<target> set to <n> by client.`, but the
  Stickman modifications are toggles that need gameplay triggers (taking damage,
  spending money, firing) and no trainer-pushed `Value` frames reflecting game
  state were observed. See `work/research/BENCHMARK_FINDINGS.md`.
* Hotkey capture and the in-game overlay.

## Resolved since the first draft

* Generic game resolution: Steam manifests → install dir → running process under
  that dir (`core/src/game_locator.rs`), with helper executables such as
  `UnityCrashHandler64.exe` filtered out.
* Trainer selection by installed game version: a title can have several cached
  trainers after a game update, so the client picks the one whose
  `supportedVersions` contains the installed build (`select_for_version`).
* Slider/number controls and hotkey labels are blueprint-driven in the native
  trainer panel.
