# Decoupling Plan

Ranked by **architectural blast radius**, descending — the axis a reviewer needs, not the axis that says what to do first. Tier 1 reshapes boundaries and must be reviewed as design changes. Tier 2 removes structure. Tier 3 extracts pure functions without moving a boundary and can be reviewed as ordinary diffs.

Value and effort are separate columns because they do not correlate with blast radius: the cheapest items in Tier 3 are worth more per hour than anything in Tier 1, and the single highest-value item (D2) sits in the middle.

Companions: `docs/rust-practices.md` (rule IDs), `docs/hexagonal-refactor.md` (what phases 1–3 delivered), `docs/remediation-plan.md` (open defects).

---

## Correction to the record

`docs/remediation-plan.md` P3-3 and `docs/code-paths.md` §12 describe `CameraDriver` as a 40-method trait awaiting a decision. **That trait is gone** — phase 1 deleted it and replaced it with consumer-defined `trait Camera` (`src/camera.rs:63`).

But P3-3 had two complaints, and only one was addressed:

| P3-3 complaint | Rule | Status |
|---|---|---|
| Trait lives at the producer, mirrors `BcCamera` | `TR-2` | ✅ Fixed — `Camera` is consumer-defined |
| 40 methods; no consumer needs all; every fake pays for all | `TR-3` | ❌ **Not fixed** — the replacement has 33 |

The port *direction* was corrected; the port *width* was carried over. Both source docs need editing: strike the `CameraDriver` naming, keep the width complaint, and re-point it at `trait Camera`. This is D1 below.

---

## Ranking

| # | Change | Blast radius | Value | Effort | Rules |
|---|--------|--------------|-------|--------|-------|
| **D1** | Split `trait Camera` into role traits | **Highest** — every consumer, `bc_camera.rs`, `fake_camera.rs` | High | L | `TR-3`, `TR-2` |
| **D2** | Sans-IO the BcMedia→Frame translation | High — `stream_source.rs` internals, RTSP data path | **Highest** | L | sans-IO, `TS-1`, `TS-5` |
| **D3** | Split discovery into build/classify + driver | High — `discovery.rs`, `CameraDiscoverer` | Medium | L | sans-IO, `TS-1` |
| **D4** | Delete the dead protocol command surface | Medium — removes 6 modules + 1 dependency | High | S | `MD-1`, `AS-5`, `DP-3` |
| **D5** | Extract the pacer re-anchor policy | Low — one function | High | S | sans-IO, `TS-6` |
| **D6** | Config warnings as values | Low — 5 signatures + call sites | Medium | S | `TS-3` |
| **D7** | Extract the watchdog disconnect predicate | Low — one function | Medium | XS | `TR-3` |
| **D8** | Move poison-recovery helpers to `src/sync.rs` | Low — import churn | Medium | XS | `MD-5`, unblocks P0-3 |
| **D9** | Re-port `preview_poller` off `CameraHandle` | Low — one signature | Low | S | `TR-2` |

**Execution order is not this order.** See § Sequencing.

---

# Tier 1 — reshapes a boundary

## D1. `trait Camera` width — **RESOLVED by S4-2** (eight flat role traits; kept for the measurement record)

`src/camera.rs:63`. Measured usage of distinct `Camera` methods per consumer:

| Consumer | Methods used |
|---|---|
| `mqtt_dispatch.rs` | 15 |
| `camera_tasks.rs` (5 pollers/listeners) | 6 |
| `oneshot/ptz.rs` | 5 |
| `oneshot/users.rs` | 4 |
| `oneshot/snapshot.rs` | 3 |
| `stream_source.rs` | 2 |
| `startup_wake.rs` | 1 |
| 11 other `oneshot/*` handlers | 1–2 each |

`fake_camera.rs` is **751 lines** to satisfy a trait whose median consumer needs two methods. Every new test fixture pays that cost; every new method taxes every fake.

`TR-3` says traits should be narrow and role-shaped. The roles are already visible in the consumer table — the pollers, the dispatcher, the stream reader, and the one-shot handlers each want a different slice.

**Target:**

```rust
pub trait CameraSession: Send + Sync {          // lifecycle — everyone
    async fn end_session(&self) -> CameraResult<()>;
    async fn keepalive_probe(&self) -> CameraResult<()>;
    async fn capabilities(&self) -> CameraResult<CameraCapabilities>;
}
pub trait VideoSource: CameraSession { … }       // stream_source: 2 methods
pub trait CameraTelemetry: CameraSession { … }   // pollers: battery, pir, floodlight, motion
pub trait CameraControl: CameraSession { … }     // ptz, light, siren, reboot
pub trait CameraAdmin: CameraSession { … }       // users, services, time, version
```

`BcCamera` implements all of them (one `impl` block each, same forwarding shape). `FakeCamera` splits into per-role fakes that a test composes; the 751-line monolith stops being the entry fee for testing a two-method consumer.

**Reviewer rule:** a new `Camera` method must land on exactly one role trait, and the PR must name which consumer needs it. A method landing on `CameraSession` needs justification that *every* consumer needs it.

**Deliberately in scope, revisiting a deferral.** `hexagonal-refactor.md` § Deferred kept vendored BC types (`RfAlarmCfg`, `LedState`, `VersionInfo`, `UserList`, `AbilityInfo`, `MotionData`, `Direction`, `LightState`) in the port signatures, reasoning that local twins would be field-for-field mapping with no second implementation to justify it. That reasoning holds for *most* of them and is not reopened here. But it stops holding where a type forces a fake to construct BC XML: split the traits first, then re-measure which vendored types actually survive on a narrow role. Anything only reachable from `CameraAdmin` can keep its BC type indefinitely — that trait has one consumer and no second backend is plausible.

## D2. BcMedia→Frame translation is the half of phase 2 that never landed

`hexagonal-refactor.md` §2 named three altitudes tangled in `stream_source.rs`: bridging policy, tokio plumbing, and **BcMedia translation**. Phase 2 extracted the first — `gap_bridging.rs` is genuinely pure, takes `Instant` as a parameter, and tests in microseconds. The third is untouched.

`apply_bcmedia_packet` (`src/stream_source.rs:1498`) and its four handlers — `handle_iframe:1546`, `handle_pframe:1830`, `handle_aac:2029`, `handle_adpcm:2296` — each take a `broadcast::Sender<Frame>`, two `mpsc::Sender<PacedFrame>`, `Arc<LastFrameBuffer>`, `Arc<RwLock<SdpParams>>`, `Arc<RwLock<AudioPresence>>`, and perform the side effects inline.

What is buried in there is the product's highest-defect-cost logic: codec detection from the first decisive NAL, parameter-set extraction, PTS synthesis and wraparound, AAC AOT/sample-rate derivation, and the audio gate during bridging. A/V desync is the product's visible failure mode and this is where it lives.

**Target** — the shape `gap_bridging.rs` already proves out:

```rust
pub enum Emit {
    Video(Frame), Audio(Frame),
    PaceVideo(PacedFrame), PaceAudio(PacedFrame),
    SdpParams(SdpParams), LastFrame(Vec<u8>), AudioSeen,
}

pub fn translate(packet: &BcMedia, state: &mut StreamTranslatorState, bridging: bool)
    -> (SmallVec<[Emit; 4]>, Option<u32>);
```

The driver becomes ~20 lines of fan-out. Codec detection and PTS edges become table tests; `tests/fixture_replay.rs` gains the ability to assert on decision *sequences* rather than absence of panic; the wraparound cases become property tests (`TS-5`).

**Reviewer rule:** no `Sender`, `Arc`, or `RwLock` in a `translate`-family signature. If a change needs one, it belongs in the driver.

**Live-verify:** this is on the RTSP path. It must land alone, and `tests/scripts/manual-verify.sh` is the gate. If hardware is unavailable, say so in the PR rather than implying verification.

## D3. Discovery: every verb is send-and-validate in one `async fn`

`src/baichuan/bc_protocol/connection/discovery.rs`, 3295 lines. `register_address:507` (129 lines), `device_initiated_dev:636` (113), `device_initiated_map:749`, `client_initiated_relay:926` each interleave XML construction, socket send, retry scheduling, and reply validation. Only the leaf predicates (`valid_ip:106`, `valid_port:110`, `get_broadcasts:1441`, `get_local_ip_for_target:1415`) are tested.

**Target:** per-verb `fn build_<verb>(...) -> UdpXml` and `fn classify_<verb>_reply(UdpXml) -> Result<Decision>`, leaving each `async fn` as a send/recv/retry driver. The XML shapes are now specified in `docs/baichuan-protocol.md` §9, so the expected values are written down rather than inferred from the code under test.

**Ranked here but sequenced last.** The failure mode is "camera not found," not "video is wrong," and the fixtures are the hardest to obtain in the tree. It is on the plan so that a future discovery change has a target shape to land in — not because it should be scheduled now.

---

# Tier 2 — removes structure

## D4. ~2 kloc of dead protocol command surface

Six `bc_protocol` modules have **zero** referenced public functions across `src/`, `tests/`, `benches/`, `fuzz/`, and `tests/scripts/decode-bc-pcap/`:

| Module | Lines | Public fns | Notes |
|---|---:|---:|---|
| `talk.rs` | 904 | 5/5 | Sole `crossbeam-channel` consumer; carries the documented `AS-5` violation |
| `email.rs` | 599 | 8/8 | |
| `pushinfo.rs` | 148 | 3/3 | |
| `stream_info.rs` | 122 | 1/1 | |
| `uid.rs` | 114 | 2/2 | |
| `ping.rs` | 97 | 1/1 | `keepalive_probe` covers liveness; this is a second unused path |
| **Total** | **1984** | | |

Partially dead, to be trimmed in the same pass: `email`-adjacent helpers, `time.rs` `get_dst`/`get_time`, `motion.rs` `await_start`/`await_stop`/`motion_detected`/`motion_detected_within`/`consume_motion_events`, `ptz.rs` `get_zoom`, `battery.rs` `monitor_battery`, `floodlight.rs` `get_floodlight_tasks`, `ledstate.rs` `set_ledstate`, `pirstate.rs` `set_pirstate`.

**`talk.rs` is the case that proves the rule.** `hexagonal-refactor.md` § Deferred recorded its blocking `crossbeam_channel::recv()` inside `BufferedStream::fill_buf` — reachable from `async fn talk_stream` — as a real `AS-5` violation, then declined to fix it *because it is unreachable from the shipped binary and two-way audio cannot be verified without hardware*.

Unreachable is not a reason to keep code and not fix it. It is a reason to delete it. Deleting resolves the `AS-5` violation permanently, drops `crossbeam-channel` from the dependency tree (`DP-3`), removes ~500 lines of tests that only test dead code, and eliminates the "cannot verify without hardware" blocker by removing the thing that needed verifying.

Two-way audio is not in scope: `CLAUDE.md` names the 9 MQTT features and talk is not among them, and the product is an RTSP/MQTT bridge for battery cameras.

**Verification before deletion** (all four must be re-run at PR time, not trusted from this doc):

1. `grep -rE "(\.|BcCamera::|Self::)<fn>\(" src/ tests/ benches/` — empty.
2. `fuzz/` and `tests/scripts/decode-bc-pcap/` build against the trimmed crate. Both are out-of-tree and depend on `bairelay` by path; today they touch only `baichuan::{bc,bcudp,fuzz_api,pcap_decode_api}` and `rtsp::codec`, none of which this removes.
3. `cargo tarpaulin` — coverage must not *drop*. Deleting tested dead code raises the denominator's quality; a drop means something live was removed.
4. The message-ID constants in `bc/model.rs` stay. They are the protocol vocabulary and are documented in `docs/baichuan-protocol.md` §5; deleting a `pub const MSG_ID_*` loses knowledge, deleting an unused RPC wrapper does not.

**Reviewer rule:** a `pub fn` on `BcCamera` with no caller in `src/` is not "future-proofing" — it is unreviewed, untested-in-anger surface. Either wire it to a consumer in the same PR or leave it out.

---

# Tier 3 — extracts pure functions in place

No boundary moves. Each is reviewable as an ordinary diff, and each converts a decision that currently needs task choreography into one that needs a table.

## D5. Pacer re-anchor policy — smallest change, best ratio

`media_pacer_task` (`src/stream_source.rs:967`) buries three rules in an async loop with channels and `sleep_until`:

```rust
let target = match next_emit_at {
    Some(t) if t > now + max_lead      => now,   // future cap
    Some(t) if snap_on_past && t < now => now,   // dry-queue snap
    Some(t)                            => t,
    None                               => now + initial_latency,
};
```

These encode hard-won behaviour — the surrounding comments cite mpv's "Invalid audio PTS" and a 1 ms/packet drift that skewed the RTCP NTP↔RTP slope by 1.6% over a 5 s window. They are verifiable today only through `#[tokio::test(start_paused)]` plus channel choreography.

**Target:** `fn next_target(cursor: Option<Instant>, now: Instant, max_lead: Duration, initial_latency: Duration, snap_on_past: bool) -> Instant`. The full matrix — including the audio/video `snap_on_past` asymmetry — becomes a table test with no runtime.

## D6. Config warnings are tautologically tested, and the tests say so

`tests/config_test.rs:1407`: *"Runs without panicking; tracing output isn't captured here."* That is `TS-3` by name.

Five functions take `&Config`, return `()`, and log inline: `warn_deprecated_pause_fields:864`, `warn_neolink_compat_fields:925`, `warn_wire_debug_enabled:994`, `warn_users_without_tls:1010`, `warn_idle_timeout_below_prune_floor:1055`. Across 35 `warn_*` references in the test file, nothing asserts *which* warning fired.

**Target:** return `Vec<ConfigWarning>`; `check-config` and startup do the logging. The warning text is operator-facing contract — `warn_users_without_tls` explains a real security trade-off — and nothing pins it today.

## D7. Watchdog disconnect predicate

`hexagonal-refactor.md`'s disposition table said watchdog splits "policy fn → domain; 30 s sweep task → app." The sweep landed; the policy did not. `src/watchdog.rs:63-77` inlines it:

```rust
if cam.config().idle_disconnect && cam.state().is_connected() {
    let grace = resolve_idle_disconnect_timeout(cam.config(), self.prune_grace);
    if let Some(idle_since) = cam.wake_lock().idle_since() {
        if idle_since.elapsed() >= grace { … }
```

The comment above it documents a race this predicate was tuned to avoid — the watchdog tearing sessions a few hundred ms before an MQTT `control/wakeup` lands. That tuning deserves a test that does not need a live `CameraHandle` and a tokio interval.

**Target:** `fn should_disconnect(idle_disconnect: bool, connected: bool, idle_for: Option<Duration>, grace: Duration) -> bool`.

## D8. `src/sync.rs` — still outstanding, still blocking P0-3

`lock_recover` / `rlock_recover` / `wlock_recover` / `RwLockPoisonRecover` / `MutexPoisonRecover` remain at `src/stream_source.rs:92-135`, imported by `camera.rs` and `status_cache.rs`. `src/sync.rs` does not exist.

`remediation-plan.md` lists this as a prerequisite for P0-3 (the poison-panic cascade across `SessionRegistry`, `LastFrameBuffer`, `CameraRegistry`). **P0-3 — a P0 — is blocked on a file move.** Do it with D2, which opens `stream_source.rs` anyway.

## D9. `preview_poller` is the one task still bound to the concrete handle

Five of six `camera_tasks.rs` entry points take `Arc<dyn Camera>`. `preview_poller:298` alone takes `Arc<crate::camera::CameraHandle>`, for `is_connected()` and `bc_camera()`.

It *is* tested — four tests drive it, including connected and error paths — but each builds a real `CameraHandle` where the sibling tasks build a `FakeCamera`. **I previously argued this was fine because the tests exist. That was the wrong test.** The question is whether keeping the concrete binding buys anything, and it does not: it buys a heavier fixture and an inconsistency a reader has to explain.

**Target:** a two-method `PreviewHost` port (`is_connected`, `video_source`) that `CameraHandle` implements, or hoist the connected-check to the caller and pass `Arc<dyn Camera>`. Decide when the code is open; either beats the status quo.

---

## Sequencing

Blast radius orders the *review*; risk and unblocking order the *work*.

| Batch | Items | Rationale |
|---|---|---|
| **1** | D4 | Deletion first — every later batch gets smaller. No behaviour change; the gate is `cargo test` + the two out-of-tree builds. |
| **2** | D5, D6, D7, D9 | Pure-function extractions. No async behaviour changes, no hardware needed, independently reviewable. |
| **3** | D2 + D8 | The big one, alone, behind `manual-verify.sh`. D8 rides along because D2 opens the file. |
| **4** | D1 | After D2, so the trait split is measured against a `stream_source.rs` that has stopped moving. |
| **5** | D3 | Only when a discovery change is already needed. |

D4 before everything is deliberate: deleting `talk.rs` removes a dependency and an `AS-5` violation before any refactor has to carry them forward.

---

## Explicitly not planned

Recorded so a future reviewer sees a decision rather than an omission.

**A domain crate.** Built during phase 4 and reverted; `hexagonal-refactor.md` § Why no domain crate has the reasoning. This is not inertia — the enforcement it bought could only stand in front of the modules least likely to grow an I/O dependency, because the `Camera` trait's signatures keep it outside the wall. **Trigger to revisit:** a second binary, or an external consumer of the policy code. D1 does not change this; narrower traits make the wall *less* necessary, not more.

**Local twins for every vendored BC type.** Partially reopened under D1 — see the note there. The blanket version stays out.

**Splitting `rtsp/server/connection.rs`.** At 2059 lines it looks like a peer of the god modules, but it is already decomposed the way this plan asks for: pure framing helpers (`find_double_crlf:278`, `parse_content_length:282`, `try_consume_request:243`, `scheme_matches_transport:564`) are unit-tested, and the handlers are driven over a `DuplexStream` — a real seam, not a mock. `handle_setup:647` at 305 lines and `build_transport:952` are worth watching; transport negotiation is a pure decision wearing an `async fn`. Not planned, but the next substantive change to `handle_setup` should extract it.

**`log_capture.rs`.** Looks like the same smell as D6 — testing behaviour through log output — but it is not. It exists to pin the literal marker strings `tests/scripts/manual-verify.sh` greps (remediation P1-4), where the log text *is* the contract. Keep.

---

## Review checklist

For any PR touching the camera port, the stream path, or the protocol crate:

1. **New `Camera` method?** Names one role trait and one consumer that needs it. (D1)
2. **New logic in a translate/handle function?** Contains no `Sender`, `Arc`, or `RwLock`. If it must, it belongs in the driver. (D2)
3. **New `pub fn` on `BcCamera`?** Has a caller in `src/` in the same PR. (D4)
4. **New decision inside an async loop?** Extracted as a pure `fn` taking time and state as parameters — the `classify_*` / `advance_*` pattern already in `camera_tasks.rs:207` and `camera.rs:262`. (D5, D7)
5. **New `warn_*` / diagnostic?** Returns a value; the caller logs. (D6)
6. **New test asserting "does not panic"?** Rejected. If that is all that can be asserted, the unit is wrong shape. (`TS-3`)
7. **New `Arc<Mutex<_>>` or `Arc<RwLock<_>>` crossing a task boundary?** Justified against `OW-5` and `AS-4`, and added to the shared-state map in `docs/code-paths.md` §13.

---

## Doc maintenance riding along

Done: P3-3 and §12 now describe the shipped role-trait seam, and the
`crates/` paths were rewritten for the single-crate tree. File sizes
quoted anywhere in these docs are point-in-time measurements — re-measure
before relying on them.
