# Benchmark Findings — WandRebuild (Rust) vs Wand Lite Core

## Harness

- Path: `work/tools/benchmark_runs.ps1`
- Run: `powershell -ExecutionPolicy Bypass -File work/tools/benchmark_runs.ps1`
- 5 repetitions per scenario; reports median/min/max. Raw JSON: `work/bench_results.json`.
- Leaves `Wand`/`WandAuxiliaryService` running; kills its own `wand-rebuild-ui` children.

## 5-run results (medians)

| Scenario | Runs | Processes (med/min/max) | Private MiB (med/min/max) | WorkingSet MiB (med/min/max) | StartupLatencyMs (med/min/max) |
|---|---:|---:|---:|---:|---:|
| WandRebuild: idle library (16s settle) | 5 | 1/1/1 | 23.4/23.1/25.8 | 44.2/44.1/44.3 | 1732/1206/4307 |
| WandRebuild: active trainer session (hold 20s, sample ~18s) | 5 | 1/1/1 | 2.7/2.7/6.3 | 9.9/9.7/13.4 | n/a (headless) |
| Wand Lite Core: idle full tree | 5 | 7/7/7 | 872.1/869.5/894.3 | 1784.7/1783.5/1788.0 | n/a |

## Reduction vs Wand Lite Core (median)

- Idle library: private 97.3% (23.4 vs 872.1 MiB); working set 97.5% (44.2 vs 1784.7 MiB).
- Active trainer session (headless self-test): private 99.7% (2.7 vs 872.1 MiB); working set 99.4% (9.9 vs 1784.7 MiB).
- Startup latency to first responsive window (idle): median 1732 ms (min 1206, max 4307).

## In-game effect verification

Verdict: **Not confirmed** (trainer acknowledgement only).

Evidence collected:
- `work/research/evidence/effect/debugpipes.txt` — `WAND_DEBUG_PIPES=1` run of `--trainer-self-test`. Traffic shows only SetValue (frame type 2) outbound and ValueResponse (type 3, result 0) inbound; no trainer-pushed Value (type 2 inbound) frames reflecting game state.
- `work/research/evidence/effect/game_before.png`, `game_during.png`, `game_after.png`, `baseline1.png`, `baseline2.png` — screenshots. Full-frame pixel comparison showed ~53% of pixels changed between captures, dominated by scene animation; a 0.5 s baseline pair with no mod change was identical, so the capture/timing is not isolating mod effects. No reproducible HUD-only difference attributable to a mod.
- Trainer self-test reports all 5 mods `confirmed` (acknowledgement), but that confirms the trainer accepted the SetValue, not that game state changed.

Blocker: mods are toggles whose effects require gameplay triggers (taking damage for God Mode, spending for Add Money, firing for Ammo, etc.). No headless path triggers those, and screenshot comparison cannot separate mod effects from animation.

## Limits of this measurement

- Summed working sets double-count shared pages (OS shared libraries/DLLs), so totals overstate unique memory; private bytes are the fairer comparison.
- Process counts and memory fluctuate with machine state; single machine, no cross-hardware validation.
- Wand Lite Core was left running during WandRebuild runs, so system memory pressure/cache state differs from a clean machine.
- Active-session scenario uses the headless `--trainer-self-test` path (no window/rendering), so its memory is lower than an interactive UI session with a trainer panel open.
- Startup latency is polled at 250 ms and is approximate.
- Game was running during benchmarks; trainer DLL runs inside the game process, whose memory was not included in the WandRebuild figures.