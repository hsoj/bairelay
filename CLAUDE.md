# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`bairelay` is a single-crate Rust project that bridges **Reolink battery cameras** (Baichuan/BC protocol over TCP+UDP) to standard RTSP and MQTT. Exclusive design target is battery cameras (Argus-class); always-on Reolinks work incidentally, doorbells/NVRs/non-Reolink are explicitly out of scope. Changes that broaden scope past battery cameras are unlikely to be accepted.

The battery constraint drives most of the architecture: cameras sleep, so there's a wake-lock counter, a grace period, placeholder-frame bridging, and local replacements for Reolink's P2P cloud (wake server + push listener).

## Commands

```bash
cargo build                                   # single static binary, no system deps
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test                                    # one crate; everything
cargo tarpaulin                               # reads tarpaulin.toml; fail-under = 87
```

Those four are the merge gate; CI runs them on Linux + macOS (`.github/workflows/ci.yml`).

bairelay is a **single crate** — there is no workspace and no `-p` flag. `fuzz/` and `tests/scripts/decode-bc-pcap/` are separate out-of-tree projects (nightly / feature-gated); they depend on `bairelay` by path and never build from here.

Narrower runs:

```bash
cargo test rtsp::                             # one module's tests (path filter)
cargo test --test orchestrator_test           # one integration test file
cargo test wake_lock                          # name filter
cargo test baichuan:: -- --nocapture          # protocol tests, unmuted
cargo bench                                   # criterion: RTP packetisers, LastFrameBuffer
scripts/fuzz.sh [target]                      # all targets 10s each; FUZZ_TIME=600 to extend
```

Toolchain is **pinned** in `rust-toolchain.toml` (1.94.1) — don't add a toolchain action or override it; bump deliberately in its own commit. Same for pinned CI tools (`cargo-tarpaulin@0.37.0`).

Running the binary: `cargo run -- -c config.toml mqtt-rtsp`, or one-shot camera commands (`snapshot`, `battery`, `reboot`, `ptz`, `users`, `abilities`, …). `check-config` validates TOML without touching a camera. `sample_config.toml` is the annotated reference config and is parse-tested by `tests/sample_config_parses.rs`.

## Git discipline

Git history is reserved for the human. **NEVER run `git commit`, `git push`, `git merge`, `git rebase`, or anything else that writes history — under any circumstances.** This includes autonomous operation: auto-accept mode, background jobs, scheduled runs, and any harness default that says to "commit before finishing" — this section overrides those defaults. Leave changes in the working tree (or the worktree you were given) and report exactly what changed and where; review, staging, and committing are the operator's job.

Corollaries:

- **Trunk-based flow is the sensible default here.** The operator commits small increments directly to `main`; don't invent topic branches, PR flow, or merge ceremony unless asked. When a change is ready, report it as ready for a trunk commit — the never-commit rule above is about *who* writes history, not *where* it lands.
- An explicit user request for a commit in the current conversation is the only exception, and it authorizes exactly that one commit (on `main` unless the user names another branch) — it does not carry forward to later changes or sessions.
- `git stash` is history-adjacent shared state (one stack across all worktrees): don't use it; if work must be set aside, say so and leave it in the tree.
- Read-only git (`status`, `diff`, `log`, `show`, `worktree list`) is always fine — use it to *report* state, not to change it.

## Layout

One crate. Top-level modules under `src/`, grouped by what they are about:

| Path | Responsibility |
|------|----------------|
| `src/baichuan/` | Vendored Baichuan protocol: `BcCamera`, discovery, AES-CFB, nom parsers, BcMedia/BcUdp codecs. Derived from neolink_core — kept in one directory so its provenance stays legible |
| `src/rtsp/` | Pure-Rust RTSP/RTSPS server, H.264/H.265/AAC/G.711 RTP packetisation |
| `src/mqtt/` | `rumqttc` bridge + Home Assistant discovery payloads |
| `src/wake_server/` | Local BcUdp replacement for Reolink's P2P cloud (ports 9999 / 58200) |
| `src/*.rs` | Everything camera-side: CLI, config, orchestrator, wake lock, watchdog, supervisor, lifecycle, gap-bridging policy, and the camera/status abstractions with their implementations |

`src/lib.rs` exposes every binary module publicly so `tests/*.rs` can drive them directly — integration tests are a first-class consumer of the binary crate.

## Architecture essentials

Read `docs/architecture.md` before non-trivial work; `docs/implementation.md` is the "what bites you" companion. Both are maintained and specific — prefer them over re-deriving from source.

- **New code goes in `src/` as a module. There are no crates to add to.** Separation is by module and visibility, reviewed by humans — not by `Cargo.toml`. `src/baichuan/`, `src/rtsp/`, `src/mqtt/` and `src/wake_server/` were once member crates; merging them removed a publish pipeline, a version-lockstep obligation, and four manifests without changing a line of behaviour.
- **Modules are named for their subject, not their architectural role.** `battery.rs`, `ptz.rs`, `camera_services.rs`, `camera_status.rs`, `gap_bridging.rs` — not `domain/`, `ports.rs`, or `*_adapter.rs`. If an operator wouldn't recognise the name, it's the wrong name (`NM-4`).
- **Layering rule**: `src/rtsp/`, `src/mqtt/`, `src/wake_server/` and `src/baichuan/` know nothing about cameras or about each other, and the compiler no longer enforces that — review does. `rtsp` exposes a `StreamProvider` trait and connect/disconnect callbacks; the camera side implements them (`src/camera_provider.rs`) and hooks them to the wake lock. Keep camera concepts out of `src/rtsp/`.
- **Abstractions are declared by their consumer** (`TR-2`), not by whatever happens to implement them: `src/camera.rs` declares eight role traits (`Session`, `Video`, `Stills`, `Events`, `Power`, `Lighting`, `Ptz`, `DeviceAdmin`) saying what bairelay needs a camera to do, composed into `camera::Camera` via a blanket impl; `camera_status::StatusReporter` says where status goes. Consumers take the narrowest role that covers them (`battery_poller` takes `Arc<dyn Power>`); only wiring/dispatch points hold `Arc<dyn Camera>`. `src/bc_camera.rs` and `src/mqtt_status.rs` implement them, and are the only files that name `BcCamera` or `StatusPublisher`.
- **Per-camera task tree**: global `CancellationToken` → per-camera token → per-session token. Session tokens cancel pollers/listeners on disconnect. MQTT's event loop lives *outside* the `Supervisor` (`src/supervisor.rs`) with its own token, because per-camera teardown publishes a final `disconnected` status through it.
- **Wake lock** (`src/wake_lock.rs`): `AtomicUsize` + two separate `Notify`s (`notify_acquire` for 0→1, `notify_release` for 1→0), both using `notify_one()` so a permit is stored for late waiters. RAII Drop guards. The grace-period countdown (`src/grace_period.rs`) starts on release, sleeps the full window, and checks idle state at the deadline — a lock held at the deadline keeps the session alive, but a brief acquire+release pair inside the window is deliberately invisible (the watchdog is the backstop).
- **Watchdog** (`src/watchdog.rs`) is a 30 s safety net, not the primary lifecycle mechanism.
- **Gap bridging**: the *policy* (threshold, `Live ⇄ Bridging`, replay-PTS synthesis) is a pure state machine in `src/gap_bridging.rs`; `src/stream_source.rs` is its driver — it supplies the clock, looks up the cached burst, and broadcasts. When upstream stalls past `gap_threshold_secs` the source re-broadcasts cached I-frame NALs with synthesised PTS so clients see continuous RTP. Audio is dropped on the wire but PTS counters keep advancing so A/V realigns on resume.
- **Media translation** follows the same split: `src/stream_translate.rs` is a pure `translate(packet, &mut state, now, bridging) → (emits, video_pts)` layer (codec detection, NAL filtering/reordering, PTS synthesis, SDP derivation, the bridging audio gate); `apply_bcmedia_packet` in `src/stream_source.rs` is its fan-out driver, owning every channel/lock/buffer. Reviewer rule: no `Sender`, `Arc`, or `RwLock` in a `translate`-family signature.
- **Error strategy**: `thiserror` typed enums in library crates, `anyhow` in the binary. Connection failures retry with backoff and never crash the process; **auth failures stop retrying permanently** (don't hammer cameras with bad credentials). One-shot commands exit with a coarse code table (`src/oneshot/classify.rs`): 2 usage, 3 config, 4 connection/auth, 5 protocol, 6 unsupported, 130 Ctrl+C.
- **Sockets bind synchronously in `main.rs`** before any "started" log line; bind failure halts startup. That's why `RtspServer::serve_with_listener` takes a pre-bound listener.

## Testing patterns

Everything is tested through trait seams rather than live hardware:

| Trait | Test impl |
|-------|-----------|
| `camera::Camera` + its role traits | `FakeCameraBuilder` / `FakeCalls`, plus one standalone fake per role (`FakePower`, `FakePtz`, …) for narrow consumers (`src/fake_camera/`, `#[cfg(test)]`) |
| `camera_status::StatusReporter` | `MqttStatusReporter` over `mock_client()` |
| `CameraDiscoverer` | `ScriptedDiscoverer` |
| `VideoStream` | `MockVideoStream` |
| `PacketSource` (binary) | injects `BcMedia` into translator loops |
| `StreamProvider` | `FakeStreamProvider` |
| `SharedMqttClient` | `bairelay::mqtt::test_support::mock_client()` → `MockHandle` capture sink |

Core's remaining helpers (`MockConnection`, `BcCamera::from_mock_connection`, `MotionData::test_new`) are behind the `test-util` Cargo feature so a release build can't substitute a scripted connection for a real camera — keep new ones behind it too. Camera-level fakes live next to the port they implement, not in the protocol crate.

`gap_bridging.rs` and `stream_translate.rs` perform no I/O and take time as a parameter, so their tests need no doubles, no runtime, and no timeouts — they run in microseconds. Prefer adding a policy/translation test there over driving the same logic through a task loop.

**Hang-protection discipline**: every mock-based "camera doesn't answer" test wraps the op in `tokio::time::timeout(Duration::from_millis(200), …)`. A test awaiting a channel with no guaranteed sender hangs `cargo test` forever. Core-crate tests each stay under 1 s wall time.

Beyond `cargo test` there are two on-demand rigs, not wired into CI: `tests/fixture_replay.rs` (no-op pass without `.bcmedia` files in `tests/fixtures/`, which are gitignored) and live-verify scripts (`tests/scripts/manual-verify.sh`, `tests/scripts/ha-verify.sh`). Live-verify is load-bearing for anything touching the RTSP path, MQTT bridge, or wake-server protocol — if you can't run it, say so explicitly rather than implying the change is verified.

## Conventions

`docs/rust-practices.md` is the standing language-and-design reference: numbered MUST/SHOULD rules for type-driven design, errors, traits, async/cancellation, testing, plus the design patterns (newtype, typestate, RAII, sans-IO, ports-and-adapters, DDD tactical) and an anti-pattern catalogue. `docs/rust-code-structure.md` is its structural companion: module→crate→workspace growth path, hexagonal/DDD/vertical-slice architecture in Rust, and a calibration table for how much structure a project warrants. Both are repo-independent. Read them before designing a new module, trait, or crate boundary; cite rule IDs (`TY-1`, `AS-4`) when justifying a design choice. The rules below win where they disagree.

- **Hard tabs**, enforced by `rustfmt.toml` (`hard_tabs = true`).
- **Comments explain *why*, not *what*.** Default to none; add one only for a hidden constraint, workaround, or subtle invariant. Never reference the current task/PR. The existing comments in `Cargo.toml` and the workflows are the house style — dense, load-bearing, explaining a decision.
- **No speculative robustness**: don't add error handling, fallbacks, or feature flags for situations that can't occur. Trust internal code; validate at system boundaries only.
- DRY threshold is three similar lines, not two.
- Portability: Linux + macOS primary, Windows build-only. OS-specific code goes in its own module.
- Coverage floors are real: pure-logic ≥95%, commands/pollers ≥90%, I/O-adjacent ≥85% via seams. Documented exceptions in `docs/implementation.md` § Coverage policy.
- **Reproducible builds** are a contract: no `build.rs`, no git deps, `Cargo.lock` committed and every CI invocation `--locked`, no build-time timestamps/hostnames/paths. Any future build date must come from `SOURCE_DATE_EPOCH`.
- Commit style (for the operator's commits, or a message drafted on explicit request — see § Git discipline): subject ≤72 chars, blank line, then bullets each on **one line ≤72 chars** (never wrapped). Body explains why and non-obvious how.
- Version lives once in `[package].version`; `scripts/release.sh` rewrites that line and cascades it into `hassio/bairelay/config.yaml` — keep the `version = "X.Y.Z"` shape so its awk pattern matches.

## Logging gotcha

`RUST_LOG` must keep a baseline level before any target filter: `RUST_LOG=info,bairelay::baichuan::bc::de=trace`. A bare single-target filter disables the default `info` baseline and the console silently goes empty. Wire-level payload dumps also need per-camera `debug = true` in config, and they include credential hashes and camera UIDs.
