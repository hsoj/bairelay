# Hexagonal Refactor Blueprint

What a complete restructuring of bairelay would look like if it fully adhered to
`docs/rust-practices.md` and `docs/rust-code-structure.md`. This is a target-state analysis with a
phased path, not a commitment: § Calibration is explicit about which phases pay for themselves and
which are ceremony for this product.

> **Status: phases 0–5 are implemented, with one deliberate departure from the sketch below —
> none of it lives in a new crate.** The separation the blueprint wanted is real, but it is drawn
> with modules inside `src/`, and the modules are named for their subject matter (`battery`,
> `ptz`, `camera_services`, `camera_status`, `gap_bridging`) rather than for their architectural
> role (`domain/`, `ports.rs`, `*_adapter.rs`). See § Phasing for what each phase did, § Why no
> domain crate for the reasoning, and § Deferred for what was consciously left alone.

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

*Historical — this is the shape originally sketched. What was built keeps the separation but
draws it with modules in `src/`; see § Why no domain crate and § Naming.*

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

| Phase | Change | Status |
|---|---|---|
| **0** | Safety net: confirm fixture-replay coverage of the bridging paths | ✅ existing suite carried through every phase green |
| **1** | Port flip: consumer-defined `Camera` trait, core implements it, delete `bc_camera_concrete`; newtypes at the boundary | ✅ `trait Camera` in `src/camera.rs`, implemented for `BcCamera` in `src/bc_camera.rs`; `CameraDriver` and the parallel concrete handle deleted; `Millivolts` (`battery.rs`), `ZoomLevel` / `PresetSlot` (`ptz.rs`), `ServiceKind` (`camera_services.rs`) added; the 12 service RPCs collapsed to `service` / `set_service`; `FakeCamera` moved to `src/fake_camera.rs` |
| **2** | Extract sans-IO gap bridging; translator loop becomes its driver | ✅ `src/gap_bridging.rs`; four `Arc<Mutex<_>>` fields collapsed into one policy handle; audio gate takes a `bool` instead of a mutex. **Not live-verified** — no hardware |
| **3** | Events-as-values + a status port; publishing shrinks to translation | ✅ `src/camera_status.rs` holds `CameraEvent` and `StatusReporter`, implemented by `src/mqtt_status.rs`, which also owns the republish-cache write that every task used to duplicate. **Not HA-verified** — no broker |
| **4** | Compile-enforce the boundary via a crate split | ⛔️ **Not done, deliberately** — see § Why no domain crate |
| **5** | Core modernization (`tracing`, `LazyLock`, async-trait trim) | ✅ 127 `log::` sites → `tracing` (they had been going nowhere — no bridge was installed); `lazy_static` → plain `const`; `env_logger` dev-dep dropped. `async-trait` correctly retained: both remaining traits are genuinely `dyn`-dispatched |
| **6** *(optional)* | `crates/app` split; `main.rs` to pure composition root | Not built — still the right call per § Calibration |

## Why no domain crate

The blueprint's phase 4 argued for `crates/domain` so that an errant `tokio` import in policy code
becomes a build error. That enforcement is real, but it was not worth what it cost here:

- **A crate is the wrong unit for this.** `rust-code-structure.md` § the growth path says to use
  the smallest tool that enforces the boundary, and to graduate to a crate when the component has a
  genuinely different consumer, needs different dependencies, or is large and stable. The policy
  code has exactly one consumer — this binary. The four existing crates all clear that bar: each is
  published to crates.io, and `crates/core` is additionally driven by the out-of-workspace `fuzz/`
  and `tests/scripts/decode-bc-pcap/` projects with their own feature flags.
- **The enforcement was mostly theatre.** Only the genuinely pure modules could live under the
  restriction; the `Camera` trait — the actual boundary against the protocol crate — could not,
  because its signatures carry BC report types. So the crate wall stood in front of the code least
  likely to grow an I/O dependency, while the code most exposed to one stayed outside it.
- **It cost a public API and a release-process edge.** A workspace member is a publishable
  artifact with its own version, description, and lockstep-bump obligation, for something no
  external consumer will ever depend on.

What replaced it: modules named for their subject, and the same discipline enforced by review —
which is what `rust-code-structure.md` describes as normal for stage-1 boundaries.

## Naming

Modules say what they are about, not what architectural role they play. `domain/`, `ports.rs`,
`*_adapter.rs`, and `*_sink.rs` were all pattern vocabulary rather than the vocabulary of Reolink
battery cameras, and `rust-code-structure.md` § Tactical DDD is explicit that ubiquitous language
belongs in identifiers (`NM-4`). The mapping used:

| Was | Is | Because |
|---|---|---|
| `domain/types.rs` | `battery.rs`, `ptz.rs`, `camera_services.rs` | Three unrelated subjects had been pooled under one architectural label |
| `domain/events.rs` + the status port | `camera_status.rs` | What a camera reports, and where it goes, are one subject |
| `domain/bridging.rs` | `gap_bridging.rs` | Matches the operator-facing config knobs `bridge_gaps` / `gap_threshold_secs` |
| `domain/ports.rs` (`Camera`) | `camera.rs` | What a camera does belongs with the camera |
| `domain/fake.rs` | `fake_camera.rs` | It is a fake camera |
| `bc_adapter.rs` | `bc_camera.rs` | It is a camera, spoken to over Baichuan |
| `mqtt_sink.rs` | `mqtt_status.rs` | It reports status; "sink" named the pattern, not the job |
| `StatusSink::publish` | `StatusReporter::report` | Same reason |

## Deferred, deliberately

Two things inside the executed phases were left as they were, with reasons:

- **`trait Camera` still names some `bairelay_neolink_core` types** (`RfAlarmCfg`, `LedState`,
  `VersionInfo`, `UserList`, `AbilityInfo`, `MotionData`, `Direction`, `LightState`, and the core
  `Error`). Giving each a local twin would be field-for-field mapping with no second implementation
  to justify it — the over-structure signal in `rust-code-structure.md` § Choosing a structure. The
  trait is consumer-defined either way, which is the property `TR-2` is about. Revisit when a
  second camera backend exists.
- **`crates/core`'s `talk` module uses a blocking `crossbeam_channel::recv()` inside
  `BufferedStream::fill_buf`, reachable from the `async fn talk_stream`** — a real `AS-5`
  violation. It is unreachable from the shipped binary (nothing in `src/` calls it), and fixing it
  means reworking the `Read`/`BufRead` impls with no way to verify two-way audio without hardware.
  Recorded rather than blind-rewritten.

Phases 1–3 delivered ~80 % of the value as modules (growth-path stage 1), which is where the
boundary stayed. The original plan was to promote them to a crate once their contents stopped
moving; in the event the promotion was tried and reverted, because the separation was already
doing its work and the crate added a published artifact nobody consumes. See § Why no domain
crate.

---

## Calibration — what this buys, honestly

Applying our own doc's over-structure test (`rust-code-structure.md` § choosing a structure):

**Clearly pays.** Phase 1 (port direction + newtypes: removes a live leak, deletes the dual-handle
hazard, fixes the two documented unit-confusion traps — millivolts, zoom ×1000). Phase 2 (the
highest-complexity logic becomes property-testable and replayable; this is where the product's
user-visible bugs live). Phase 3 (event routing testable as values; kills the supervisor/MQTT
ordering subtlety as a distributed concern).

**Judged not worth it, after trying it.** Phase 4 — the domain crate's pitch was *enforcement*
(a `tokio` import in policy code becomes a build error instead of a review catch) plus faster
incremental builds. Built and then removed: the wall could only stand in front of the modules
already least at risk, since the `Camera` trait's signatures keep it outside, and the cost was a
publishable crate with no external consumer. § Why no domain crate has the full reasoning. The
enforcement argument would return if a second binary or an external consumer ever needed the
policy code.

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
