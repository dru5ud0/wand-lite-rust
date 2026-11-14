# Confirmed Wand 12.30.1 legacy (`trainerlib`) trainer protocol

Status: **CONFIRMED by static inspection of the installed shipped client bundles.**
Source: `C:\Users\Dru\AppData\Local\Wand\app-12.30.1\resources\app.asar`
Extracted copy: `C:\Users\Dru\Documents\Codex\2026-11-12\i-have-been-working-on-a\work\research-asar`

All offsets below are byte offsets into the named bundle file.

## 1. Backend selection (confirmed)

`app-10739a52.04369e7cf64f5e4748bd.bundle.js` module `69482`:

```
fetch(e,t,r){switch(e){case"trainerlib":return await this.#ee(t,r);
case"trainerlib-local":return await this.#te(t);default:return!1}}
```

- Trainer metadata carries `loader` (`"trainerlib"`) and `loaderArgs`
  (`{trainerId, binaryUrl, binaryHash}`), plus `blueprint.cheats[]`.
- `"cheats/trainer/index"` (same bundle, tail) registers two backends:
  `[["trainerlib", ...],["tophat", ...]]`.
- Live telemetry recovered from the app's own cache
  (`%APPDATA%\wemod\Partitions\ads\Cache\Cache_Data\data_2`) proves the selection for
  Stick It to the Stickman:
  `title_id=97315 game_id=105784 app_sku=2085540 trainer_id=50696
   trainer_backend=trainerlib game_version=2639440150 supported=true`
  Earlier runs used trainer `50588` (trainerlib) and `50380` (once trainerlib, once tophat).

## 2. Legacy bootstrap struct (confirmed)

`app-10739a52...bundle.js` module `59455`:

```js
run(e){return this.#j.createRemoteThread(e.hostProcessId,e.call.module,e.call.export,this.#q(e),e.call.proxy??void 0)}
#q(e){const t=Buffer.alloc(1552+32*e.variables.length);let r=0;
 t.write(e.logPipe,0,512,"utf16le"),r+=512,
 t.write(e.messagePipe,r,256,"utf16le"),r+=256,
 t.writeUInt32LE(e.flags,r),r+=4,r+=252,
 t.write(e.module,r,512,"utf16le"),r+=512,
 t.writeUInt32LE(e.targetProcessId,r),r+=4,
 t.writeUInt32LE(e.gameVersion||0,r),r+=4,
 t.writeUInt32LE(e.variables.length,r),r+=4,
 e.variables.forEach(e=>{t.write(e,r,32,"ascii"),r+=32}),r}
```

| Offset | Size | Field |
|---:|---:|---|
| 0 | 512 | log pipe path, UTF-16LE, NUL padded |
| 512 | 256 | message (`In`) pipe path, UTF-16LE |
| 768 | 4 | flags (u32 LE) |
| 772 | 252 | reserved/padding |
| 1024 | 512 | trainer DLL path (`module`), UTF-16LE |
| 1536 | 4 | target process id (u32 LE) |
| 1540 | 4 | game version (u32 LE) |
| 1544 | 4 | variable count (u32 LE) |
| 1548 | 32*n | variable names, ASCII, one 32-byte slot each |
| end | 4 | trailing pad (total = 1552 + 32*n) |

`variables` = `Array.from(new Set([...blueprint.cheats.map(c=>c.target),
...metadata.gameEvents.map(e=>"wm_event_"+e.key.substring(0,22))]))`
(module `69482`, `#de`).

## 3. Injection sequence (confirmed)

`app-10739a52...bundle.js` module `75773` + `76974`:

1. `grantFilePermissions([trainerDll, TrainerLib_{arch}.dll, CELib_{arch}.dll])`
2. optional `MaskModules`: copy trainer DLL to `<trainerlib>\stub\<name>.dll` (decoy) and
   inject decoy first.
3. `prepareProcess(targetProcessId, flags)` → `waitForInputIdle(pid, 10_000)` and, if
   `SuspendProcess` flag, `suspendProcess(pid)`.
4. `injectDll(hostProcessId, TrainerLib_{arch}.dll, HideThreads?0x700000:0, 5000)`
   (LoadLibrary into the target; `hostProcessId` is the game PID, or the
   `TrainerHost_{arch}.exe` PID when the `UseExternalHost` flag is set).
5. `createRemoteThread(hostProcessId, module=TrainerLib path, export="Run",
   param=bootstrap struct, proxy = HideThreads ? "ExitProcess" : null)`
6. `resumeProcess(targetProcessId)` when suspended.

`TrainerLib_x64.dll` exports `Run` (RVA `0x154b0`), which consumes the struct, loads the
trainer DLL from the `module` field, calls its `Initialize` export and then connects the
named pipes.

## 4. Named pipes and handshake (confirmed)

`app-10739a52...bundle.js` module `20899`:

```js
const t=`\\\\.\\pipe\\WeMod\\Trainer_${Date.now()}_`;
[n,i,s]=await Promise.all([this.#x(`${t}In`),this.#x(`${t}Out`),logPipe?…:this.#x(`${t}Log`)]);
// #x = net.createServer(); server.listen(name); grantFilePermissions(name,[everyone,appContainer],Read|Write)
```

- **The client is the pipe server**; the injected trainer is the client.
- On a connection to `Out`: the client writes the `In` pipe address as **UTF-16LE**
  (`t.write(e.address(),"utf16le",cb)`) and keeps that socket as the command channel.
- On a connection to `In`: the client starts decoding frames.
- `Log` pipe frames (decoded in `#Y`): header `[level:u32][pad:u32][byte_len:u32][pad:u32]`
  then `byte_len` bytes of **UTF-16LE** message text. `level` 0=Verbose,1=Info,2=Warning,3=Error.
  A log line matching `/Trainer setup failed/i` fails the session; `/Last Win32:?\s*0?x?([0-9a-fA-F]+)/`
  extracts the Win32 error code.

## 5. Frame format (confirmed)

Enum `m = {Loading:0, Ready:1, Value:2, ValueResponse:3, Buffer:4}`.

| Type | Size | Layout |
|---|---:|---|
| Loading (0) | 8 | `u32 kind` |
| Ready (1) | 8 | `u32 kind` |
| Value (2) | 56 | `u32 kind` @0, `u32` @4, `u32 requestId` @8, `u32` @12, `ascii[32] name` @16, `f64 value` @48 |
| ValueResponse (3) | 24 | `u32 kind` @0, `u32 requestId` @8, `u32 result` @16 |
| Buffer (4) | 48+n | `u32 kind` @0, `ascii[32] type` @8, `u32 n` @40, `n` bytes @48 |

- Client→trainer value set (`setValue(name,value)`): a `Value` frame with a monotonically
  increasing `requestId` starting at 1; the promise resolves with the `result` u32 of the
  matching `ValueResponse`. `result != 0` indicates a rejected/unknown variable
  (`trainer.unknown_variable` exists in the client error enum).
- `sendBufferMessage(type,payload)` rejects `type.length >= 32`; used by the client for
  `global_state` (live game data) at least.
- Client subscribes to: `value`, `buffer-message`, `log`, `input-event`.

## 6. Lifecycle

`connected` = `In` pipe connected; `ready` = `Ready` frame received;
`failed` = trainer setup failure log line or `In` socket error/`EPIPE`.

Teardown: close all three servers/sockets and dispose subscriptions.

## 7. Still unknown (needs runtime evidence)

- Exact `target` identifiers for trainer `50696`'s five cheats. Public page confirms the
  display names only: *God Mode, Add Money When Spend, Add Reroll Dice When Use,
  Ammo Never Decrease, Instant Kill*. Candidate targets follow Wand's convention
  (`god_mode`, `add_money_when_spend`, `add_reroll_dice_when_use`,
  `ammo_never_decrease`, `instant_kill`) and can be validated at runtime with the
  `ValueResponse.result` oracle.
- Whether `TrainerLib` requires the trainer DLL to be present in the trainerlib dir or
  accepts the cached path in the `module` field (it takes a full path, so the cache is fine).
- The `flags` bitfield values (`$R` enum) except `SuspendProcess`, `MaskModules`,
  `HideThreads`, `UseExternalHost` (names confirmed, numeric values not yet).
