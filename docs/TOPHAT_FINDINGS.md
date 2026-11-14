# Tophat backend findings (Wand 12.30.1)

Status: read-only investigation. Evidence is static inspection of the installed
Wand 12.30.1 client (`app.asar`) plus the host's LevelDB cache and process/TCP
state. No game or Wand process was launched, suspended, or injected.

## Legend

- **CONFIRMED**: directly evidenced by shipped bundle code, cache, or host state.
- **INFERRED**: follows from confirmed evidence but not runtime-verified.
- **UNKNOWN**: not recoverable from available evidence.

---

## 1. Tophat-capable titles on this machine

**Result: none found in Wand's cached trainer blueprints.** Every cached trainer
entry in `%APPDATA%\wemod\Local Storage\leveldb` has `"loader":"trainerlib"`.
A scan of the whole `%APPDATA%\wemod` and `%LOCALAPPDATA%\wemod` trees found the
same 19 `"loader"` values, all `trainerlib` (see
`work/tools/scan_loaders.py`, `work/tools/tophat_titles.py`).

- CONFIRMED: cached trainer blueprints contain only `trainerlib` loaders.
- INFERRED: no Tophat title has been opened/cached by this account on this
  machine, so no Tophat live session is available from cache alone.
- UNKNOWN: whether the server-side catalog contains Tophat titles for this account;
  the client fetches trainer metadata from `GET /v3/games/{gameId}/trainer` with
  a bearer token, which is outside read-only scope.

---

## 2. Tophat RPC / FlatBuffers contract

Service path: `/tophat.TophatService` (CONFIRMED, `tophat_rpc_pretty.js` offset
12685; `tophat_codec.rs` `GRPC_SERVICE`).

Methods (CONFIRMED, `tophat_rpc_pretty.js` ~offset 12640-12700):

```
g = ["LoadPlugin","UnloadPlugin","ListPlugins","GetPluginInfo",
     "Disconnect","Heartbeat","ExecuteCommand","ListCommands",
     "StartStreamingConnection"]
b = ["StartStreamingConnection"]   // server-streaming methods
```

Unary methods use gRPC unary POST; `StartStreamingConnection` is a **server-streaming**
method (`responseStream: true`).

### Field layouts (FlatBuffers, field index → type)

HeartbeatRequest (`tophat_schema_bundle.js` / `tophat_load_schema_bundle.js`)
- 0 `timestamp` int64
- 1 `sequenceNumber` int32

HeartbeatResponse
- 0 `success` int8
- 1 `serverTimestamp` int64
- 2 `sequenceNumber` int32
- 3 `error` offset (WandError, optional)

LoadPluginRequest (`tophat_load_schema_bundle.js`)
- 0 `pluginName` string offset
- 1 `pluginPath` string offset
- 2 `loadFlags` int32 (default 0)

LoadPluginResponse
- 0 `success` int8
- 1 `errorMessage` string offset
- 2 `pluginHandle` int64
- 3 `pluginName` string offset
- 4 `pluginPath` string offset
- 5 `loadTimeMs` int64
- 6 `error` WandError offset

UnloadPluginRequest
- 0 `pluginHandle` int64
- 1 `pluginName` string offset

UnloadPluginResponse
- 0 `success` int8
- 1 `errorMessage` string offset
- 2 `pluginName` string offset
- 3 `error` WandError offset

CommandRequest (ExecuteCommand)
- 0 `command` string offset
- 1 `parameters` vector<int8> offset
- 2 `returnType` int8 (client sends 3)
- 3 `timeoutMs` int32 (client sends 5000)
- 4 `clientId` string offset
- 5 `requestId` string offset (optional)

CommandResponse
- 0 `success` int8
- 1 `errorMessage` string offset
- 2 `returnValue` vector<int8> offset
- 3 `handledByPlugin` string offset
- 4 `command` string offset
- 5 `clientId` string offset
- 6 `errorCode` int8
- 7 `error` WandError offset
- 8 `requestId` string offset

DisconnectRequest
- 0 `reason` string offset (optional; client sends none)

DisconnectResponse
- 0 `success` int8
- 1 `message` string offset
- 2 `error` WandError offset

StreamMessage (used by StartStreamingConnection)
- 0 `messageType` string offset
- 1 `payload` vector<int8> offset
- 2 `sequenceId` int64
- 3 `timestamp` int64
- 4 `source` string offset
- 5 `requiresResponse` int8
- 6 `traceparent` string offset
- 7 `tracestate` string offset

StreamRegistration
- 0 `clientId` string offset
- 1 `clientInfo` string offset
- 2 `streamCapabilities` vector<offset>

ServerNotification (server→client side channel)
- 0 `notificationType` string
- 1 `pluginName` string
- 2 `severity` string
- 3 `details` string
- 4 `timestamp` int64

ServerCommand (server→client)
- 0 `commandType` string
- 1 `parameters` vector<int8>
- 2 `timeoutMs` int32
- 3 `commandId` int64

### Corrections needed in `tophat_codec.rs`

Mostly **none for the three methods already implemented**; the codec matches
the schema and the client's usage:

- `encode_heartbeat` matches HeartbeatRequest (field 0 timestamp, field 1 seq).
- `decode_heartbeat_response` matches HeartbeatResponse (0/1/2/3).
- `encode_load_plugin` matches LoadPluginRequest for fields 0/1; field 2
  (`loadFlags`) is omitted and defaults to 0, which matches the shipped client
  (it never sets `loadFlags`).
- `decode_load_plugin_response` reads 0/1/2; ignores 3-6 (acceptable).
- `encode_execute_*` matches CommandRequest (0 command, 1 parameters, 2 returnType=3,
  3 timeoutMs=5000, 4 clientId, 5 requestId).
- `decode_command_response` reads 0/1/2/6; ignores 3/4/5/7/8 (acceptable).

**Missing for a full session** (not corrections, but gaps):
- `encode_unload_plugin(plugin_handle, plugin_name?)`
- `encode_disconnect(reason?)`
- `encode_stream_registration(client_id)` producing a `StreamMessage` with
  `messageType="register"`, empty payload, `sequenceId=0`, `timestamp=now`,
  `source=clientId`, `requiresResponse=false`.
- Decoders for `UnloadPluginResponse`, `DisconnectResponse`.
- `tophat_transport::Method` must add `UnloadPlugin`, `Disconnect`,
  `StartStreamingConnection` (streaming), and `ListPlugins`/`GetPluginInfo`/
  `ListCommands` if desired.
- `tophat_transport` currently only supports bounded unary calls; streaming
  requires a persistent h2 connection with `makeServerStreamRequest` and an
  event loop (see §4).

---

## 3. Service lifecycle

CONFIRMED:
- Manifest (`vendor..pnpm-2a95838e...bundle.js` offset 21224):
  - `serviceDll: "tophat_service_{arch}.dll"`
  - `trainerlibPluginDll: "TrainerLibPlugin_{arch}.dll"`
  - `inputCaptureDll: "InputCapturePlugin_{arch}.dll"`
  - `startExport: "StartTophatServiceThread"`
  - `registryFilename: "service-ports.json"`
- The installed DLL exports `StartTophatServiceThread`, `StopTophatService`,
  `dummy_debug_proc`, `tophat_stream_broadcast` (dumpbin on
  `.../trainerlib/tophat_service_x64.dll`).
- Injection sequence (from `app-10739a52...bundle.js` offsets
  ~10522-11400):
  1. `grantFilePermissions([tophat_service DLL, TrainerLibPlugin, InputCapture, trainer DLL])`
  2. `prepareProcess(gamePid, flags)` (suspends if `SuspendProcess` flag)
  3. `injectDll(hostProcessId, tophat_service DLL)` (LoadLibrary into the game
     process, or into `TrainerHost_{arch}.exe` when `UseExternalHost`)
  4. `createRemoteThread(hostProcessId, module=tophat_service DLL,
     export="StartTophatServiceThread")` with **no bootstrap struct argument**
  5. resume process if suspended
- The service thread binds a loopback TCP port and registers it in
  `%APPDATA%\Wand\service-ports.json`.
- `connectToTophat` (module 3926, `vendor..pnpm-80c6c6c4...bundle.js`) polls
  that file every 500 ms until an entry for the target process appears, then
  opens an h2 connection to `127.0.0.1:<port>`.

Registry schema (CONFIRMED by `service-ports.json` on this machine and Rust
`RegistryEntry`):

```json
[
  { "pid": <u32>, "port": <u32>, "service": "tophat_service",
    "target": "<game process name>", "started": <unix seconds> }
]
```

Who writes it: **INFERRED** — the `tophat_service` DLL's `StartTophatServiceThread`
running inside the host process. The JS only reads it; no JS writer was found.
The DLL is packed (`.wemod` section) so the exact writer code is not visible
without decompilation.

---

## 4. Streaming contract

CONFIRMED (`tophat_vendor_2.js` / `vendor..pnpm-2a95838e...bundle.js`):

- `StartStreamingConnection` is **server-streaming**.
- Client opens it with `serverStream("StartStreamingConnection", <StreamMessage>, metadata)`.
- The request body is a `StreamMessage` produced by `streamRegistrationRequest(clientId)`:
  `messageType="register"`, empty payload, `sequenceId=0`, `timestamp=now`,
  `source=clientId`, `requiresResponse=false`.
- Metadata header `client-id: <clientId>` is set.
- Each server-pushed `StreamMessage` has `messageType` = **channel name** and
  `payload` = raw bytes for that channel. The client dispatches via
  `EventEmitter.emit(channel, payload, sequenceId, {traceparent, tracestate})`.
- Channels:
  - `trainer_initializing` → no payload (callback with no args)
  - `trainer_ready` → no payload
  - `trainer_status` → `parseStatusMessage(payload)`
  - `trainer_log` → `parseLogMessage(payload)`
  - `trainer_value_request` → `parseValueMessage(payload)`
  - `trainer_value_response` → `parseValueResponseMessage(payload)`; used to
    resolve pending `setValue` promises by `requestId`.
- Lifecycle: `on("data")` dispatches; `on("error"/"end")` emits `"disconnect"`
  (with optional error). Client calls `cancel()` on close.
- Heartbeat runs separately every 5000 ms via unary `Heartbeat(seq++)`.

To receive the trainer events, a client must:
1. Open the unary gRPC connection (port discovery + h2).
2. Call `serverStream("StartStreamingConnection", registerMessage, {client-id})`.
3. Attach `data` handler, decode each `StreamMessage`, and `emit(messageType, payload, ...)`.
4. Register listeners for the six channel names above before calling
   `LoadPlugin`/`ExecuteCommand("trainer_run_mod_json", ...)`.
5. Run a 5 s heartbeat loop.

Payload parsers for `trainer_status`/`trainer_log`/`trainer_value_request`/
`trainer_value_response` are in Wand module 87404, which is **not present** in the
extracted bundle set; their exact byte layouts are therefore **UNKNOWN** from
this evidence. They are likely small FlatBuffers structs, but must be recovered
before a Rust client can decode trainer status/log/value requests. The legacy
`Value`/`ValueResponse` structs (56/24 bytes) are a plausible starting point for
`trainer_value_request`/`trainer_value_response` (the Rust probe already prints
`trainer_value_request payload too short: ... < 56` in `tophat_vendor_2.js`
offset ~1559).

---

## 5. Live probe result

`%APPDATA%\Wand\service-ports.json` exists and contains one stale entry:

```json
{ "pid": 19744, "port": 51155, "service": "tophat_service",
  "started": 1793498724, "target": "Mycopunk" }
```

- PID 19744 is **not** running.
- No process owns a TCP listener on port 51155 (`Get-NetTCPConnection -State
  Listen` returned nothing for that port; no Wand process owns any listener).
- No other registered PID has a listener.

**Conclusion: no live Tophat service exists on this machine.** Per the task
constraints, `cargo run --example tophat_probe` was **not** run and no process
was launched, killed, suspended, or injected.

---

## 6. Ordered implementation plan for `tophat_session.rs`

No full code; function signatures + state machine only.

### State

```rust
pub enum TophatPhase {
    Idle,
    ResolvingHost,
    Injecting,
    Connecting,        // port discovery + h2 handshake
    Streaming,         // StartStreamingConnection open
    LoadingPlugin,     // trainerlib plugin handle acquired
    Running,           // trainer_run_mod_json completed
    Stopping,
    Disconnected,
    Failed(TophatError),
}

pub struct TophatSession {
    phase: TophatPhase,
    target: Option<TophatTarget>,
    host_process_id: Option<u32>,
    external_host: bool,
    rpc: Option<TophatRpc>,          // from connectToTophat
    stream: Option<TophatStream>,    // server-stream handle
    plugin_handle: Option<u64>,
    pending_values: BTreeMap<String, oneshot::Receiver<ValueResult>>,
    heartbeat: Option<HeartbeatTask>,
    callbacks: TophatCallbacks,
}
```

### Inputs

```rust
pub struct TophatTarget {
    pub process: ProcessInfo,       // image path, pid, arch
    pub trainer_dll: PathBuf,
    pub plugin_dir: PathBuf,        // contains tophat_service_{arch}.dll etc.
    pub game_version: u32,
    pub variables: Vec<String>,     // from blueprint targets
    pub flags: u32,                 // SuspendProcess etc.
    pub connect_timeout: Duration,
    pub client_id: String,          // default "tophat-client"
}
```

### Public API

```rust
impl TophatSession {
    pub fn new(callbacks: TophatCallbacks) -> Self;
    pub async fn launch(&mut self, target: TophatTarget) -> Result<(), TophatError>;
    pub async fn set_value(&self, name: &str, value: f64) -> Result<ValueResult, TophatError>;
    pub fn on_initializing(&self, cb: impl Fn() + Send + Sync + 'static);
    pub fn on_ready(&self, cb: impl Fn() + Send + Sync + 'static);
    pub fn on_status(&self, cb: impl Fn(StatusMessage) + Send + Sync + 'static);
    pub fn on_log(&self, cb: impl Fn(LogMessage) + Send + Sync + 'static);
    pub fn on_value_request(&self, cb: impl Fn(ValueRequest) + Send + Sync + 'static);
    pub async fn stop(&mut self) -> Result<(), TophatError>;
    pub async fn dispose(&mut self);
}
```

### Ordered launch sequence

1. `ResolvingHost`: resolve `hostProcessId` / `externalHost` using the existing
   `detectGameEngine`/host resolver (reuse trainerlib host logic).
2. `Injecting`:
   - `grantFilePermissions([serviceDll, pluginDll, inputCaptureDll, trainerDll])`
   - `prepareProcess(gamePid, flags)` → suspended bool
   - `injectDll(hostProcessId, serviceDll)`
   - `createRemoteThread(hostProcessId, serviceDll, "StartTophatServiceThread")`
   - if suspended: `resumeProcess(gamePid)`
3. `Connecting`:
   - `connectToTophat(processName, hostProcessId, registryPath, heartbeatInterval=5s,
     signal, metadataInjector)` (polls service-ports.json, h2 handshake)
   - store `rpc`, `stream`
4. `Streaming`: call `stream.serverStream("StartStreamingConnection",
   streamRegistrationRequest(clientId), metadata{client-id})`; spawn task that
   decodes each `StreamMessage` and dispatches to channel callbacks; run heartbeat
   task every 5 s.
5. `LoadingPlugin`:
   - `rpc.loadPlugin("trainerlib", pluginDllPath)` → `plugin_handle`
6. `Running`:
   - `rpc.executeCommand("trainer_run_mod_json", {processId, gameVersion,
     trainerDllPath, initialVars})`
   - best-effort: `rpc.loadPlugin("input_capture", inputCaptureDll)` then
     `executeCommand("capture_start")`
7. On any error: stop heartbeat, cancel stream, unload plugin, resume process,
   move to `Failed`.

### Stop sequence

1. `Stopping`: executeCommand("trainer_stop_mod")
2. `rpc.unloadPlugin(plugin_handle)`
3. cancel stream, stop heartbeat, close rpc connection
4. `Disconnected`

### Value set flow

- `set_value(name, value)`:
  - allocate `requestId` (monotonic)
  - create oneshot channel stored in `pending_values[requestId]`
  - `executeCommand("trainer_push_client_req_from_json", {requestId, name, value})`
  - await oneshot (or abort signal) → `ValueResult { request_id, result }`
  - on `trainer_value_response` channel, parse payload and resolve matching
    pending request.

### Open work (UNKNOWN/INFERRED)

- Recover `parseStatusMessage`/`parseLogMessage`/`parseValueMessage`/
  `parseValueResponseMessage` layouts (likely in an unrecovered async bundle or
  native code) before decoding those channels.
- Confirm whether `service-ports.json` `started` is seconds or ms (sample
  `1793498724` looks like seconds).
- Confirm `StartTophatServiceThread` argument (none observed in JS).
- Confirm service thread's port selection and whether it supports external host
  PID registration.

---

## 7. Evidence citations

- `work/tophat_rpc_pretty.js` (offset 11813 `streamRegistrationRequest`,
  12640 method list, 12685 `StartStreamingConnection`).
- `work/tophat_load_schema_bundle.js` (LoadPlugin/UnloadPlugin/StreamMessage/
  ServerNotification/ServerCommand layouts).
- `work/tophat_schema_bundle.js` (Heartbeat/Command/Disconnect layouts).
- `work/research-asar/vendor..pnpm-2a95838e.4f0070c38f0e380d42c3.bundle.js`
  (offset 21224 manifest; 21428 `StartTophatServiceThread`; 21472
  `service-ports`).
- `work/research-asar/app-10739a52.04369e7cf64f5e4748bd.bundle.js`
  (offset 7169 manifest usage; 10522 grantFilePermissions; 10809 prepareProcess;
  11127 injectDll; 11359 createRemoteThread; 13358 registry path).
- `work/research-asar/vendor..pnpm-80c6c6c4.4761957f734da1f5bc74.bundle.js`
  (module 3926, `connectToTophat` port polling).
- `work/research-asar/vendor..pnpm-2a95838e.4f0070c38f0e380d42c3.bundle.js`
  (tophat trainer lifecycle, channels, parsers).
- Host: `%APPDATA%\Wand\service-ports.json` (stale entry, PID 19744).
- Host: `%APPDATA%\wemod\Local Storage\leveldb` (19 cached trainers, all
  `trainerlib`).