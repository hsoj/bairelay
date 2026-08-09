# Action Plan

The single ordered plan. Consolidates `docs/remediation-plan.md` (defects), `docs/decoupling-plan.md` (testability), the open threads in `docs/hexagonal-refactor.md` (structure), and the 2026-08-05 full-codebase review.

**Stages 0–3 landed 2026-08-08** (see the resolved table). The only open item is **S4-1** (needs live hardware for `manual-verify.sh`). File:line references in the S4-1 section predate the 2026-08-08 sweep — re-verify at PR time.

Ordered by the project's stated priorities, in order: **stability → developer experience → design best practice → composition-based trait design**. Where an item serves several, it is placed by the highest one it serves.

**Sequencing lives here and nowhere else.** Companion docs keep their roles:

| Doc | Role |
|---|---|
| `remediation-plan.md` | Findings archive — evidence and diagnosis, no sequencing |
| `decoupling-plan.md` | Design rationale for Stage 4 — target shapes, no sequencing |
| `hexagonal-refactor.md` | Historical record of phases 1–5 — do not re-plan from it |

---

# Resolved since the last revision

Recorded so nobody re-litigates them. Deltas from the planned shape are noted; they are deliberate, not unfinished work.

| Item | Resolution | Evidence |
|---|---|---|
| **S4-2 — decompose `trait Camera`** | Eight **flat** role traits in `src/camera.rs` (`Session` 2, `Video` 2, `Stills` 1, `Events` 2, `Power` 3, `Lighting` 7, `Ptz` 5, `DeviceAdmin` 11 methods; no role names another). `camera_tasks.rs` pollers, `StreamSource::start`, and `keepalive_loop` take single roles; per-role fakes exist in `src/fake_camera/roles.rs`. | commits `083609a`–`ade6ea1` |
| **One-shot commands skipped `apply_cloud_auth`** | All four config-load paths route through `config::load_config` (read → parse → hydrate → validate) with typed `ConfigLoadError`; cloud hydration can no longer be skipped by construction. Regression test in `tests/config_test.rs`. | `54eefad` |
| **Test scaffolding shipped in release builds** | `start_inert_for_test*`, `FakeFrameInjector`, `set_sdp_params_for_test`, `set_state_for_test`, `start_with_packet_source` now behind `#[cfg(any(test, feature = "test-util"))]`; the comments that falsely claimed a gate now describe the real one. | `8b4f2f6` |
| **`BcCamera` named outside the adapter** + **speculative auth-substring fallback** | Connect/login and auth classification moved into `bc_camera::connect` / `ConnectError` (`Auth` terminal, `Other` retryable). `camera.rs` and `oneshot/runner.rs` hold only `Arc<dyn Camera>`; `is_login_failure` and its Debug-substring matching deleted. `oneshot::classify` still finds the baichuan cause on the source chain (pinned by test). | `cf9b9ca` |
| **`tick_bridging` burst copy on the Live path** | `BridgingPolicy::on_tick` takes the replay anchor as a lazy `FnOnce` closure; the up-to-8 MiB payload is assembled only once a gap is open. Laziness pinned by `live_tick_never_consults_the_replay_anchor`. | `1ea4231` |
| **S1-1 — `src/sync.rs` + poison sweep** (2026-08-08) | Helpers moved to `src/sync.rs` (trait form only; free fns deleted — F10 unified), with poison-recovery pinned by tests. All 37 `expect("… poisoned")` sites in `src/rtsp/` + `src/wake_server/` converted, plus the silent `if let Ok` SDP skip in `stream_source.rs` and the `camera_provider.rs`/`startup_wake.rs` stragglers. Only `fake_camera` and one test-serialization lock keep bare handling — both test-only, per plan. | — |
| **No-panic policy for production code** (2026-08-08) | Every remaining production `expect`/`unwrap` eliminated: `cloud.rs` cache locks → `lock_recover`; SETUP handler takes session-task handles off the entry before insert (atomic, three dead registry accessors deleted); RTSP response/RTP builders degrade (drop header / empty packet = RTP loss) instead of panicking; `main.rs` zips the MQTT pair; UID regex replaced with a pure byte check (**`regex` dependency dropped**); embedded-font and serde invariants degrade gracefully. Enforced by `#![warn(clippy::unwrap_used, clippy::expect_used)]` in `lib.rs`/`main.rs` + `clippy.toml` test exemptions, so a new production panic fails CI. Also closed S1-4's `Server: bairelay/0.1.0` literal (now `env!("CARGO_PKG_VERSION")`). | — |
| **`crates/` paths in the planning docs** (half of old S2-2) | `remediation-plan.md` and `code-paths.md` no longer reference `crates/core` / `crates/rtsp` paths. The `CameraDriver` staleness remains — see S2-2. | `ade6ea1` |
| **S0-1 — library surface declared unstable** (2026-08-08) | `//!` stability block in `lib.rs`, README note, `release.yml` step name fixed. Option A (declare, don't restrict). | — |
| **S0-2 — supply-chain CI** (2026-08-08) | `deny.toml` + pinned `cargo-deny@0.20.2` job, MSRV job (1.93 verified locally), nightly fuzz-smoke workflow (`fuzz.yml`, 8 targets × 10 s). The advisory feed found real issues on day one: quick-xml upgraded 0.36→0.41 (two DoS advisories on the untrusted camera-XML path), `get_if_addrs` replaced by its maintained fork `if-addrs` (kills the unmaintained `gcc 0.3` transitively), and documented ignores for rustls-webpki 0.102 (pinned by rumqttc 0.25.1, latest), rustls-pemfile, ttf-parser. | — |
| **S1-2 — `#[must_use]` on RAII guards** (2026-08-08) | `WakeLockGuard` + `SubscriptionHandle`, each with a message naming the failure. | — |
| **S1-3 — `set_var` race** (2026-08-08) | `verbosity_env_filter(verbose, rust_log: Option<&str>)`; tests are value-driven (assert the rendered filter), all env mutation deleted. | — |
| **S1-4 — large futures** (2026-08-08) | `large_futures` lint on in `lib.rs`/`main.rs` (+`future-size-threshold` in clippy.toml). Root-caused instead of 300 `Box::pin`s: `BcPayloads::BcXml` boxed (`Bc` 4128 → ~180 bytes; the old `large_enum_variant` allow deleted), `get_services`/`set_services` moved to `Box<BcXml>`. Zero warnings remain. | — |
| **S1-5 — MQTT replies reflect outcomes** (2026-08-08) | All four `Query*` arms + directional PTZ now propagate errors → `FAIL` on the reply topic; `serialize_xml` failures logged and propagated; every `let _ = reporter.report(…)` warn-logs. `camera_status.rs`'s "they all log and carry on" is now true. | — |
| **S1-6 — coverage flake** (2026-08-08) | Both motion hold-down tests on `start_paused` virtual time (tarpaulin ×3 green); `floodlight_listener_exits_cleanly_on_subscribe_error` renamed to `…_on_closed_channel`. | — |
| **S2-1 — dead protocol modules** (2026-08-08) | `talk` / `email` / `pushinfo` / `stream_info` / `uid` / `ping` deleted (~2 000 lines), `crossbeam-channel` dropped, `monitor_battery` + `MotionData::{motion_detected, motion_detected_within, await_start, await_stop}` trimmed. `fuzz/` + `decode-bc-pcap` still build; `MSG_ID_*` constants kept. | — |
| **S2-2/S2-3/S2-4 — docs, dead code, comment rot** (2026-08-08) | Planning docs re-pointed at the role-trait seam; `gap_threshold` accessor+field, `emit_success_bytes`/`emit_failure_payload`, `drive_reconnect_with_backoff`+`ReconnectOutcome` deleted; `snapshot_json_preflight` now calls `check_json_output`. All 25 surviving phase/task comments scrubbed; CONTRIBUTING/README/publish-crates workspace-era text fixed; the 32-KiB-guard and 60-s-timeout lying comments corrected. | — |
| **S2-5 — naming/lint hygiene** (2026-08-08) | `bc_camera` renamed to `camera` at every role-trait seam and on `CameraHandle` (field + accessor); poller `interval_ms: u64` → `Duration`; probe timeout named `PROBE_TIMEOUT`; three eligible `#[allow]`s → `#[expect(…, reason)]`. | — |
| **S3-1…S3-5 — pure-function extractions** (2026-08-08) | `next_target` (pacer re-anchor table, incl. `snap_on_past` asymmetry) + table test; `watchdog::should_disconnect` + decision-table test; config warnings as `Vec<ConfigWarning>` (`config_warnings`/`log_config_warnings`, tests now assert values); `preview_poller` takes `ConnectedStills` (consumer-declared two-method capability); all four live-verify markers pinned by `log_capture` tests ("RTSP server listening", "Disconnected", "Grace period expired, disconnecting", plus the existing startup-wake pair). | — |

**S4-2 deltas worth knowing:** `trait Camera` composes the eight roles as a supertrait bound with a marker blanket impl (`camera.rs:190-192`) — a composition marker, not the forbidden `CameraDriver`-style forwarding impl. Wiring points (`mqtt_dispatch`, `startup_wake`, `CameraHandle`) hold full `Arc<dyn Camera>`; vendored BC types stay in role signatures per the header comment at `camera.rs:48-52` ("keep, re-measure later"). Two loose ends became their own items: `preview_poller` still takes the concrete handle (S3-4) and the parameters/fields are still *named* `bc_camera` (S2-5).

---

# Stages 0–3 — **complete** (2026-08-08)

Every item in Stages 0, 1, 2, and 3 is resolved; see the table above
for what shipped and the deltas. The review checklist below remains in
force. One follow-up worth knowing: the quick-xml 0.41 upgrade and the
`BcPayloads::BcXml(Box<…>)` change touched the protocol parse/serialize
path — covered by the full unit suite and fixture replay shapes, but a
live-verify pass has not been run on them.

---

# Stage 4 — Composition

## S4-1. Sans-IO the BcMedia→Frame translation · L · highest value remaining

Still open, with marginal progress to build on: mutable state is now consolidated in `StreamTranslatorState` (`stream_source.rs:307`), and the handlers have direct unit tests — but `apply_bcmedia_packet:1511` and its four handlers still take a `broadcast::Sender`, two `mpsc::Sender`s, and three `Arc<RwLock<…>>`s and perform side effects inline. `stream_source.rs` is 5,233 lines; A/V desync is the product's visible failure mode and this is where it lives.

**Target** unchanged — the shape `gap_bridging.rs` proves:

```rust
pub enum Emit {
    Video(Frame), Audio(Frame),
    PaceVideo(PacedFrame), PaceAudio(PacedFrame),
    SdpParams(SdpParams), LastFrame(Vec<u8>), AudioSeen,
}

pub fn translate(packet: &BcMedia, state: &mut StreamTranslatorState, bridging: bool)
    -> (SmallVec<[Emit; 4]>, Option<u32>);
```

`Emit` is a closed set → `enum` + exhaustive `match` (TY-5). Codec detection and PTS edges become table tests; wraparound becomes a property test.

**Live-verify:** on the RTSP path — lands alone, gated by `tests/scripts/manual-verify.sh`. If hardware is unavailable, say so in the PR rather than implying verification.

---

# Deferred, with triggers

| Item | Trigger to revisit |
|---|---|
| **Discovery build/classify split** (`discovery.rs`, still 3,295 lines) | When a discovery change is already needed. Failure mode is "camera not found", not "video is wrong". |
| **`rand` migration** — now **three** locked versions (0.8.7, 0.9.5, 0.10.2) | `cargo-deny` `bans` (S0-2) caps it. Migrate when `rand` is touched anyway. |
| **`[lints]` + `missing_docs` sweep** | Only if the lib surface becomes a real API. S0-1 removes the reason. |
| **`#[non_exhaustive]`** | `config::*` types only. The blanket sweep dies with S0-1. |
| **Splitting `rtsp/server/connection.rs`** | Extract `build_transport` on the next substantive `handle_setup` change. |
| **Splitting `stream_source.rs` / `camera.rs`** (5,233 / 3,233 lines) | S4-1 shrinks the former structurally; re-measure both after it lands rather than moving code twice. |
| **Vendored BC types in role signatures** | Re-measure per-role after S4-1; anything reachable only from `DeviceAdmin` keeps its BC type indefinitely. |
| **A domain crate** | A second binary, or an external consumer of the policy code. |

---

# Risk ranking

Assessed 2026-08-08, post-sweep. Everything ranked above S4-1 in the previous revision has landed; the remaining risk surface is:

| # | Item | Likelihood | Impact | Failure scenario | Effort |
|---|------|:---:|:---:|---|:---:|
| 1 | **S4-1** translation still effectful | Med | **High** | A/V desync — the product's visible failure — lives in ~5,200 effectful lines where every change is under-tested and needs hardware to verify. Standing *change* risk: it fires whenever the stream path is next touched. | L |

---

# Sequencing

| Batch | Items | Gate | Status |
|---|---|---|---|
| **0** | S0-1, S0-2 | CI green | **landed 2026-08-08** |
| **1** | S1-2 … S1-6 | `cargo test` + `clippy` + tarpaulin ×3 | **landed 2026-08-08** |
| **2** | S2-1 … S2-5 | `cargo test` + tarpaulin non-decreasing + out-of-tree builds | **landed 2026-08-08** |
| **3** | S3-1 … S3-5 | `cargo test` | **landed 2026-08-08** |
| **4** | S4-1 | `manual-verify.sh` | open — **needs hardware** |

---

# Review checklist

Applies to any PR touching the camera port, the stream path, or the protocol module.

**Composition**

1. **New trait?** Names a capability (`can-do`), not a taxonomy (`is-a`). No supertrait used for reuse — the shipped role traits (`Session`, `Video`, `Stills`, `Events`, `Power`, `Lighting`, `Ptz`, `DeviceAdmin`) are flat; keep them that way.
2. **New camera-facing method?** Lands on exactly one role trait, and the PR names the consumer that needs it. A method on `Session` must justify that *every* consumer needs it.
3. **Consumer takes a camera?** The narrowest role that covers it, named `camera` — never the adapter's name, never `Arc<dyn Camera>` outside wiring/dispatch points. (S2-5)
4. **Reuse between types?** A held field with explicit delegation, never `Deref`. (TR-6)
5. **Closed set of outcomes?** `enum` + exhaustive `match`, so adding a case breaks every site. (TY-5)

**Testability**

6. **New logic in a `translate`/`handle` function?** No `Sender`, `Arc`, or `RwLock` in the signature. If it needs one, it belongs in the driver. (S4-1)
7. **New decision inside an async loop?** Extracted as a pure `fn` taking time and state as parameters. (S3-1, S3-2)
8. **New timing-sensitive test?** `start_paused = true` + `advance`, never wall-clock sleeps as synchronization. (S1-6)
9. **New `warn_*` or diagnostic?** Returns a value; the caller logs. (S3-3)
10. **New test asserting "does not panic"?** Rejected. If that is all that can be asserted, the unit is the wrong shape. (TS-3)

**Stability**

11. **New `pub fn` on `BcCamera`?** Has a caller in `src/` in the same PR. (S2-1)
12. **New shared lock crossing a task boundary?** Justified against OW-5 and AS-4, uses the poison-recovering helpers (the trait form), never bare `.expect("poisoned")` and never `if let Ok(guard)`. (S1-1)
13. **New RAII guard?** `#[must_use]`. (S1-2)
14. **Command handler replies to an operator?** The reply reflects the actual outcome — no unconditional `OK`. (S1-5)
15. **New log string that `manual-verify.sh` greps?** Pinned by a `log_capture` test. (S3-5)
