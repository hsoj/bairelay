# Action Plan

The single ordered plan. Consolidates `docs/remediation-plan.md` (defects), `docs/decoupling-plan.md` (testability), and the open threads in `docs/hexagonal-refactor.md` (structure).

Ordered by the project's stated priorities, in order: **stability → developer experience → design best practice → composition-based trait design**. Where an item serves several, it is placed by the highest one it serves.

**Sequencing lives here and nowhere else.** After this lands:

| Doc | New role |
|---|---|
| `remediation-plan.md` | Findings archive — evidence and diagnosis, no sequencing |
| `decoupling-plan.md` | Design rationale for Stage 4 — target shapes, no sequencing |
| `hexagonal-refactor.md` | Historical record of phases 1–5 — do not re-plan from it |

---

## The gate nobody noticed

`release.yml:269` publishes `bairelay` to crates.io on every non-draft release. `src/lib.rs` declares all ~35 internal modules `pub` — deliberately, so `tests/*.rs` can drive them (`CLAUDE.md` says exactly this).

Those two facts together mean **the entire internal module tree is public API on crates.io**. Consequences, all currently being paid:

- Splitting `trait Camera` (S4-2) is semver-major.
- Deleting dead protocol modules (S2-1) is semver-major.
- Changing a `translate` signature (S4-1) is semver-major.
- `missing_docs` across the surface is 196 undocumented *public* items (old P2-1).
- Every enum variant added to the reverse-engineered protocol error is semver-major (old P2-2).

Nobody consumes that surface. It is public for the test harness. The cost is that every item in Stages 2–4 carries a semver argument it does not deserve, and two P2 items exist only to service an API commitment that was never intended.

**S0-1 removes this, and it comes first because it makes everything after it cheaper.**

---

# Stage 0 — Unblock

## S0-1. Declare the library surface unstable · S · stability + DX

Two viable shapes:

**A — Declare, don't restrict (recommended).** Keep `pub mod`, add `#![doc(hidden)]` semantics per module plus an explicit `//!` policy block in `lib.rs` and a line in `README.md`: *the binary CLI and its config are the stable interface; the library surface carries no semver guarantee and exists for the test harness.* Near-zero churn, honest, and standard practice for binary crates that also publish.

**B — Feature-gate.** `#[cfg(any(test, feature = "internals"))] pub mod …`, CI runs `cargo test --features internals`. Enforced by the compiler rather than by declaration, at the cost of a feature flag on every module and a CI flag that must never be forgotten.

Take A now; B only if someone actually depends on the lib surface and needs the wall.

**This dissolves rather than resolves two old findings.** P2-1 (196 undocumented public items) and P2-2 (`#[non_exhaustive]` on 0 of 46 public enums — the count is 0 today, not 1) exist to service an API commitment that was never real. Neither is worth doing on a surface with no consumers. `#[non_exhaustive]` stays worth applying to genuinely public *config* types (`config::Config` and friends parse operator TOML), and nothing else.

**Fix the cosmetic staleness in the same PR:** `release.yml:290` names the step "Publish bairelay-\* + bairelay" — the `bairelay-*` member crates were merged away. `scripts/publish-crates.sh` is already correct; only the step name lies.

## S0-2. CI: supply chain, MSRV, fuzz smoke · S · stability

Three workflow-only additions, no source risk (old P1-1, P1-2, P1-7):

- **`cargo-deny`** + `deny.toml` — advisories, licences, bans. Neither exists today. 379 crates including `rustls` and `aws-lc-rs`, a committed `Cargo.lock`, and every CI invocation `--locked`: a pinned lockfile with no advisory feed holds known-vulnerable versions indefinitely and silently. The `bans` check also prevents the duplicate-dependency regression (old P2-4) without a migration.
- **MSRV job** — `rust-version = "1.93"`, toolchain pinned to `1.94.1`, nothing builds against 1.93. Either verify the claim in CI or delete it. Verifying is one job; the claim is worth keeping while the crate publishes.
- **Nightly fuzz smoke** — eight targets covering the entire untrusted-input surface (`bc_deserialize`, `bcudp_deserialize`, `bcxml_try_parse`, `nal_split_decode`, `aac_parse_adts`, `adpcm_decode_block`, `udp_flow_state`, `wake_server_decode_discovery`) plus `scripts/fuzz.sh`, with zero CI wiring. At the script's default 10 s/target this turns dormant assets into a live regression net.

Do S0-2 before Stage 1 so the poison sweep lands under a gate that can catch what it disturbs.

---

# Stage 1 — Stability defects

## S1-1. `src/sync.rs` + poison sweep · M · **highest stability item**

**40 `expect("… poisoned")` sites remain** in `src/rtsp/` and `src/wake_server/` — verified, not inherited from the doc. `SessionRegistry` is shared by every RTSP connection and `LastFrameBuffer` by every session, so one panic under those locks takes down the whole server rather than one client.

`src/stream_source.rs:82–105` documents precisely why this is wrong — *"cascades a single bug across every other holder"* — and ships `lock_recover` / `rlock_recover` / `wlock_recover` / `RwLockPoisonRecover` / `MutexPoisonRecover` to fix it. The binary uses them. The RTSP and wake-server paths do not. `src/wake_server/route.rs:149` already uses `unwrap_or_else(|p| p.into_inner())`, so the idiom exists in-tree and is simply not applied uniformly.

`src/sync.rs` still does not exist, and the helpers still live in the largest module in the tree. **A P0 has been blocked on a file move.** Move first, then sweep.

Leave the 11 sites in `fake_camera.rs` alone — test-only.

## S1-2. `#[must_use]` on the RAII guards · S · stability

Two `#[must_use]` in ~80 kloc. The guards are exactly where it matters: `WakeLockGuard` (`src/wake_lock.rs:26`) and `SubscriptionHandle`.

```rust
cam.wake_lock().acquire();   // acquires and instantly releases
```

Correct-looking, silently wrong, and the wake lock is the core of the battery-camera design — a dropped guard means a camera that sleeps through the thing that wanted it awake. This is filed as API hygiene in the old plan; it is a stability defect with a one-attribute fix.

## S1-3. `RUST_LOG` test data race · S · stability

`src/cli_convert.rs:358,365,1016,1021,1025`. Two tests `set_var` / `remove_var` on `RUST_LOG` and run in parallel threads of the same process — the exact UB that made `set_var` `unsafe`. The `// SAFETY (single-threaded test…)` comment asserts something `cargo test` does not provide.

Fix by making `verbosity_env_filter` take the value as a parameter. That removes the `unsafe` entirely and makes the function testable as a value — so it also belongs to Stage 3's pattern.

## S1-4. Version literal and large futures · S · stability

- `src/rtsp/protocol/message.rs:161` hardcodes `Server: bairelay/0.1.0` against a crate at 1.1.2, breaking the repo's own "version lives once" rule. Use `concat!("bairelay/", env!("CARGO_PKG_VERSION"))`.
- `src/oneshot/runner.rs:50` is a 33,992-byte future; hundreds sit in the 16–36 KB range. With one task tree per camera plus per-session tasks these inflate every `select!` arm holding them. `Box::pin` the handful clippy's `large_futures` names.

---

# Stage 2 — Delete before refactoring

Deleting first means every later stage carries less.

## S2-1. Remove the dead protocol command surface · S · DX

Six `bc_protocol` modules with **zero** referenced public functions across `src/`, `tests/`, `benches/`, `fuzz/`, and `tests/scripts/decode-bc-pcap/`:

| Module | Lines | Public fns |
|---|---:|---:|
| `talk.rs` | 904 | 5/5 |
| `email.rs` | 599 | 8/8 |
| `pushinfo.rs` | 148 | 3/3 |
| `stream_info.rs` | 122 | 1/1 |
| `uid.rs` | 114 | 2/2 |
| `ping.rs` | 97 | 1/1 |
| **Total** | **1984** | |

Trim in the same pass: `time.rs` `get_dst`/`get_time`, `motion.rs` `await_start`/`await_stop`/`motion_detected`/`motion_detected_within`/`consume_motion_events`, `ptz.rs` `get_zoom`, `battery.rs` `monitor_battery`, `floodlight.rs` `get_floodlight_tasks`, `ledstate.rs` `set_ledstate`, `pirstate.rs` `set_pirstate`.

**`talk.rs` is the reason this stage exists.** `hexagonal-refactor.md` § Deferred records its blocking `crossbeam_channel::recv()` inside `BufferedStream::fill_buf`, reachable from `async fn talk_stream`, as a real `AS-5` violation — then declines to fix it *because it is unreachable from the shipped binary and two-way audio cannot be verified without hardware*.

Unreachable is not a reason to keep code unfixed. It is a reason to delete it. Deleting resolves the `AS-5` violation permanently, drops `crossbeam-channel` from the dependency tree, removes ~500 lines of tests that only exercise dead code, and dissolves the "cannot verify without hardware" blocker by removing the thing that needed verifying. Two-way audio is not in scope — `CLAUDE.md` lists nine MQTT features and talk is not among them.

**Verification, re-run at PR time and not trusted from this doc:**

1. `grep -rE "(\.|BcCamera::|Self::)<fn>\(" src/ tests/ benches/` — empty. (Note: `bc_camera.rs` forwards via UFCS, so a bare `.<fn>(` pattern gives false positives for *live* code. Match all three call forms.)
2. `fuzz/` and `tests/scripts/decode-bc-pcap/` build against the trimmed crate. Both depend on `bairelay` by path and today touch only `baichuan::{bc,bcudp,fuzz_api,pcap_decode_api}` and `rtsp::codec`.
3. `cargo tarpaulin` — coverage must not **drop**. Removing tested dead code should raise it; a drop means something live went with it.
4. Message-ID constants in `bc/model.rs` **stay**. They are protocol vocabulary, documented in `docs/baichuan-protocol.md` §5. Deleting a `pub const MSG_ID_*` loses knowledge; deleting an unused RPC wrapper does not.

## S2-2. Reconcile the stale docs · S · DX

`remediation-plan.md` P3-3 and `code-paths.md` §12 both describe `CameraDriver` as a 40-method trait awaiting a decision. **That trait was deleted in phase 1.** Its replacement, consumer-defined `trait Camera` (`src/camera.rs:63`), has 33 methods — so the *port-direction* complaint (`TR-2`) closed and the *port-width* complaint (`TR-3`) did not. Both docs must strike the naming, keep the width finding, and re-point it at S4-2.

Also stale: `crates/core` / `crates/rtsp` paths throughout both docs (single crate since the merge); `stream_source.rs` and `camera.rs` line counts in P3-6; the §12 seam diagram still painting `CameraDriver` red.

---

# Stage 3 — Pure-function extraction

No boundary moves. Each converts a decision that currently needs task choreography into one that needs a table. All four are independently reviewable, need no hardware, and follow a pattern already in the tree — `classify_battery_tick` / `advance_battery_counter` (`camera_tasks.rs:207`) and `classify_keepalive_tick` / `advance_keepalive_counter` (`camera.rs:262`).

## S3-1. Pacer re-anchor policy · S · best ratio in the plan

`media_pacer_task` (`src/stream_source.rs:967`) buries three rules in an async loop with channels and `sleep_until`:

```rust
let target = match next_emit_at {
    Some(t) if t > now + max_lead      => now,   // future cap
    Some(t) if snap_on_past && t < now => now,   // dry-queue snap
    Some(t)                            => t,
    None                               => now + initial_latency,
};
```

Surrounding comments cite mpv's "Invalid audio PTS" and a 1 ms/packet drift that skewed the RTCP NTP↔RTP slope by 1.6% over a 5 s window — hard-won behaviour, verifiable today only through `#[tokio::test(start_paused)]` plus channel choreography.

**Target:** `fn next_target(cursor: Option<Instant>, now: Instant, max_lead: Duration, initial_latency: Duration, snap_on_past: bool) -> Instant`. The full matrix, including the audio/video `snap_on_past` asymmetry, becomes a table test with no runtime.

## S3-2. Watchdog disconnect predicate · XS

`src/watchdog.rs:63-77` inlines the decision the hexagonal blueprint said to extract ("policy fn → domain; 30 s sweep task → app" — the sweep landed, the policy did not). Its comment documents a real race the predicate was tuned to avoid: the watchdog tearing sessions a few hundred ms before an MQTT `control/wakeup` lands.

**Target:** `fn should_disconnect(idle_disconnect: bool, connected: bool, idle_for: Option<Duration>, grace: Duration) -> bool`.

## S3-3. Config warnings as values · S

`tests/config_test.rs:1407` states it outright: *"Runs without panicking; tracing output isn't captured here."* That is `TS-3` by name — the anti-pattern catalogue's "tests asserting mock behaviour" entry, in its weakest form: asserting nothing at all.

Five functions take `&Config`, return `()`, and log inline: `warn_deprecated_pause_fields:864`, `warn_neolink_compat_fields:925`, `warn_wire_debug_enabled:994`, `warn_users_without_tls:1010`, `warn_idle_timeout_below_prune_floor:1055`. Across 35 `warn_*` references, nothing asserts which warning fired — including `warn_users_without_tls`, whose text is operator-facing security guidance.

**Target:** return `Vec<ConfigWarning>`; `check-config` and startup do the logging.

## S3-4. `preview_poller` off the concrete handle · S

Five of six `camera_tasks.rs` entry points take `Arc<dyn Camera>`. `preview_poller:298` alone takes `Arc<CameraHandle>`, for `is_connected()` and `bc_camera()`.

It is tested — four tests, including connected and error paths — but each builds a real `CameraHandle` where sibling tasks build a `FakeCamera`. *Having tests is not the same as the binding earning its keep.* It buys a heavier fixture and an inconsistency a reader must explain.

**Target:** a two-method `PreviewHost` capability that `CameraHandle` implements, or hoist the connected-check to the caller and take `Arc<dyn Camera>`.

## S3-5. Finish the live-verify marker contract · S

`tests/scripts/manual-verify.sh` drives the daemon by grepping stdout; reword a marker and it stalls its poll window and reports a *misleading* FAIL. `src/log_capture.rs` landed with two markers pinned. Remaining: the two `src/camera.rs` lifecycle markers (`Grace period expired, disconnecting`, `Disconnected`) and `RTSP server listening`.

`log_capture.rs` is not a `TS-3` violation despite looking like one — there, the log text *is* the contract. Keep it, and say so in its module docs so a future reviewer does not "fix" it.

---

# Stage 4 — Composition

The two items that reshape a boundary. Both land alone.

## S4-1. Sans-IO the BcMedia→Frame translation · L · highest value in the plan

`hexagonal-refactor.md` §2 named three altitudes tangled in `stream_source.rs`: bridging policy, tokio plumbing, and BcMedia translation. Phase 2 extracted the first — `gap_bridging.rs` is pure, takes `Instant` as a parameter, tests in microseconds. The third is untouched.

`apply_bcmedia_packet:1498` and its four handlers (`handle_iframe:1546`, `handle_pframe:1830`, `handle_aac:2029`, `handle_adpcm:2296`) each take a `broadcast::Sender<Frame>`, two `mpsc::Sender<PacedFrame>`, `Arc<LastFrameBuffer>`, `Arc<RwLock<SdpParams>>`, `Arc<RwLock<AudioPresence>>`, and perform the side effects inline. Buried inside: codec detection from the first decisive NAL, parameter-set extraction, PTS synthesis and wraparound, AAC AOT/sample-rate derivation, and the audio gate during bridging. A/V desync is the product's visible failure mode and this is where it lives.

**Target** — the shape `gap_bridging.rs` already proves:

```rust
pub enum Emit {
    Video(Frame), Audio(Frame),
    PaceVideo(PacedFrame), PaceAudio(PacedFrame),
    SdpParams(SdpParams), LastFrame(Vec<u8>), AudioSeen,
}

pub fn translate(packet: &BcMedia, state: &mut StreamTranslatorState, bridging: bool)
    -> (SmallVec<[Emit; 4]>, Option<u32>);
```

The driver becomes ~20 lines of fan-out. Codec detection and PTS edges become table tests; `tests/fixture_replay.rs` gains the ability to assert on decision *sequences*; wraparound becomes a property test (`TS-5`).

`Emit` is a closed set, so it is an `enum` and not a trait — `TY-5`, and the composition table's "subtype polymorphism, closed set → `enum` + `match`". A new emission kind should break every `match`; that is the point.

**Live-verify:** on the RTSP path. Lands alone, gated by `tests/scripts/manual-verify.sh`. If hardware is unavailable, say so in the PR rather than implying verification.

Carry S1-1's `src/sync.rs` move in this PR if it has not already landed — this opens the file anyway.

## S4-2. Decompose `trait Camera` into flat capabilities · L

`src/camera.rs:63` is 33 methods. Measured distinct-method usage per consumer:

| Consumer | Methods |
|---|---|
| `mqtt_dispatch.rs` | 15 |
| `camera_tasks.rs` (five pollers/listeners) | 6 |
| `oneshot/ptz.rs` | 5 |
| `oneshot/users.rs` | 4 |
| `oneshot/snapshot.rs` | 3 |
| `stream_source.rs` | 2 |
| `startup_wake.rs` | 1 |
| 11 other `oneshot/*` handlers | 1–2 each |

`fake_camera.rs` is **751 lines** to satisfy a trait whose median consumer needs two methods. Every fixture pays that entry fee; every new method taxes every fake.

### Correction: no supertraits

An earlier sketch in `decoupling-plan.md` proposed `trait VideoSource: CameraSession`, `trait CameraTelemetry: CameraSession`, and so on. **That is inheritance in trait clothing** and it is withdrawn. `rust-practices.md` § composition over inheritance is explicit: *"Prefer 'has-a' and 'can-do' to 'is-a'. Model capabilities rather than taxonomies."* A supertrait chain re-creates the taxonomy — every role drags `CameraSession` along whether or not it needs it, and a fake for a two-method consumer is back to implementing the base.

**Flat, independent capabilities, composed at the use site:**

```rust
pub trait VideoSource:      Send + Sync { /* start_video, stop_video */ }
pub trait CameraTelemetry:  Send + Sync { /* battery, pir, floodlight, motion */ }
pub trait CameraControl:    Send + Sync { /* ptz, light, siren, reboot */ }
pub trait CameraAdmin:      Send + Sync { /* users, services, time, version */ }
pub trait CameraSession:    Send + Sync { /* end_session, keepalive_probe, capabilities */ }
```

No trait names another. Consumers state exactly what they can do:

```rust
async fn battery_poller(cam: Arc<dyn CameraTelemetry>, …)          // one capability
async fn dispatch_control(cam: &(dyn CameraControl + CameraAdmin), …)  // two, composed
```

`BcCamera` implements each independently — five small `impl` blocks, same forwarding shape as today, no blanket forwarding impl (that construct is what made `CameraDriver` mirror its producer in the first place). Fakes compose: `FakeTelemetry`, `FakeControl`, and a test builds only what it needs. The 751-line monolith stops being the price of testing a two-method consumer.

Where a consumer genuinely needs several capabilities, hold several `Arc<dyn _>` **fields** and delegate explicitly — the composition table's "code reuse via is-a → hold the other type as a field, delegate explicitly." Three visible lines beat an invisible vtable walk, and `Deref` is out under `TR-6` regardless.

### Vendored types, partially reopened

`hexagonal-refactor.md` § Deferred kept vendored BC types (`RfAlarmCfg`, `LedState`, `VersionInfo`, `UserList`, `AbilityInfo`, `MotionData`, `Direction`, `LightState`) in the port signatures on the grounds that local twins would be field-for-field mapping with no second implementation. That holds for most and is not reopened wholesale. It stops holding where a type forces a fake to construct BC XML — so split the capabilities first, then re-measure. Anything reachable only from `CameraAdmin` (one consumer, no plausible second backend) keeps its BC type indefinitely.

**Sequenced after S4-1** so the split is measured against a `stream_source.rs` that has stopped moving.

---

# Deferred, with triggers

Recorded as decisions, not omissions.

| Item | Trigger to revisit |
|---|---|
| **Discovery build/classify split** (`discovery.rs`, 3295 lines, only leaf predicates tested) | When a discovery change is already needed. Target shape in `decoupling-plan.md` D3; XML shapes now specified in `baichuan-protocol.md` §9. Failure mode is "camera not found", not "video is wrong". |
| **`rand` 0.8 → 0.9 migration** (old P2-4) | `cargo-deny` `bans` (S0-2) prevents regression without it. Do it when `rand` is touched anyway. |
| **`[lints]` + `missing_docs` sweep** (old P2-1) | Only if the lib surface becomes a real API. S0-1 removes the reason. |
| **`#[non_exhaustive]` sweep** (old P2-2) | Apply to `config::*` types only — they parse operator TOML. The blanket sweep dies with S0-1. |
| **Splitting `rtsp/server/connection.rs`** | Already decomposed correctly — pure framing helpers unit-tested, handlers driven over a `DuplexStream`. Extract `build_transport:952` on the next substantive change to `handle_setup:647`. |
| **A domain crate** (phase 4, built and reverted) | A second binary, or an external consumer of the policy code. S4-2 makes the wall *less* necessary, not more. |

---

# Sequencing

| Batch | Items | Gate | Hardware |
|---|---|---|---|
| **0** | S0-1, S0-2 | CI green | no |
| **1** | S1-1, S1-2, S1-3, S1-4 | `cargo test` + `clippy` | no |
| **2** | S2-1, S2-2 | `cargo test` + tarpaulin non-decreasing + out-of-tree builds | no |
| **3** | S3-1 … S3-5 | `cargo test` | no |
| **4a** | S4-1 (+ S1-1 if not landed) | `manual-verify.sh` | **yes** |
| **4b** | S4-2 | `cargo test` + `ha-verify.sh` | broker |

Batches 0–3 need no camera. That is deliberate: everything verifiable without hardware lands before the two items that are not, so a hardware-blocked project still gets the stability and DX wins.

Within a batch, items are independent and can land in any order or in parallel.

---

# Review checklist

Applies to any PR touching the camera port, the stream path, or the protocol module.

**Composition**

1. **New trait?** Names a capability (`can-do`), not a taxonomy (`is-a`). No supertrait used for reuse. (S4-2)
2. **New `Camera`-family method?** Lands on exactly one capability trait, and the PR names the consumer that needs it. A method on `CameraSession` must justify that *every* consumer needs it.
3. **Reuse between types?** A held field with explicit delegation, never `Deref`. (`TR-6`)
4. **Closed set of outcomes?** `enum` + exhaustive `match`, so adding a case breaks every site. (`TY-5`)

**Testability**

5. **New logic in a `translate`/`handle` function?** No `Sender`, `Arc`, or `RwLock` in the signature. If it needs one, it belongs in the driver. (S4-1)
6. **New decision inside an async loop?** Extracted as a pure `fn` taking time and state as parameters. (S3-1, S3-2)
7. **New `warn_*` or diagnostic?** Returns a value; the caller logs. (S3-3)
8. **New test asserting "does not panic"?** Rejected. If that is all that can be asserted, the unit is the wrong shape. (`TS-3`)

**Stability**

9. **New `pub fn` on `BcCamera`?** Has a caller in `src/` in the same PR. (S2-1)
10. **New `Arc<Mutex<_>>` / `Arc<RwLock<_>>` crossing a task boundary?** Justified against `OW-5` and `AS-4`, uses the `src/sync.rs` poison-recovering helpers, and is added to the shared-state map in `code-paths.md` §13. (S1-1)
11. **New RAII guard?** `#[must_use]`. (S1-2)
12. **New log string that `manual-verify.sh` greps?** Pinned by a `log_capture` test. (S3-5)
