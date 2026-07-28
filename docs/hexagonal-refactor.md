# Hexagonal Refactor Blueprint

What a complete restructuring of bairelay would look like if it fully adhered to
`docs/rust-practices.md` and `docs/rust-code-structure.md`. This is a target-state analysis with a
phased path, not a commitment: § Calibration is explicit about which phases pay for themselves and
which are ceremony for this product.

Baseline assessment first, because it changes the shape of the answer: **bairelay is already about
70 % hexagonal.** The leaf crates are clean adapters, `StreamProvider` is a consumer-defined port
(`TR-2`) done correctly, sockets bind in the composition root, the error strategy matches `ER-1`,
wake locks are RAII guards, and every external dependency already has a trait seam with a fake.
The remaining 30 % is concentrated in three places: the port direction of `CameraDriver`, the
absence of a domain layer distinct from the binary, and `stream_source.rs` entangling the bridging
state machine with its I/O driver.

---

## The domain, named

DDD's first exercise: say what the domain actually is. Bairelay's core domain is **battery-camera
lifecycle policy** — everything downstream of "cameras sleep":

| Concern | Today lives in |
|---|---|
| Wake-lock counting, acquire/release semantics | `src/wake_lock.rs` |
| Grace-period countdown, reset-on-acquire | `src/grace_period.rs` |
| Idle-disconnect reconciliation policy | `src/watchdog.rs` |
| Connection lifecycle: discover → login → keepalive → reconnect; backoff; auth-terminality | `src/camera.rs`, `src/run_support.rs` |
| Gap bridging: `Live ⇄ Bridging`, I-frame re-broadcast, PTS synthesis, A/V realignment | `src/stream_source.rs` |
| Last-frame placeholder policy | `src/camera.rs` (`LastFrameBuffer`) |

Supporting subdomains: the Baichuan protocol (`crates/core`, generic/vendored), RTSP delivery,
MQTT/HA reporting, and the wake server (a camera-facing inbound adapter replacing Reolink's cloud).

Two DDD notes that simplify everything:

- **There is no persistence.** No repositories, no aggregates-as-transaction-boundaries, no unit
  of work. The outbound ports are device/broker/clock-shaped, not storage-shaped. Anyone proposing
  a `Repository` trait here has imported a pattern without its problem.
- **The aggregate is the per-camera state**: wake count + connection state + stream sources +
  last frame, owned by one root (`CameraHandle` today), mutated through it, with the per-camera
  task tree as its lifecycle. That boundary already exists; the refactor makes it a crate fact.

---

## Target structure

```
bairelay/
├── src/                        # binary: composition root ONLY
│   ├── main.rs                 # bind sockets, build config, wire adapters to services
│   ├── cli.rs                  # clap surface
│   └── oneshot/                # one-shot dispatch → app services, exit-code classify
├── crates/
│   ├── domain/                 # NEW — bairelay-domain (pure; deps: thiserror, bytes)
│   │   ├── wake.rs             #   wake lock, grace period (Clock-free: takes deadlines/instants)
│   │   ├── lifecycle.rs        #   connection state machine, backoff policy, auth-terminality
│   │   ├── bridging.rs         #   sans-IO gap-bridging state machine (see below)
│   │   ├── events.rs           #   CameraEvent enum: Motion, Battery, Floodlight, Pir, …
│   │   └── ports.rs            #   Camera, VideoSource, StatusSink, Clock  (defined HERE — TR-2)
│   ├── app/                    # NEW, optional — bairelay-app (orchestration)
│   │   ├── orchestrator.rs     #   per-camera task trees, supervisor, watchdog driver
│   │   ├── provider.rs         #   impl rtsp::StreamProvider, hooks wake lock  (inbound glue)
│   │   └── dispatch.rs         #   routes domain events → StatusSink
│   ├── core/                   # bairelay-camera-bc — BC protocol + `impl domain ports` (outbound adapter)
│   ├── rtsp/                   # unchanged — inbound adapter, keeps defining StreamProvider
│   ├── mqtt/                   # outbound adapter — impl StatusSink; HA payload translation only
│   └── wake-server/            # camera-facing inbound adapter — unchanged shape
```

Dependency arrows, all inward:

```
rtsp ──┐                                  ┌── core (BC protocol)
mqtt ──┼──►  app  ──►  domain  ◄── impl ──┤
wake-server ┘                             └── (fakes, behind test-util)
                 binary wires everything; only it names concrete adapters
```

`domain/Cargo.toml` listing `tokio` would be the mechanical failure signal
(`rust-code-structure.md` § the one invariant). It should compile on stable with `thiserror`,
`bytes`, and nothing else.

---

## The three structural corrections

### 1. Flip `CameraDriver` to a consumer-defined port

The clearest `TR-2` violation in the tree: `CameraDriver` lives in `crates/core` — the *producer* —
and mirrors "the subset of `BcCamera` the binary calls." The trait is shaped by what the vendored
type offers, not by what orchestration needs, and the leak is already visible: `CameraHandle`
keeps a parallel `bc_camera_concrete: Arc<BcCamera>` because two operations (`logout()`,
`StreamSource::start`) aren't on the trait. The seam has a hole exactly where the abstraction was
bent around the implementation.

Target: `domain::ports::Camera` (and `VideoSource`) defined by the domain in its own vocabulary,
including session teardown and stream start, so the concrete escape hatch disappears. `crates/core`
implements the port; `FakeCamera` moves behind the domain's `test-util` feature and implements the
same trait. Sketch:

```rust
// crates/domain/src/ports.rs
pub trait Camera: Send + Sync {
    async fn connect(&self) -> Result<Session, ConnectError>;   // dyn via async_trait or boxed
    async fn end_session(&self) -> Result<(), SessionError>;    // absorbs logout()
    async fn start_video(&self, ch: Channel, q: StreamKind) -> Result<Box<dyn VideoSource>, StreamError>;
    async fn battery(&self) -> Result<BatteryStatus, QueryError>;
    // …capability-grouped; split per TR-3 if fakes get wide
}
```

This phase also fixes the port's *vocabulary* (`TY-2`, `TY-6`): `BatteryStatus` carries
`Millivolts(i32)` instead of a raw `i32` documented as "actually millivolts"; zoom takes a
`ZoomLevel` whose one constructor does the ×1000, instead of every caller pre-multiplying; UIDs,
channels, and durations become newtypes at the port. The anti-corruption layer is the port
signature itself — BC/XML shapes stop at `crates/core`.

### 2. Extract the bridging state machine as sans-IO

`stream_source.rs` (5.4 kloc, the largest module) interleaves three altitudes: the gap-bridging
*policy* (Live/Bridging transitions, cached I-frame re-broadcast, PTS synthesis, audio-cadence
advancement), the tokio *plumbing* (200 ms ticker, broadcast channels, reader tasks), and the
*BcMedia translation*. The policy is the most intricate logic in the product and it is only
testable today by injecting packets through `PacketSource` into a live task loop.

Target (`rust-code-structure.md` § sans-IO): a pure `domain::bridging` machine —

```rust
pub struct Bridging { /* gap threshold, cached iframe NALs, PTS counters, state */ }

pub enum Input<'a> { Video(Frame<'a>), Audio(Frame<'a>), Tick { now: Instant } }
pub enum Output { Broadcast(Frame<'static>), EnterBridging, ResumeLive }

impl Bridging {
    pub fn handle(&mut self, input: Input<'_>) -> impl Iterator<Item = Output>;
}
```

Time arrives as a parameter; channels don't exist; the 200 ms ticker becomes a driver detail. The
existing `PacketSource` tests convert almost mechanically into table-driven fixture tests (bytes
in → decisions out), the PTS-realignment edge cases become property tests (`TS-5`), and the
machine becomes replayable against captured `.bcmedia` fixtures without a runtime. What remains in
the app layer is a thin loop: read source → `handle` → fan out.

This is the single highest-value move in the whole refactor: it takes the code with the highest
defect cost (A/V desync is the product's visible failure mode) from "testable via task
choreography" to "exhaustively testable as a value."

### 3. Introduce the domain crate; events as return values

Wake lock, grace period, watchdog *policy* (the decision "this camera should disconnect now" —
not the 30 s task), backoff schedule, and the connection state machine move to `crates/domain`.
The camera task tree, supervisor, and tokio wiring stay in the app layer as drivers of those
policies.

Event flow inverts per `rust-code-structure.md` § events-as-return-values. Today, listener tasks
call MQTT publishing paths directly; `mqtt_dispatch.rs` (1.4 kloc) knows both camera semantics and
broker topics. Target: listeners produce `CameraEvent` values; one app-layer router consumes them
and feeds a `StatusSink` port; `crates/mqtt` implements `StatusSink` as pure translation
(event → topic + HA payload). Consequences: the MQTT-outlives-supervisor subtlety (final
`disconnected` publish during teardown) becomes a property of one router task instead of a rule
every camera task must respect; and event routing becomes testable by asserting on `Vec<CameraEvent>`.

---

## Module disposition table

Where each binary module lands (sizes are current):

| Module | kloc | Disposition |
|---|---|---|
| `stream_source.rs` | 5.4 | **Split**: bridging policy → `domain::bridging`; translator loop + broadcast plumbing → `app`; `BcMedia` mapping → `core` adapter edge |
| `camera.rs` | 3.1 | **Split**: connection state machine + `LastFrameBuffer` policy → `domain::lifecycle`; `CameraHandle` (the aggregate root + task glue) → `app` |
| `camera_tasks.rs` | 1.9 | → `app` (pollers/listeners become drivers producing `CameraEvent`s) |
| `config.rs` | 1.7 | Stays in binary; gains a `TryFrom<RawConfig>` boundary producing domain types (validated durations, newtyped IDs) — config *shape* is a CLI concern, config *meaning* is domain |
| `mqtt_dispatch.rs` | 1.4 | **Shrinks**: routing → `app::dispatch`; payload translation → `crates/mqtt` |
| `cli_convert.rs`, `cli.rs`, `oneshot/*` | 2.9 | Stay in binary (inbound CLI adapter); oneshot handlers call app services instead of `BcCamera` directly, killing a second copy of connect/login logic |
| `run_support.rs` | 1.0 | Backoff *schedule* → domain; `sleep_or_cancel` → app |
| `wake_lock.rs`, `grace_period.rs` | ~0.4 | → `domain::wake`, unchanged logic (already pure — the model citizen) |
| `watchdog.rs` | 0.2 | Policy fn → domain; 30 s sweep task → app |
| `camera_provider.rs` | 0.2 | → `app::provider` (unchanged role: implements `rtsp::StreamProvider`, hooks wake lock) |
| `main.rs`, `supervisor.rs`, `orchestrator.rs` | 1.1 | `main.rs` thins to pure composition root; supervisor/orchestrator → `app` |
| `push_listener.rs`, `startup_wake.rs`, `preview_overlay.rs`, `tls_load.rs`, `bcmedia_dump.rs` | 2.7 | → `app` (push, startup-wake) / binary (tls_load, dump tooling) |

Untouched by design: `crates/rtsp` and `crates/wake-server` internals (already correct adapters),
the reproducible-build contract, the exit-code table, the toolchain pin.

## Language-practice cleanups riding along

Independent of structure, flagged by `rust-practices.md` against current dependency lists:

- `crates/core`: `log` → `tracing` (one telemetry system, `DC-5`); `lazy_static` → `std::sync::LazyLock`;
  `async-trait` retained only for dyn-dispatched ports, native `async fn`/`impl Future + Send`
  elsewhere; `crossbeam-channel` in async paths reviewed against tokio channels (`AS-5`).
- Port-level newtypes as listed in correction 1 (`Millivolts`, `ZoomLevel`, `Channel`, `Uid`).
- `#[expect]` over `#[allow]` for any lint suppressions encountered while moving code (`DP-6`).

---

## Phasing

Ordered so each phase is independently shippable, live-verifiable, and valuable if the effort
stops after it. The live-verify constraint is load-bearing: phases are cut so that RTSP-path and
MQTT-path changes land in *separate* diffs, each small enough for `manual-verify.sh` /
`ha-verify.sh` to cover.

| Phase | Change | Risk / verification |
|---|---|---|
| **0** | Safety net: confirm fixture-replay coverage of the bridging paths; capture any missing `.bcmedia` fixtures while the current code still runs | None — additive |
| **1** | Port flip: domain-owned `Camera` trait (in the binary first — no new crate yet), core implements it, delete `bc_camera_concrete`; newtypes at the port | Mechanical; unit + one-shot commands verify; no RTSP/MQTT behavior change |
| **2** | Extract sans-IO `Bridging`; translator loop becomes its driver; port `PacketSource` tests to fixture tables | The risky one — needs live-verify on the RTSP path (gap → bridge → resume, A/V realign) |
| **3** | Events-as-values + `StatusSink`; `mqtt_dispatch` shrinks to translation | Needs `ha-verify.sh`; RTSP untouched |
| **4** | Create `crates/domain`; move wake/grace/lifecycle/bridging/events; wire `default-members`, coverage config | Pure code motion if 1–3 landed; compile-enforces what 1–3 established |
| **5** | Core modernization (`tracing`, `LazyLock`, async-trait trim) | Independent; can interleave anywhere |
| **6** *(optional)* | `crates/app` split; `main.rs` to pure composition root; oneshot through app services | Lowest value-per-diff; do only if the binary keeps growing |

Phases 1–3 deliver ~80 % of the value while `crates/domain` doesn't exist yet — the boundary is
established as modules first (growth-path stage 1), then promoted to a crate (stage 3) once its
contents stop moving. Creating the crate first and shoveling code into it is the tempting wrong
order: it fossilizes today's module shapes before the port flip and bridging extraction reshape them.

---

## Calibration — what this buys, honestly

Applying our own doc's over-structure test (`rust-code-structure.md` § choosing a structure):

**Clearly pays.** Phase 1 (port direction + newtypes: removes a live leak, deletes the dual-handle
hazard, fixes the two documented unit-confusion traps — millivolts, zoom ×1000). Phase 2 (the
highest-complexity logic becomes property-testable and replayable; this is where the product's
user-visible bugs live). Phase 3 (event routing testable as values; kills the supervisor/MQTT
ordering subtlety as a distributed concern).

**Pays if sustained work continues.** Phase 4 — the domain crate's value is *enforcement*
(a `tokio` import in policy code becomes a build error instead of a review catch) plus faster
incremental builds. On a project with occasional contributors and agent-driven changes,
enforcement-by-compiler is worth more than usual; that tips it to "yes, after 1–3."

**Ceremony for this product, skipped.** An application-service *layer* with one service per use
case; `dyn`-injected everything (the composition root instantiates once — generics suffice,
`TR-1`); repositories/UoW (no persistence); event sourcing/CQRS (no storage, no audit
requirement); splitting `rtsp` internals (already a clean hexagon of its own); making `wake-server`
domain-aware (it's an adapter; its 1.7 kloc don't warrant internal layers). Phase 6 exists on the
list for completeness and should be resisted until the binary demonstrably regrows past its
composition-root role.

**The honest total.** A "complete refactor" touches ~15 of the binary's 27 kloc and grazes core's
public surface — weeks of effort with live hardware in the loop for two of the phases. The
incremental path above front-loads the payback so the project can stop after any phase and be
strictly better off than before; a big-bang rewrite to the target tree would spend the same effort
while parking the product unverifiable in the middle. If only one phase ever lands, it should be
Phase 2.
