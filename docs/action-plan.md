# Action Plan

The single plan document. Consolidates every prior planning doc — the staged plan
(stages 0–4, all landed except the S4-1 live-verify pass), `remediation-plan.md`
(2026-07-26 review), `decoupling-plan.md` (stage-4 design rationale), and
`hexagonal-refactor.md` (phases 1–5) — plus the **2026-08-17 full-codebase compliance
audit** (five passes — errors/panics, async/concurrency, types/API shape,
architecture/layering, tests/hygiene — against `rust-practices.md`,
`rust-code-structure.md`, and `CLAUDE.md`; every finding confirmed by reading the code,
not grep alone). The three retired docs are deleted; their still-load-bearing content
lives in § Design rationale and § Checked and clean below, and their landed history
lives in git.

Ordered by the project's stated priorities: **stability → developer experience →
design best practice → composition-based trait design**. Where an item serves several,
it is placed by the highest one it serves.

**Sequencing lives here and nowhere else.** `docs/code-paths.md` is the mermaid map of
the paths these items touch; `docs/rust-practices.md` / `docs/rust-code-structure.md`
supply the rule IDs cited throughout.

Line numbers below are point-in-time (2026-08-17, `main` @ c770f56) — re-measure before
relying on them.

---

# Resolved history

Stages 0–3 landed 2026-08-08; S4-2 (role-trait split) and S4-1's code (sans-IO
`stream_translate.rs`) landed by 2026-08-16. The full resolved table — what shipped,
deltas, and evidence — lives in this file's git history (revision before 2026-08-17).
Do not re-litigate it.

**Two resolved claims were found incomplete by the audit and are reopened below:**

- *"Test scaffolding shipped in release builds"* (`8b4f2f6`) gated the stream-source
  hooks but missed `mqtt::test_support` and `config::test_helpers` → S5-6.
- *"No-panic policy … enforced by clippy lints"* covers `unwrap`/`expect` only; the
  `panic!`/`unreachable!`/`assert!`/indexing half of the rule is unlinted, and CI never
  lints feature-gated code (`--all-features` absent) → S5-5.

One remediation item was never closed: **P3-2** (binary manufactures the vendored
`bc_protocol::Error::Other` for binary-layer failures — still live at six
`mqtt_dispatch.rs` production sites) → folded into S6-5.

---

# Stage 4 — carried forward

## S4-1v. Live-verify the sans-IO translation refactor · **OPEN, needs hardware**

The only survivor of the previous plan. `stream_translate.rs` landed 2026-08-16
behaviour-preserving by unit/driver tests, but `tests/scripts/manual-verify.sh` has not
run against it and `tests/fixtures/` holds no `.bcmedia` files, so `fixture_replay`
passed as a no-op. Before or with the next release: `bairelay capture <cam> --output
tests/fixtures` per camera/stream, `cargo test --test fixture_replay`, then the full
`manual-verify.sh` (and `ha-verify.sh` if HA is reachable).

**Combine with batch 5a below** — S5-1/S5-2 touch the same RTSP/MQTT paths, so one
hardware session can gate both.

---

# Stage 5 — Stability (2026-08-17 audit, critical + high)

## S5-1. RTSP write deadline — a stalled client leaks a wake lock forever · S

`rtsp/server/transport.rs:73-78` holds the per-connection writer mutex across four
`write_all` calls with no deadline. A client that stops reading (zero TCP window) parks
the write forever; `dispatch_one` (`session_task.rs:153`) awaits it outside its
`select!` so cancellation can't preempt; the drain at `session_task.rs:359` never
returns; `sessions.remove` at `:365` never runs; the `SubscriptionHandle` →
`WakeLockGuard` is never dropped — **the battery camera never sleeps**, and the control
loop can't even answer TEARDOWN (same mutex). Highest-impact finding in the audit: one
misbehaving client defeats the idle-disconnect design. Fix: a write deadline in
`write_framed`, or race `dispatch_one` against the session token. On the RTSP path →
live-verify gate (ride with S4-1v).

## S5-2. Timeout every MQTT publish; audit the wake-lock-holding sites · S

`MqttStatusReporter::report` (`mqtt_status.rs:60-73`) has no timeout at any layer;
rumqttc parks callers when its 256-slot queue fills during a broker outage. Confirmed
unbounded sites: `push_listener.rs:221` (**holds a `WakeLockGuard`** — the motion-off
publish at `:234` has a 1 s timeout, the motion-on one doesn't), `camera_tasks.rs:486`
awaited inline by `camera.rs:1252` during session startup, `camera.rs:1099/1187/1345`
(blocks teardown), `main.rs:632` (inside the event-loop `select!` arm), plus the five
poller publish sites. `orchestrator.run()` awaits every camera task with no budget, so
one wedged publish blocks `main()` from reaching shutdown. Fix once inside
`MqttStatusReporter::report` (the reasoning already exists at `main.rs:278`), then
special-case the two sites above.

## S5-3. Stop logging credential material at default-on / ungated levels · S

`login.rs:477/544/630` `{:?}`-dump whole `LoginUser` bodies (documented *"password in
plain text"*) at `debug!` with **no `context.debug` gate** — contradicting CLAUDE.md's
"wire dumps need per-camera `debug = true`". Also `login.rs:476` (sigV3 nonce +
iteration count at INFO — offline-attack material), `:485` (reject body at WARN, fires
on every auth failure), `wake_server/register.rs:346/361` (camera UID at INFO,
non-vendored). The hardened pattern exists at `bc/de.rs:129` (gated **and** `trace!`,
with a comment explaining exactly this leak class) — apply it. Vendored file, but this
is the auth path; fix in place.

## S5-4. Classify MQTT auth failures as terminal · S

`mqtt_loop.rs:119-127` discards the `ConnAck` code and collapses every error to
`LogError`, so bad broker credentials retry forever at 30 s intervals with one warning
per minute — violating the house rule the camera path implements correctly
(`bc_camera.rs:55-61` classifies, `camera.rs:1435-1441` breaks permanently). Match
`ConnAck.code` (`BadUserNamePassword`, `NotAuthorized`) and
`ConnectionError::ConnectionRefused` → terminal error, mirror `classify_login_error`.

## S5-5. Finish the lint wall; lint what CI never sees · S

Production is genuinely clean of `unwrap`/`expect`, but the stated no-panic rule is
only half-enforced: add `#![warn(clippy::panic, clippy::unreachable, clippy::todo,
clippy::unimplemented, clippy::indexing_slicing)]` to `lib.rs`/`main.rs` (clippy.toml
already exempts tests). Seven sites currently pass silently — triage them in the same
change: runtime `assert!` on camera-derived NALs in the public packetizers
(`rtsp/codec/h264.rs:67`, `h265.rs:59`; add `# Panics` docs or degrade), a message-free
`unreachable!()` in an untrusted-input parser (`rtsp/protocol/transport.rs:95` —
restructure the `if`s), and a fragile one whose guarantee lives 180 lines away on an
MQTT-triggered path (`mqtt_dispatch.rs:212`). Separately: CI runs no `--all-features`
lane, so `test-util`/`fuzz-api` code is never linted — add one.

## S5-6. Finish gating the test doubles (reopens `8b4f2f6`) · S

`mqtt/mod.rs:6` declares `pub mod test_support;` ungated while `test_support.rs:14`
falsely claims it's behind `test-util`; `SharedMqttClient::__test_new_with_capture` /
`for_test_stub` (`mqtt/client.rs:126/142`) are live release symbols; same for
`config.rs:896` `pub mod test_helpers`. Add `#[cfg(any(test, feature = "test-util"))]`
to all four, fix the false doc comment, and extend `Cargo.toml`'s feature doc to name
the mqtt helpers. While there: `camera.rs:534/623` use `#[doc(hidden)]` where
`stream_source.rs:552` uses the real cfg for the identical constraint — make them
match.

## S5-7. Close the suite-hang holes in the tests · M

12 async tests await an unbounded primitive with no timeout (house rule + AS-9). Worst:
`rtsp/server/transport.rs:290` — bare `recv_from` on real UDP loopback, which can hang
CI with *no code regression at all*. Also `transport.rs:232/255` (`read_exact` on
duplex pairs), `tests/rtsp_multi_track_setup.rs:159/383` (DESCRIBE reads that bypass
the file's own timeout helper), vendored `mock.rs:477/556` + `bcsub.rs:136` (the
sibling test in the same file does it right), and three vendored paused-clock
`task.await` joins (`pirstate.rs:326/363`, `services.rs:867`) that a later `timeout()`
could never rescue. Then the 18 `cancel-then-join` sites in `camera_tasks.rs` — wrap
like `:823` already does.

## S5-8. Motion Start/Stop coalescing silently drops wake-on-motion · S

`next_motion` (`baichuan/bc_protocol/motion.rs:43-73`, vendored) drains N buffered
events and returns only the last; the consumer (`camera_tasks.rs:75`) keys the wake
lock off Start/Stop edges, so a Start+Stop inside one poll window delivers only Stop
and wake-on-motion no-ops for that event. Return the drained sequence (or deliver
edges) and handle them in order.

## S5-9. Reconcile the grace-period contract · S — **decided 2026-08-18: docs follow code**

`grace_period.rs:22-53` is check-at-deadline: it sleeps the full window and checks
idle state once, so an acquire+release pair inside the window is invisible and a
session can be cancelled ~½ s after the camera was last used; the watchdog doesn't
cover this path (`GracePeriod::run` returning is what fires `sc.cancel()`,
`camera.rs:1276`). The decision: check-at-deadline is the accepted behaviour.
CLAUDE.md now describes it accurately. **Remaining:** pin
the accepted semantics with a test — held-at-deadline keeps the session; a brief
acquire+release inside the window does not re-arm.

## S5-10. Cap and token the network-driven spawns · S

`main.rs:618` spawns per inbound MQTT control message — no token, no cap, not joined
at shutdown, each acquiring a wake lock; give it the mqtt loop's cancel and a `JoinSet`
drained on shutdown. `wake_server/register.rs:223-224` spawns two tasks per `C2R_C`
request with no rate limit; `push_listener.rs:174/258` spawns per connection, uncapped,
each holding a wake lock. The correct shape exists at `rtsp/server/listener.rs:99`
(`Semaphore` permit-before-accept).

---

# Stage 6 — Design best practice (audit, moderate)

## S6-1. One enum for connection state · M

`CameraHandle` holds `state: Arc<RwLock<CameraState>>` (`camera.rs:321`) and `camera:
RwLock<Option<Arc<dyn Camera>>>` (`:326`) under separate locks; readers can observe
`Connected` with a `None` driver, and `mqtt_dispatch.rs:73-95` already pays with a 15 s
defensive poll loop. Fold into one `RwLock<ConnState>`
(`Disconnected | Connecting | Connected(Arc<dyn Camera>)`) per TY-1.

## S6-2. Newtypes where swaps compile silently · M

Three adjacent bare `IpAddr` params on `handle_connection`
(`rtsp/server/connection.rs:78`, passed positionally at `listener.rs:144/173` — a
peer/local swap sends RTP to the wrong host); `add_user(user_name: String, password:
String, …)` (`camera.rs:164`); `aac_pts_next`/`g711_pts_next` as identical `u32` in
different clock domains (`stream_translate.rs:56-57` — cross-assignment is the A/V
desync bug class this module exists to prevent). TY-2's textbook cases; fix these
three, leave the rest to the backlog.

## S6-3. Name the literals that must agree across files · S

One pair has already drifted: `camera_tasks.rs:38-41` claims "the same 1 → 60 s ladder
as the camera reconnect path" while `camera.rs:1386` codes 2 s. Also: `stale_after`
80 000 ms in two files (`wake_server/config.rs:55`, `main.rs:532`); the RTSP session
timeout as one sweep constant plus three `"timeout=30"` format strings
(`connection.rs:139/373/811/930`); the idle-floor math duplicated with four bare
literals (`config.rs:1078-1104`); the floodlight `30` pinned by two tests instead of a
constant (`mqtt_dispatch.rs:103`). One named constant per family (NM-6).

## S6-4. Decide and enforce the eroded layer boundaries · M

Four seams, each needing a written decision rather than silent drift:

- **`wake_server` → `baichuan`** (6 sites, incl. a vendored error `#[from]` in the
  *public* `WakeServerError`, `wake_server/mod.rs:32`). The codec dependency is
  probably right — so amend CLAUDE.md's "know nothing about each other" to name
  `baichuan::bcudp` as a shared codec tier (and add `src/sync.rs` to the layout
  table), or remove the edge. Don't leave the rule provably false.
- **`StatusPublisher` escape**: `mqtt_loop.rs:71` constructs one directly and makes a
  semantic status decision outside the `StatusReporter` seam. Route
  `publish_shutdown_fanout` through the reporters (`camera.rs:640` already builds
  them).
- **Discovery bypasses the port**: `camera.rs` names `crate::mqtt` at 11 sites for HA
  discovery, putting `MqttError` in `CameraHandle`'s public API while status goes
  through `StatusReporter`. Either a `DiscoveryReporter` port or narrow the port's
  charter in writing. Doing this also shrinks the `impl CameraHandle` god block
  (§ Deferred).
- **rumqttc taxonomy leak**: `mqtt/mod.rs:16` re-exports rumqttc types and
  `mqtt_loop.rs` matches on them camera-side. Move `classify_event`'s rumqttc matching
  into `src/mqtt/` behind the existing `EventAction` enum and drop the re-export.
  (Pairs naturally with S5-4, which edits the same match.)

Also: wake-lock semantics are baked into RTSP contract text (`rtsp/provider.rs:83`
`#[must_use]` message, `rtsp/server/registry.rs:60/64` doc comments) despite
`provider.rs:91` declaring the handle opaque — reword to "ends the subscription".

## S6-5. Error seams: stop laundering through the vendored enum · M

Successor to remediation P3-2, still live: `mqtt_dispatch.rs:188/227/256/276/301/357`
manufacture `bc_protocol::Error::Other(...)` for binary-layer failures. Plus the
audit's additions: first-party code matches vendored variants directly
(`camera_tasks.rs:178`, `oneshot/classify.rs:37-38`) — extend the `bc_camera.rs:38`
`ConnectError` pattern (narrow enum, cause via `#[source]`); stringly `Result<_,
String>` in `config.rs:702/764/1158` and the untrusted-input parser
`try_consume_request` (`rtsp/server/connection.rs:243` → `MessageError`); `tls_load.rs`
anyhow-in-a-library including an ER-4 impossible-state `Err` at `:59-64` (make the
state unrepresentable or panic).

## S6-6. Split `DeviceAdmin` · M

`camera.rs:149-186` staples identity + clock + users + services into 11 methods; six
consumers each use 1–4 and all get user-deletion powers; `FakeDeviceAdmin` stubs all
11. Split per its own doc sentence: `Identity` (`capabilities`, `version`,
`ability_info`), `Clock` (`set_time`), `Users` (4), `Services` (2), `reboot` →
`Session` or its own. `Lighting` (7 methods; siren/status-light consumers use 1 each)
is the same smell milder — judge while in the file. Keep the roles flat (S4-2 rule).

## S6-7. Move blocking and CPU-heavy work off the async workers · S

`bcmedia_dump.rs:116-192` does synchronous `std::fs` per media packet from the
translator task (opt-in, but a slow disk stalls every stream on that worker) —
`spawn_blocking` or a writer task. `preview_overlay.rs:35-52` JPEG-decodes/re-encodes
up to ~1.5 MiB inline in the poller — `spawn_blocking`; and hoist the caption
short-circuit above `fast_hash` (`:265`), which currently hashes the whole payload
every tick regardless of state (two-line fix, do it first).

## S6-8. Make `CameraConfig`'s reachability a type · M

`config.rs:322-360` is the TY-1 textbook case: `discovery` enum + five `Option`s
policed by four hand-written validators (`config.rs:1291-1318`). A `CameraReach {
Local { address }, P2p { uid }, Cloud { uid, account } }` built once at parse time
deletes the validators and the downstream `uid` unwrapping. Serde shape stays; the
conversion lives in the existing hydrate step.

---

# Stage 7 — Backlog (audit, low/style — opportunistic only)

Apply when already touching the file (`rust-practices.md` § Applying to existing
code) — none of these justify a standalone churn PR. Grouped by theme:

- **Weakened types**: `bridging: bool` across the translate signatures while
  `GapState` exists; `json: bool` vs the existing `Mode`; `build_digest_challenge`'s
  bool; `set_service(Option<bool>, Option<u32>)`; port as `u32` beside `cli.rs:133`'s
  comment on why `u16`; TLS paths as `String` / unnamed `(String, String)` tuples;
  bare-seconds/millis fields (`battery_update` doesn't name its unit; wake_server's
  `RuntimeConfig` keeps `_ms` fields while push_listener's uses `Duration`);
  `SocketAddr` rebuilt via `format!…parse()` (`main.rs:381`); `translate`'s
  four-reason `Option<u32>` return.
- **API shape**: `Arc<HashMap<String, Arc<CameraHandle>>>` in four public signatures →
  a `CameraRegistry` handle type; lock-handout getters (`audio_presence()`,
  `sdp_params_handle()`) where the file already documents the return-a-`Copy` pattern;
  `&Arc<T>` params that never clone; `get_camera() -> Option<&Arc<..>>`.
- **Naming**: `json_mode` vs `mode_json` (same forwarded value); `ServiceName` vs
  `ServiceKind` + the 6-arm converter; `supervisor::Service` vs camera-port "Service";
  telescoping `with_*_and_*` constructors beside the working builder pattern;
  `default_2000()`; `run_support.rs` as a role-named module; the one narrow-role miss
  (`startup_wake.rs:193` takes `Camera`, uses only `snapshot`).
- **Suppressions & docs**: 38 `#[allow]` vs 3 `#[expect]`, zero `reason =` (one cites
  "this phase" — also DC-7); module docs missing from exactly the files CLAUDE.md
  describes in prose (`camera.rs` has the content as `//` — one character per line;
  `wake_lock.rs`, `watchdog.rs`, `orchestrator.rs`, `config.rs`, `grace_period.rs`,
  `cli.rs`, `main.rs`); three DC-7 archaeology comments (`stream_source.rs:199`,
  `camera.rs:281`, `mqtt_status.rs:7`); stale workspace-era text in
  `docs/implementation.md:470`.
- **Test hygiene**: `fixture_replay.rs:2307` burns ~3.2 s wall time every run (the one
  unconditional budget breach); 17 tests sleep ≥100 ms unpaused;
  `udp_bind_skips_occupied_pair` silently passes on port collision;
  `push_listener.rs`'s probe-and-drop port race lacks the retry its RTSP siblings
  have; tautological self-tests of the fake (`fake_camera/mod.rs:664/705/736`).
- **Coverage gaps**: no `bcmedia` fuzz target (every sibling `de.rs` has one); RTP
  packetisers have neither fuzz nor proptests (benched only).
- **Async nits**: `floodlight_poller` missing the `MissedTickBehavior::Delay` its four
  siblings set with comments (`camera_tasks.rs:412`); `stream_source_create_lock` is
  an async mutex with no await under it (`camera.rs:339/817`); the paced-frame
  `select!` can drop one taken frame at teardown (`stream_source.rs:881` — comment,
  don't rewrite); `rumqttc::poll()` raced against cancel without a cancel-safety note
  (`main.rs:606`); wake_server's loser `JoinHandle` detached, not aborted
  (`wake_server/mod.rs:104`); six per-packet `debug!` drop-path logs in the translate
  hot loop → `trace!` (`stream_translate.rs:392-612`); two `format!`-before-log sites
  (`camera.rs:1438/1442`); a `// only one waiter permitted` invariant comment on
  `WakeLockCounter::notify_future` (`wake_lock.rs:98`).
- **Vendored, fix only if in the file anyway**: ~45 capitalized `Display` messages +
  the indistinguishable `Timeout`/`TimeoutError` pair + the "coversion" typo
  (`errors.rs`); the `Other(&str)`/`Cloud(String)` catch-alls (S6-5 reduces first-party
  exposure); two `Drop` impls missing the `try_current()` guard their siblings have
  (`motion.rs:216`, `stream.rs:89`); `cloud.rs`'s process-global `TEST_BASE`
  single-test mock.
- **Documented invariant gaps** (from the 2026-07 review, still open):
  `rtsp/codec/aac.rs:122` does `(au.len() as u16) << 3`, silently wrapping the 13-bit
  AU-size field for frames ≥ 8192 bytes — not reachable with real AAC; a
  `debug_assert!` would document the invariant.

---

# Deferred, with triggers

| Item | Trigger to revisit |
|---|---|
| **Discovery build/classify split** (D3; `discovery.rs` ~3,295 lines) | When a discovery change is already needed. Failure mode is "camera not found", not "video is wrong". |
| **Splitting `impl CameraHandle`** (`camera.rs:411-1519`, 42 methods, 4 seams: lifecycle, stream-source registry, session tasks, MQTT/HA publishing) | S6-4's discovery port extracts seam 4; lift the stream-source registry when next touching it; lifecycle + session tasks stay together (one state machine). |
| **Splitting `stream_source.rs`** (~1,557 prod lines post-S4-1) | Deliberately the fan-out driver per CLAUDE.md. The three pacer tasks (`:799-958`) are the nameable seam if it grows again. |
| **`rand` migration** (three locked versions) | `cargo-deny` bans caps it. Migrate when `rand` is touched anyway. |
| **`[lints]` + `missing_docs` sweep** | Only if the lib surface becomes a real API (S0-1 removed the reason). |
| **`#[non_exhaustive]`** | `config::*` types only. |
| **Splitting `rtsp/server/connection.rs`** | Extract `build_transport` on the next substantive `handle_setup` change. |
| **Vendored BC types in role signatures** | Re-measure per role after S6-6; anything reachable only from the admin roles keeps its BC type indefinitely. |
| **`ConnectedStills` test double** | 1 impl, 0 doubles today; add a 10-line fake when next testing the asleep branch. |
| **A domain crate** | A second binary, or an external consumer of the policy code. |

---

# Design rationale

Absorbed from the retired `decoupling-plan.md` and `hexagonal-refactor.md` — the
reasoning the deferred items above lean on, kept so a future change lands in a decided
shape rather than re-litigating it.

**Discovery split target shape** (for the D3 deferral). Per-verb
`fn build_<verb>(...) -> UdpXml` and `fn classify_<verb>_reply(UdpXml) -> Result<Decision>`,
leaving each `async fn` as a send/recv/retry driver. The XML shapes are specified in
`docs/baichuan-protocol.md` §9, so expected values are written down rather than
inferred from the code under test.

**Why no domain crate.** Built during the hexagonal refactor's phase 4 and reverted.
The crate wall could only stand in front of the modules least likely to grow an I/O
dependency — the `Camera` trait's signatures carry vendored BC types, keeping the
actual protocol boundary outside it — and the cost was a publishable artifact with no
external consumer. What replaced it: modules named for their subject, the same
discipline enforced by review (the normal stage-1 posture per
`rust-code-structure.md` § growth path). Narrower role traits make the wall *less*
necessary, not more. Trigger to revisit is in the table above.

**DDD ceremony deliberately skipped.** There is no persistence, so no repositories,
no aggregates-as-transaction-boundaries, no unit of work, no event sourcing, and no
per-use-case application-service layer. The permanently useful parts were taken —
value objects, anti-corruption at every external representation, events as return
values — and the rest has nothing to hold up.

**`log_capture.rs` is not the "assert on logs" smell.** It exists to pin the literal
marker strings `tests/scripts/manual-verify.sh` greps; the log text *is* the contract
there. Keep it.

**Blanket local twins for vendored BC types stay out.** Field-for-field copies with
no second implementation would be pure mapping ceremony; the per-role re-measure
after S6-6 (deferred table) is the surviving version of this idea.

---

# Checked and clean

Verified negatives, so the next review doesn't re-derive them. Merged from the
2026-07-26 review and re-confirmed or extended by the 2026-08-17 audit.

- **`wake_lock.rs`** — acquire/release/`idle_since` interleavings traced twice, no
  lost-wakeup race; the permit-storing `notify_one` + re-check loop is correct.
  Latent: `notify_release` supports exactly one waiter (only `GracePeriod::run` waits
  today) — worth an invariant comment, not a bug.
- **No lock held across `.await` anywhere** — machine-enforced
  (`clippy::await_holding_lock` + `-D warnings`), zero suppressions.
- **No unbounded channels** — all 12 production channels bounded with reasoned,
  commented capacities and explicit backpressure policy at every producer.
- **Zero production `unwrap()`/`expect()`** — poison recovery centralised in
  `src/sync.rs`; lint-enforced in CI.
- **RTSP request framing** — oversize and overflowing `Content-Length` rejected
  before arithmetic (`checked_add` + cap); pipelined requests drained per read.
- **`src/wake_server/`** — every in-memory map capped (`MAX_MAP_ENTRIES = 1024`,
  route `CACHE_CAP`), refresh-vs-insert distinguished; hostile-flood memory
  amplification handled.
- **`src/mqtt/`** — control messages validate the camera name against an ASCII
  allowlist before use.
- **Crypto** — the constant IV in `baichuan/bc/crypto.rs` is Reolink firmware's
  constraint, correctly documented; freshness comes from the per-session derived key.
- **Wire-parser hardening** — all indexing guarded by length checks; the dangerous
  arithmetic sites explicitly clamp or `checked_add`, with comments. Load-bearing:
  release builds have no `overflow-checks`, so these guards are the protection.
- **Drop impls** — all 13 avoid blocking, awaiting, and failing.
- **Config parsing** — the placement scan covers the exact top-level TLS-key shape
  `manual-verify.sh --tls` generates, plus misplaced-key failure cases.
- **Reproducible builds** — no `build.rs`, no git deps, lockfile committed,
  `--locked` in CI.

---

# Risk ranking

Assessed 2026-08-17, post-audit.

| # | Item | Likelihood | Impact | Failure scenario | Effort |
|---|------|:---:|:---:|---|:---:|
| 1 | **S5-1 wake-lock leak** | Med | **High** | One client with a stalled TCP window and the camera never sleeps again — the product's core promise, silently broken until the battery dies. | S |
| 2 | **S5-2 unbounded publishes** | Med | High | Broker outage wedges wake-lock release, session startup, and shutdown; recovery requires killing the daemon. | S |
| 3 | **S5-3 credential logging** | High (any `RUST_LOG=debug` run) | High | Plaintext password bodies land in journald/log aggregation; INFO/WARN sites leak on default config. | S |
| 4 | **S4-1v live-verify not run** | Med | High | A subtle A/V regression from the translation refactor surfaces only on hardware. | S (one session) |
| 5 | **S5-7 suite hangs** | Med (UDP one: spontaneous) | Med | `cargo test` burns a CI job to its timeout with no diagnostic output. | M |
| 6 | **S5-4 auth retry loop** | Low (needs a typo) | Med | Misconfigured broker credentials hammer forever, one warning/minute. | S |
| 7 | **S5-6 doubles in release** | Low | Med | A release consumer wires the mock MQTT client; the doc claims it's impossible. | S |

---

# Sequencing

| Batch | Items | Gate |
|---|---|---|
| **5a** | S5-1, S5-2, S5-3, S5-4 | `cargo test` + **one hardware session covering S4-1v** (`manual-verify.sh`, fixture capture, `ha-verify.sh` if reachable) |
| **5b** | S5-5, S5-6 | CI green incl. the new `--all-features` lane; triage of the 7 unlinted panic sites reviewed |
| **5c** | S5-7, S5-8, S5-9, S5-10 | `cargo test` (×3 for the de-flaked suite) + tarpaulin non-decreasing |
| **6** | S6-1 … S6-8, individually as touched or as small standalone PRs | `cargo test`; S6-4's CLAUDE.md amendment reviewed as a design decision |
| **7** | Backlog | Riding along only — no standalone churn |

5a before everything: it is the smallest batch that removes the two ways the daemon can
silently stop doing its job, and it shares the hardware session S4-1v already owes.

---

# Review checklist

Applies to any PR touching the camera port, the stream path, the protocol module, or
logging. Rules 1–15 carried from the previous revision (in force since 2026-08-08);
16–19 added from the audit.

**Composition**

1. **New trait?** Names a capability (`can-do`), not a taxonomy. The role traits stay flat.
2. **New camera-facing method?** Lands on exactly one role trait; the PR names the consumer.
3. **Consumer takes a camera?** The narrowest role that covers it — never `Arc<dyn Camera>` outside wiring/dispatch.
4. **Reuse between types?** A held field with explicit delegation, never `Deref`. (TR-6)
5. **Closed set of outcomes?** `enum` + exhaustive `match`. (TY-5)

**Testability**

6. **New logic in a `translate`/`handle` function?** No `Sender`, `Arc`, or `RwLock` in the signature.
7. **New decision inside an async loop?** Extracted as a pure `fn` taking time and state as parameters.
8. **New timing-sensitive test?** `start_paused = true` + `advance`, never wall-clock sleeps.
9. **New `warn_*` or diagnostic?** Returns a value; the caller logs.
10. **New test asserting "does not panic"?** Rejected — the unit is the wrong shape. (TS-3)

**Stability**

11. **New `pub fn` on `BcCamera`?** Has a caller in `src/` in the same PR.
12. **New shared lock crossing a task boundary?** Justified against OW-5/AS-4; uses the poison-recovering helpers.
13. **New RAII guard?** `#[must_use]` — with a message naming the failure, not the mechanism (S6-4).
14. **Command handler replies to an operator?** The reply reflects the actual outcome.
15. **New log string that `manual-verify.sh` greps?** Pinned by a `log_capture` test.
16. **New await on a peer, broker, or disk?** Carries a deadline, or the PR states why the watchdog covers it. (AS-6, S5-1/S5-2)
17. **New log line carrying a payload or identifier?** Checked against DC-6: no credential material at any default-on level; wire dumps gated on per-camera `debug` and emitted at `trace`. (S5-3)
18. **New `tokio::spawn`?** Reachable by a cancellation token or a stored handle, and capped if driven by network input. (AS-2, S5-10)
19. **New async test awaiting a channel, socket, or join?** Wrapped in `tokio::time::timeout` — including cancel-then-join. (AS-9, S5-7)
