# Action Plan

The single ordered plan. Consolidates `docs/remediation-plan.md` (defects), `docs/decoupling-plan.md` (testability), the open threads in `docs/hexagonal-refactor.md` (structure), and the 2026-08-05 full-codebase review.

**Verified against the tree at commit `1ea4231` (2026-08-05).** Every file:line below was checked on that commit, not inherited from an earlier revision. Re-verify at PR time regardless.

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
| **`crates/` paths in the planning docs** (half of old S2-2) | `remediation-plan.md` and `code-paths.md` no longer reference `crates/core` / `crates/rtsp` paths. The `CameraDriver` staleness remains — see S2-2. | `ade6ea1` |

**S4-2 deltas worth knowing:** `trait Camera` composes the eight roles as a supertrait bound with a marker blanket impl (`camera.rs:190-192`) — a composition marker, not the forbidden `CameraDriver`-style forwarding impl. Wiring points (`mqtt_dispatch`, `startup_wake`, `CameraHandle`) hold full `Arc<dyn Camera>`; vendored BC types stay in role signatures per the header comment at `camera.rs:48-52` ("keep, re-measure later"). Two loose ends became their own items: `preview_poller` still takes the concrete handle (S3-4) and the parameters/fields are still *named* `bc_camera` (S2-5).

---

## The gate nobody noticed (still open)

`release.yml:268-269` publishes `bairelay` to crates.io on every non-draft release while `src/lib.rs` declares the internal module tree `pub` for the test harness — so the internals are public API on crates.io that nobody consumes. The S4-2 split already shipped as a semver-major on that surface and nobody noticed, which is the argument in one sentence. Since the plan was written, `fake_camera` and `log_capture` moved behind `#[cfg(test)]` (`lib.rs:30,37`), shrinking the exposure without declaring a policy.

**S0-1 removes the semver tax and comes first because it makes everything after it cheaper.**

---

# Stage 0 — Unblock

## S0-1. Declare the library surface unstable · S · stability + DX

**A — Declare, don't restrict (recommended).** `//!` policy block in `lib.rs` plus a line in `README.md`: *the binary CLI and its config are the stable interface; the library surface carries no semver guarantee and exists for the test harness.* Near-zero churn, honest, standard for binary crates that also publish. **B — feature-gate every module** stays the fallback if someone ever depends on the lib surface.

This dissolves old P2-1 (196 undocumented public items) and P2-2 (`#[non_exhaustive]` sweep) — neither is worth doing on a surface with no consumers. `#[non_exhaustive]` stays worth applying to `config::*` types only (they parse operator TOML).

**Same PR:** fix the stale step name at `release.yml:291` — "Publish bairelay-\* + bairelay" names member crates that were merged away.

## S0-2. CI: supply chain, MSRV, fuzz smoke · S · stability

Verified absent: no `deny.toml`, no MSRV job, no fuzz wiring in any workflow. Three workflow-only additions, no source risk:

- **`cargo-deny`** + `deny.toml` — advisories, licences, bans. 379 crates including `rustls`, a committed `Cargo.lock`, every CI invocation `--locked`: a pinned lockfile with no advisory feed holds known-vulnerable versions indefinitely and silently. The `bans` check also caps the `rand` duplication, which has **worsened since the last revision**: `Cargo.lock` now carries 0.8.7, 0.9.5, *and* 0.10.2.
- **MSRV job** — `rust-version = "1.93"` is claimed and never built. Verify in CI or delete the claim.
- **Nightly fuzz smoke** — eight targets cover the untrusted-input surface; `scripts/fuzz.sh` exists; zero CI wiring. 10 s/target turns dormant assets into a live regression net.

Do S0-2 before Stage 1 so the poison sweep lands under a gate that can catch what it disturbs.

---

# Stage 1 — Stability defects

## S1-1. `src/sync.rs` + poison sweep · M · **highest stability item**

**37 `expect("… poisoned")` sites remain in `src/rtsp/` and `src/wake_server/`** (recounted at `1ea4231`). One panic under `SessionRegistry` or `LastFrameBuffer` locks takes down the whole server rather than one client. `src/sync.rs` still does not exist; the recovery helpers still live in `stream_source.rs`.

The sweep's scope grew — fold in three findings from the review:

- `src/stream_source.rs:1637` — `if let Ok(mut guard) = sdp_params.write()` **silently no-ops on poison** while 8 sibling sites in the same file recover. After any poisoning, `SdpParams.video` can never populate: every DESCRIBE 503s forever with zero log output. This is the worst single site in the sweep; use `wlock_recover`.
- `src/camera_provider.rs:95`, `src/startup_wake.rs:158,234` — `.expect("… poisoned")` on the same presence/SDP locks the stream path deliberately recovers. Pick one policy per lock.
- **Two parallel recovery APIs coexist** (`stream_source.rs:92-105` free fns vs `:114-134` trait methods, both `pub(crate)`, both used). The `src/sync.rs` move is the moment to keep the trait form and delete the free functions.

Leave `fake_camera` sites alone — test-only.

## S1-2. `#[must_use]` on the RAII guards · S · stability

Still exactly 2 `#[must_use]` in `src/`, and both are builder setters. `WakeLockGuard` (`src/wake_lock.rs:26`) and `SubscriptionHandle` (`src/rtsp/provider.rs:83`) carry nothing — `cam.wake_lock().acquire();` acquires and instantly releases, and a dropped guard means a camera that sleeps through the thing that wanted it awake. One attribute each.

## S1-3. `RUST_LOG` test data race · S · stability

`src/cli_convert.rs` still has 5 `set_var`/`remove_var` sites in parallel-threaded tests — the exact UB that made `set_var` `unsafe`. Fix by making `verbosity_env_filter` take the value as a parameter; the `unsafe` disappears and the function becomes value-testable (Stage 3's pattern).

## S1-4. Version literal and large futures · S · stability

- `src/rtsp/protocol/message.rs:161` still hardcodes `Server: bairelay/0.1.0` against a crate at 1.1.2. Use `concat!("bairelay/", env!("CARGO_PKG_VERSION"))`.
- No `clippy.toml`, no `large_futures` lint, no size-motivated `Box::pin` anywhere. Enable the lint, box what it names.

## S1-5. Failures the operator cannot see · S · stability

New from the review; partially improved since (query errors are now warn-logged) but the operator-facing half remains:

- **Every MQTT `Query*` arm returns `Ok(())` unconditionally** (`src/mqtt_dispatch.rs:233,260,273,299`), so a battery/PIR/preview/preset query that errored or timed out still publishes `OK` on the reply topic — directly contradicting the rationale written at `:186-197` for the `PtzPresetByName` FAIL fix ("operators saw 'success' while the camera never moved"). Same for a failed directional PTZ move.
- `if let Ok(xml) = serialize_xml(...)` (`:226,266`) swallows serialization errors unlogged, and `serialize_xml` returns `Result<String, String>` (ER-1).
- **Status-report error policy is inconsistent**: `let _ = reporter.report(…)` with no log at `camera_tasks.rs:96,100,251,402,441,464` and `camera.rs:1354-1355`, while `preview_poller` and `mqtt_dispatch` warn-log the same failure class. `camera_status.rs` justifies the opaque error with "they all log and carry on" — half the call sites don't. Pick one policy.

## S1-6. Deflake the coverage gate · S · stability

The motion hold-down tests (`src/camera_tasks.rs:1245-1377`) use real 50/100/700 ms sleeps against a 500 ms window on multi-thread runtimes; their own comment admits "smaller windows have flaked under coverage", and **they flaked a tarpaulin run during the 2026-08-05 review session** — this is a live problem, not a theoretical one. Both are single-task deterministic scenarios; convert to `start_paused = true` + `advance` like the battery-timeout tests in the same file. Rename `floodlight_listener_exits_cleanly_on_subscribe_error` (`:1479`) while in the file — it tests the closed-channel path, and the real subscribe-error test already exists.

---

# Stage 2 — Delete and tidy before refactoring

Deleting first means every later stage carries less.

## S2-1. Remove the dead protocol command surface · S · DX

Re-verified at `1ea4231`: all six modules present and still zero call sites outside their own files across `src/`, `tests/`, `benches/` — `talk.rs` 904, `email.rs` 599, `pushinfo.rs` 148, `stream_info.rs` 122, `uid.rs` 114, `ping.rs` 97 lines. `crossbeam-channel` still in the tree solely for `talk.rs`'s blocking-in-async `AS-5` violation. Unreachable is not a reason to keep code unfixed; it is a reason to delete it. Trim the dead per-module fns (`get_dst`, `await_start`, `get_zoom`, `monitor_battery`, …) in the same pass.

**Verification, re-run at PR time:** the three call-form greps from the old plan; `fuzz/` and `decode-bc-pcap` still build; tarpaulin non-decreasing; `MSG_ID_*` constants in `bc/model.rs` stay (protocol vocabulary, documented in `baichuan-protocol.md` §5).

## S2-2. Reconcile the stale planning docs · S · DX

The `crates/` paths were fixed; the trait-shape claims were not, and the role-trait refactor added a layer of staleness:

- `remediation-plan.md:215` — P3-3 still describes "`CameraDriver` is a 40-method trait" pointing at the deleted `camera_driver.rs:28`.
- `code-paths.md:434,639,668,792` — the seam diagram still paints `CameraDriver` "~42 methods" red.
- `decoupling-plan.md:46` — "trait Camera is 33 methods" predates the eight-role split; its status line at `:266` describes the already-fixed `crates/` paths as still broken.

Strike the dead names, keep the findings that survived, point them at what shipped.

## S2-3. Delete the review's dead code · S · DX

- `StreamSource.gap_threshold` field + accessor (`stream_source.rs:527-529`) — used by exactly one test, doc references deleted code ("Task 5 … reads this from inside the reader loop"). Delete or demote to test-only.
- `emit_success_bytes` / `emit_failure_payload` (`run_support.rs:37,44`) — one-line wrappers with no callers outside their own tests. Leftover indirection from the `main.rs` extraction.
- `snapshot_json_preflight` (`oneshot/dispatch.rs:34-44`) duplicates `check_json_output` (`oneshot/snapshot.rs:36-44`) — same check, same error string, both run per invocation. One calls the other.
- `drive_reconnect_with_backoff` + `ReconnectOutcome` (`camera.rs:1556-1581`, `#[cfg(test)]`) duplicate `run()`'s connect/bail/backoff loop, so the backoff tests pass even if `run()` regresses. Either have `run()` call the helper or delete it and keep the `run()`-level tests.

## S2-4. Comment and doc rot sweep · S · DX

All violations of the house rule "never reference the current task/PR", verified surviving:

- **32 phase/task comments in `src/`** — `stream_source.rs` ×7 (178, 520, 607, 1193, 2403, 2508, 3086), `baichuan/bc/xml_tests.rs` ×8, `camera.rs` ×3 (858, 1879, 2380), `camera_tasks.rs` ×2 (1092, 1244), `rtsp/server/rtcp.rs` ×2, `mqtt/discovery/publisher.rs` ×2, and singles in `mqtt/discovery/mod.rs`, `config.rs`, `bcmedia_dump.rs`, `wake_server/route.rs`, and four baichuan files. Re-grep `Task [0-9]|Phase [0-9]|pre-fix|post-Phase` at PR time. Two half-scrubbed ungrammatical survivors: "used by 's replay-frame synth" (`stream_source.rs:1496`) and "'s gap marker must not" (`:1841`).
- **User-facing workspace-era staleness:** `CONTRIBUTING.md:26-40` still diagrams the deleted `crates/` tree; `README.md:490` still instructs `cargo bench -p bairelay_rtsp` (fails — no workspace); `scripts/publish-crates.sh:78-79` still claims "five crates inherit from `[workspace.package].version`"; ~20 `crates/…` path references in comments across `src/` and `tests/`.
- **Lying comments:** `camera_tasks.rs:294` claims a >32 KiB JPEG guard that does not exist in the body; `mqtt_dispatch.rs:471-472` claims "the bound is 60 s (production timeout)" while production is 15 s (`:86`); `Cargo.toml:102-105` documents `strip = "symbols"` two sections above where it lives (`:130`).

## S2-5. Naming and lint hygiene · S · design practice

- **`bc_camera` as the name for `Arc<dyn Camera>`/role-trait values** — the S4-2 refactor fixed the types but kept the adapter's name on the port: `camera_tasks.rs:30,209,388,420,457` parameters and the `CameraHandle` field + accessor (`camera.rs:325,748`). Rename to `camera` (NM-4: the whole point of the seam is that callers don't know it's Baichuan).
- **`interval_ms: u64` → `Duration`** at the poller seams (`camera_tasks.rs:211,303,390`), and name the inline `Duration::from_secs(10)` probe timeout (`:234,399,460`) once, as `preview_poller`'s `SNAPSHOT_TIMEOUT` already models.
- **`#[allow]` → `#[expect(..., reason)]`** (DP-6) for the six sites in `stream_source.rs` where it can hold (`:1355,1510,1763`; the cfg-gated `dead_code` ones at `:161,404,589` must stay `allow` — the expectation is unfulfilled in `cfg(test)` builds, and their comments already say why).

---

# Stage 3 — Pure-function extraction

No boundary moves. Each converts a decision that needs task choreography into one that needs a table, following `classify_battery_tick` / `classify_keepalive_tick` already in the tree. All verified still open.

## S3-1. Pacer re-anchor policy · S · best ratio in the plan

`media_pacer_task` still buries the three-rule target computation (future cap / dry-queue snap / initial latency) in an async loop. **Target:** `fn next_target(cursor: Option<Instant>, now: Instant, max_lead: Duration, initial_latency: Duration, snap_on_past: bool) -> Instant` — the full matrix, including the audio/video `snap_on_past` asymmetry, becomes a table test with no runtime.

## S3-2. Watchdog disconnect predicate · XS

Still inline at `src/watchdog.rs`. **Target:** `fn should_disconnect(idle_disconnect: bool, connected: bool, idle_for: Option<Duration>, grace: Duration) -> bool`.

## S3-3. Config warnings as values · S

Five `warn_*` functions still take `&Config`, return `()`, log inline — and their tests still assert nothing (TS-3 in its weakest form). **Target:** return `Vec<ConfigWarning>`; `check-config` and startup do the logging. Note `load_config` (new since the last revision) is the natural place to hang this.

## S3-4. `preview_poller` off the concrete handle · S

Survived S4-2: five of six `camera_tasks.rs` entry points now take role traits; `preview_poller:298` alone still takes `Arc<CameraHandle>`. **Target:** a two-method capability that `CameraHandle` implements, or hoist the connected-check to the caller.

## S3-5. Finish the live-verify marker contract · S

The `log_capture.rs` module doc ("the log text *is* the contract") landed; the marker coverage did not. Pinned today: only the two `startup_wake` markers. Still unpinned: `camera.rs` lifecycle markers ("Grace period expired, disconnecting" `:1275`, "Disconnected" `:1335`) and "RTSP server listening" (`rtsp/server/listener.rs:78`) — exactly the strings `manual-verify.sh` greps.

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

**Live-verify:** on the RTSP path — lands alone, gated by `tests/scripts/manual-verify.sh`. If hardware is unavailable, say so in the PR rather than implying verification. Carry S1-1's `src/sync.rs` move in this PR if it has not already landed.

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

# Sequencing

| Batch | Items | Gate | Hardware |
|---|---|---|---|
| **0** | S0-1, S0-2 | CI green | no |
| **1** | S1-1 … S1-6 | `cargo test` + `clippy` (+ tarpaulin ×3 for S1-6 — the flake must be shown dead) | no |
| **2** | S2-1 … S2-5 | `cargo test` + tarpaulin non-decreasing + out-of-tree builds | no |
| **3** | S3-1 … S3-5 | `cargo test` | no |
| **4** | S4-1 (+ S1-1 if not landed) | `manual-verify.sh` | **yes** |

Batches 0–3 need no camera — everything verifiable without hardware lands before the one item that is not. Within a batch, items are independent and can land in any order or in parallel.

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
