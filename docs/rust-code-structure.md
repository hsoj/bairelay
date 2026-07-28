# Structuring Rust Codebases

A repo-independent reference for how to *organize* Rust code: modules, crates, workspaces, and
the architectural styles that decide where things live — layered, hexagonal (ports & adapters),
domain-driven design, vertical slices, and sans-IO. Companion to `rust-practices.md` (language-level
rules, cited here by ID as `TY-1`, `TR-2`, …); this document is self-contained if read alone.

Written for reuse by future agents: every section leads with the decision it settles, and § Choosing
a structure gives the calibration table. Nothing here refers to any particular repository.

---

## The one invariant: dependencies point inward

Every structure described below is a different way of enforcing a single rule:

> **Code that decides depends on nothing; code that talks to the world depends on the code that
> decides. Never the reverse.**

"Decides" means domain rules, protocol state machines, orchestration logic. "Talks to the world"
means HTTP, databases, message brokers, files, clocks, other processes. The inner code defines
traits describing what it needs (`TR-2`); the outer code implements them. If the inner code's
`Cargo.toml` (or module imports) name a database driver, an HTTP client, or a broker SDK, the
structure has failed regardless of how the directories are named.

Why this is *the* rule:

- **Testability** — inner logic runs against in-memory fakes with no infrastructure standing up.
- **Deleteability** — adapters can be replaced (Postgres → SQLite, HTTP → gRPC) without touching
  the logic they serve.
- **Compile-time enforcement** — in Rust the rule can be made mechanical: put the layers in
  separate crates and an inward-pointing violation is a dependency cycle that will not build.
  This is Rust's structural superpower over most languages, where layering is convention only.

Everything else — hexagonal's zones, DDD's bounded contexts, clean architecture's circles — is
vocabulary and refinement layered on this invariant. If a structure discussion loses sight of it,
the discussion is about aesthetics.

A mechanical check worth automating (CI or review): the domain/core crate's dependency list should
contain approximately nothing — `thiserror`, maybe `serde`, maybe a time or decimal type. Each
addition to it is an architectural decision, not a convenience.

---

## The growth path: module → split → workspace

Rust gives three escalating tools for separation. Use the smallest one that enforces the boundary
you need, and graduate deliberately.

### Stage 1 — modules within one crate

The default. Boundaries are visibility (`pub`, `pub(crate)`, `pub(super)`), reviewed by humans.

```
src/
  main.rs          // wiring only
  config.rs
  domain/          // pure logic; imports nothing from siblings
    mod.rs
    order.rs
    pricing.rs
  store.rs         // implements traits that domain/ defines
  http.rs          // calls into domain/
```

Enforcement is soft — nothing stops `domain/pricing.rs` from importing `crate::store` except
review. Mitigations: keep the domain module's imports auditable (one `use` block at the top of
`mod.rs` re-exporting what the module offers), and treat any `use crate::{adapter}` inside the
domain module as a defect.

Stay here while: one team, one deliverable, boundaries still shifting. Premature crate splits fossilize
boundaries you haven't found yet.

### Stage 2 — binary + library split

```
src/lib.rs         // everything, publicly reachable by integration tests
src/main.rs        // thin: parse args, build config, call lib
tests/             // drive the public API as an external consumer
```

Do this early and always for binaries. The payoff is integration tests: `tests/*.rs` link the
library as an external crate, which both enables black-box testing and forces the public API to be
usable. A `main.rs` containing logic is untestable by construction.

### Stage 3 — workspace of crates

Boundaries become compile-time facts. Graduate a module to a crate when any of these is true:

- The boundary is **architecturally load-bearing** — domain vs. infrastructure, protocol vs.
  transport — and you want violations to be build errors, not review comments.
- A component has a genuinely **different consumer** (another binary, an external user, a fuzzer).
- **Build time**: the component is large and stable; a crate boundary makes it an independently
  cached compilation unit.
- The component needs **different dependencies or features** than the rest (e.g. a `no_std`
  protocol core).

Workspace mechanics that are best practice regardless of architecture:

- One `[workspace.dependencies]` table; members use `dep.workspace = true`. Shared `edition`,
  `rust-version`, `license`, lints via `[workspace.package]` / `[workspace.lints]` (`DP-2`).
- Flat crate layout (`crates/foo`, `crates/bar`), not nested trees mirroring the dependency graph —
  the graph will change; the directory tree shouldn't have to.
- Name crates for their responsibility, prefixed for namespace (`myapp-core`, `myapp-http`), and
  keep each crate describable in one sentence.
- One `Cargo.lock` at the root, committed for binaries, CI running `--locked` (`DP-4`).
- Prefer *more, smaller* crates over fewer, larger ones once you're here at all — smaller crates
  compile in parallel, expose smaller public surfaces, and make ownership legible.

**The mapping to architecture:** stages don't change the architecture, only its enforcement.
A well-run stage-1 codebase and a workspace can have identical structure; the workspace makes the
structure a fact instead of an intention.

---

## The structural styles, compared

Five named styles cover the practical space. They are not rivals so much as answers to different
questions.

| Style | Organizing question | First-class unit | Rust fit |
|-------|--------------------|--------------------|----------|
| **Layered (n-tier)** | "What technical kind of code is this?" | Layer (handlers / services / data) | Weak — layers invite wide, shallow modules and give the compiler nothing to enforce |
| **Hexagonal / ports & adapters** | "Is this decision or is this I/O?" | The core, plus adapters around it | Strong — ports are traits, adapters are impls, the boundary is a crate edge |
| **Clean / onion architecture** | Same as hexagonal, with more prescribed rings | Concentric layers | Same core idea; the extra rings (use-case layer, interface adapters) are ceremony unless the app is large |
| **Vertical slices** | "What feature does this serve?" | Feature (all its handlers/logic/storage together) | Good *within* a boundary — Rust modules per feature; combine with hexagonal, below |
| **Sans-IO** | "Can this logic run without performing I/O?" | Pure state machine + thin I/O driver | Excellent — the same inward-dependency rule applied at protocol level; ideal for codecs and wire protocols |

Working positions, defensible from the sources and from Rust's mechanics:

1. **Pure layered architecture is the weakest choice in Rust.** Its layers are technical, not
   semantic, so cohesion is low (every feature smears across all layers) and nothing about the
   layering is compiler-checkable. It survives as the *internal* layering of a single slice.
2. **Hexagonal is the default skeleton** the moment a program has both nontrivial logic and real
   I/O. In Rust it costs almost nothing: the ports you'd want for testing (`TS-1`) are the same
   traits the architecture asks for.
3. **Vertical slices answer a different question than hexagonal** — slices decide *how to group
   features*, hexagonal decides *how each feature relates to I/O*. The strong combination:
   slice the domain by feature/context, keep the hexagonal core/adapter split inside or across
   slices. A slice is a module or crate; its internal layering is its own business.
4. **Sans-IO is hexagonal for protocols.** When the "domain" is a wire protocol, codec, or
   state machine, make it a pure library (bytes/events in → decisions/bytes out, no sockets, no
   runtime, no clock — time arrives as a parameter). The transport becomes a trivially thin driver
   loop, and the core becomes fuzzable, replayable, and runtime-agnostic.
5. **Clean architecture's extra rings are opt-in.** Add an explicit application/use-case layer
   only when orchestration logic (spanning multiple aggregates/ports) grows too large to live in
   the domain services — not on day one.

---

## Hexagonal architecture in Rust, concretely

### The three zones

```
        ┌───────────────── adapters ──────────────────┐
        │   inbound: HTTP, gRPC, CLI, queue consumer  │  drive the app
        │  ┌────────────── core ────────────────┐     │
        │  │  domain model: entities, values,   │     │
        │  │  invariants (TY-1..TY-3)           │     │
        │  │  ports: traits the core defines    │     │
        │  │  services: the use-case API        │     │
        │  └────────────────────────────────────┘     │
        │   outbound: database, mail, clock, metrics  │  driven by the app
        └─────────────────────────────────────────────┘
                 main.rs = composition root
```

- **Inbound (driving) ports** — the core's public API: usually a service trait, or just the
  service type's inherent methods. Inbound adapters (HTTP handlers, CLI commands) *call* it.
- **Outbound (driven) ports** — traits the core *defines* for what it needs
  (`trait OrderRepository`, `trait Mailer`, `trait Clock`). Outbound adapters *implement* them.
- **Composition root** — `main.rs` (or a `bootstrap` module) is the only place that names both a
  port and its concrete adapter, constructs everything, and wires it together. Nothing else may.

### Canonical single-crate layout

```
src/
  main.rs                    // composition root, nothing else
  domain/
    order/
      mod.rs                 // models: Order, OrderId, validated newtypes
      service.rs             // OrderService: use-case methods
      ports.rs               // OrderRepository, PaymentGateway, Mailer traits
      error.rs               // CreateOrderError, ... (thiserror enums)
  inbound/
    http/                    // axum/actix handlers → call OrderService
    cli/
  outbound/
    postgres.rs              // impl OrderRepository for Postgres
    smtp.rs                  // impl Mailer for SmtpMailer
    clock.rs                 // impl Clock for SystemClock
```

Workspace variant: `crates/core` (domain + ports + services), `crates/http`, `crates/postgres`,
binary crate with only the composition root. Adapters depend on core; core depends on nothing.

### Ports as traits: the load-bearing details

```rust
// Defined in the core, next to the service that consumes it (TR-2).
pub trait OrderRepository: Send + Sync + 'static {
    fn insert(&self, order: &Order)
        -> impl Future<Output = Result<(), InsertOrderError>> + Send;
    fn find(&self, id: OrderId)
        -> impl Future<Output = Result<Option<Order>, RepoError>> + Send;
}
```

- **`Send + Sync + 'static` bounds on port traits** — required for the trait to be usable behind
  shared handles and inside multithreaded runtimes and web frameworks. Add them at the trait, not
  ad hoc at every use site.
- **Async methods on dyn-dispatched ports** need care: `async fn` in traits doesn't produce `Send`
  futures you can name for `dyn`. Either return `impl Future<…> + Send` (as above, fine for
  generics), or use `#[trait_variant]`/boxed futures when the port must be object-safe (`TR-4`).
- **Port methods speak domain types only.** Parameters and returns are domain models and domain
  errors — never `sqlx::Row`, never `http::StatusCode`, never a driver's error type. Conversion
  happens inside the adapter.
- **Keep ports role-shaped and narrow** (`TR-3`): `OrderRepository`, `PaymentGateway`, `Clock` —
  not one `Infrastructure` trait. A fake for a test should be a page, not a stub farm.
- **A clock port is not over-engineering.** Time is I/O. Any core logic that branches on "now"
  gets a `Clock` port (or takes `now: Instant` as a parameter, the sans-IO move), because that is
  the difference between deterministic tests and flaky ones (`TS-6`).

### Wiring: generics vs. trait objects

Two idioms, both correct, chosen by trade-off (`TR-1`):

| | Generic (`Service<R: OrderRepository>`) | Dynamic (`Arc<dyn OrderRepository>`) |
|---|---|---|
| Dispatch | Static, zero-cost, inlinable | Vtable |
| Type inference/errors | Types propagate everywhere; error messages grow | Contained |
| Swapping at runtime / config-chosen adapter | No (one type per instantiation) | Yes |
| Binary size / compile time | Monomorphization cost | Flat |
| Signature noise | `AppState<R, M, C>` spreads through the app | `AppState` stays simple |

Default: **generics for the service's own dependencies, instantiated once in the composition
root**. The monomorphization happens exactly once (one concrete repo, one concrete mailer), so the
cost is nil and you keep static dispatch. Switch a given port to `dyn` when the adapter is chosen
at runtime (config-selected backend, plugins) or when three-plus generic parameters start
infecting every signature that holds the service. Mixing is fine — this is per-port, not global.

Either way, the **service type is `Clone`** with its state behind `Arc` internally (`OW-3`), so
handlers hold it by value:

```rust
#[derive(Clone)]
pub struct OrderService<R, P> {
    repo: Arc<R>,
    payments: Arc<P>,
}
```

### What adapters do — and the wrapping rule

An adapter's whole job is translation: wire/vendor shapes ↔ domain shapes, vendor errors ↔ domain
errors, transport concerns (status codes, retries, connection pools) kept entirely on its side.

**Wrap third-party crates in your own types at the adapter boundary.** `pub struct Postgres(sqlx::PgPool)`
rather than passing `PgPool` around. The wrapper exposes only what the app needs, gives migrations
off that dependency a single point of impact, and keeps vendor types out of every signature. The
same rule at the inbound edge: request/response DTOs (with their `serde` derives and API-versioning
concerns) are defined in the adapter and converted to domain commands via `TryFrom` — the domain
model must be free to diverge from the API's shape, because it will.

---

## Domain-driven design in Rust

DDD supplies the vocabulary for *what goes inside the hexagon* and *how many hexagons there are*.

### Strategic DDD → workspace structure

- **Bounded context**: a boundary within which a model and its language are consistent. In Rust,
  a bounded context maps naturally to **a crate** (or a top-level module at stage 1) — its public
  API is the context's contract, and the compiler enforces that other contexts use only that.
- **Same term, different contexts, different types — on purpose.** `catalog::Book` (title,
  description, cover) and `lending::Book` (due dates, borrower) should be *distinct types*, not
  one struct serving both masters with a growing field set. A struct accreting fields used by
  disjoint call sites is the classic sign of a missed context boundary.
- **Cross-context communication** goes through explicit translation — each context converts the
  other's published types into its own at the border (an anti-corruption layer, which in Rust is
  just a module of `From`/`TryFrom` impls) — or through events. Never by sharing mutable state or
  reaching into another context's internals.
- **Cross-context operations are non-atomic by nature.** If two contexts seem to need a shared
  transaction, the boundary is drawn wrong — either merge them or redesign the interaction as an
  event flow with eventual consistency.
- **Start with fewer, larger contexts** and split when friction appears (a model that means two
  things, teams stepping on each other). Splitting a crate is cheap; unsplitting a wrong boundary
  after both sides have grown APIs is not. The same applies tenfold to services: do not turn
  context boundaries into network boundaries until they've been stable in-process.
- **Ubiquitous language lives in identifiers.** Types, methods, and events use the domain's own
  vocabulary (`Order::place`, `Loan::overdue`, `InvoiceIssued`) — not tech-speak
  (`OrderManager::process`, `handle_data`). If domain experts wouldn't recognize the name, rename
  it (`NM-4`).

### Tactical DDD → Rust constructs

The tactical patterns map onto Rust unusually well because ownership *is* the aggregate boundary:

| DDD concept | Rust realization | Key point |
|---|---|---|
| Value object | Newtype / small immutable struct, `PartialEq` by value (`TY-2`) | The natural Rust default |
| Entity | Struct with an ID newtype; equality/hash by ID only | Don't derive `PartialEq` — it would compare state, not identity |
| Aggregate | One root struct **owning** its children as private fields; all mutation via `&mut self` methods on the root | See below — this is the strongest fit |
| Repository | Outbound port trait, per aggregate root, speaking whole aggregates | Load/store roots, not fragments |
| Domain service | Free function or stateless struct over domain types | For operations owned by no single entity |
| Domain event | Enum variant **returned** from aggregate methods | See below |
| Factory | Constructor/builder returning `Result` (`TY-3`) | No invalid aggregate ever exists |
| Application service | The hexagonal `Service`: orchestrates aggregates + ports per use case | Thin; no business rules of its own |

**Aggregates and ownership.** DDD says an aggregate is modified only as a whole, through its root,
which enforces all invariants. In most languages that's a convention; in Rust it's the type system:
children are private fields (not `Arc`s handed out), so *there is no way* to mutate a child except
through a `&mut self` method on the root, and the borrow checker guarantees no aliased writer
exists. Handing out `Arc<Mutex<Child>>` doesn't just risk the invariant — it deletes the aggregate
boundary (`OW-5`). The aggregate is also the transaction boundary: one repository call persists
one root, atomically.

**Events as return values.** The idiomatic Rust move — which also solves a borrow-checker
friction — is for aggregate commands to *return* events rather than publish them:

```rust
impl Order {
    pub fn ship(&mut self, at: Timestamp) -> Result<OrderShipped, ShipError> {
        // check invariants, mutate self, describe what happened
    }
}
```

The aggregate stays pure (no bus handle inside domain types, no I/O in the core), the application
service persists the aggregate and publishes the returned event, and tests assert on returned
events — plain values — instead of observing side effects. This composes directly with event
sourcing (state = fold of events) where that's warranted, but is worth doing even without it.

**Repositories and transactions.** The repository trait speaks aggregates; the *adapter* owns
transaction mechanics internally. When a use case genuinely spans multiple repository calls
atomically, prefer widening the repository method (one call, one transaction, adapter-internal)
over exposing a transaction object through the port — a `Transaction` in the port's vocabulary
drags persistence semantics into the core. If it truly can't be avoided, the escape hatch is a
`UnitOfWork` port; treat needing it as a hint that an aggregate boundary is misplaced.

**What to skip.** For a program with little business logic — a translator, a proxy, a collector —
tactical DDD is scaffolding with nothing to hold up. Take the permanently useful parts (value
objects, anti-corruption at every external representation) and skip aggregates/repositories until
there are invariants worth guarding.

---

## Vertical slices, in Rust terms

Organize by feature, not by technical kind — `src/features/place_order/` containing its handler,
its logic, its storage access — maximizing cohesion inside a slice and minimizing coupling between
slices.

What survives translation to Rust:

- **Slice = module (or crate) per feature/context.** Each slice exposes a deliberately tiny public
  API; slices don't reach into each other's internals. This is just bounded contexts at finer
  grain, and Rust's visibility system is built for it.
- **Each slice chooses its own internal complexity.** A trivial CRUD slice can be one flat module;
  a complex slice gets the full core/ports/adapters treatment. No global mandate that every
  feature carry every layer.
- **Shared code is promoted reluctantly.** A `shared`/`common` module grows without bound
  (`NM-4`); prefer duplicating two similar lines across slices until the third occurrence proves
  the abstraction (the DRY threshold), then promote to a named crate with a real responsibility.

What does *not* survive: taking "slices own their data access" to mean domain logic may import the
database driver. Inside a slice the inward-dependency rule still holds — a slice is a small
hexagon, not an exemption from one.

---

## Testing follows structure

The point of all of the above is that each zone gets a distinct, cheap testing strategy (`TS-1`):

| Zone | Test style | Doubles needed |
|---|---|---|
| Domain model (entities, values) | Plain unit tests, no async, no doubles | None — it's pure |
| Sans-IO protocol core | Fixture replay, property tests, fuzzing (`TS-5`) | None — bytes in, decisions out |
| Application services | Unit tests with in-memory fakes of outbound ports | One small fake per port |
| Inbound adapters | Handler tests against a fake *service* (the inbound port) | Fake service |
| Outbound adapters | Narrow integration tests against the real dependency (testcontainer DB, local broker) | None — the real thing, few tests |
| Whole system | A handful of end-to-end smoke tests through the composition root | None |

Consequences worth stating explicitly:

- Fakes implement **ports**, not vendors: an `InMemoryOrderRepository` over a `HashMap`, not a mock
  of a database driver. If a port is annoying to fake, the port is too wide (`TR-3`).
- Keep fakes behind a `test-util` feature or in a test-support module so production code cannot
  link them (`TS-2`).
- The pyramid shape falls out for free: thousands of pure-core tests (microseconds each), dozens
  of adapter integration tests, a few E2E. If the ratio is inverted, structure — not testing
  discipline — is usually the cause: logic has leaked into adapters where only expensive tests
  can reach it.

---

## Choosing a structure: calibration

Architecture should remove pain, not add ceremony. The failure modes are symmetric — a 500-line
tool wearing a 12-crate hexagon, and a 50k-line service where the SQL lives in the HTTP handlers.

| Project | Structure |
|---|---|
| Script/one-shot tool (< ~1 kloc) | Single crate, `lib.rs` + thin `main.rs`, modules by topic. No ports. Stop there. |
| CLI or small service with real logic | Stage 1–2 hexagonal-lite: a pure `domain`/`core` module, ports for the 2–3 real externals (store, clock, network), adapters as sibling modules. |
| Long-lived service, multiple adapters | Full hexagonal, probably a workspace: `core` crate + adapter crates + thin binary. DDD tactical patterns where invariants exist. |
| Multiple subdomains / teams | Bounded contexts as crates (or crate groups); vertical slicing between them; hexagonal within; events between contexts. |
| Protocol / codec / embedded logic | Sans-IO core crate (`no_std`-capable if useful) + separate driver crate(s) per transport/runtime. |

Signals you're **under**-structured: logic reachable only through expensive integration tests;
a change to the storage layer touching business rules; vendor types appearing three imports deep;
"we can't test that without a real X".

Signals you're **over**-structured: ports with exactly one implementor *and* no test double
(`TR-1`); traits and layers you traverse but never vary; DTO↔domain mappings that are pure
field-for-field copies for every type; more wiring code than logic. Collapse layers that aren't
paying rent — the growth path runs in both directions.

**On day one, choose the smallest structure with a pure core.** The single decision that's
expensive to retrofit is letting I/O entangle the logic; everything else (crate splits, dyn vs.
generic, slice grouping) refactors mechanically later.

---

## Agent checklist

When creating or reviewing structure, verify:

- [ ] Core/domain code imports no I/O, framework, or vendor crates — check the actual `use` lines
      / `Cargo.toml`, not the directory names.
- [ ] Every outbound dependency of the core is a trait the core defines, with `Send + Sync`
      bounds, speaking domain types only.
- [ ] Exactly one composition root names concrete adapters; nothing else does.
- [ ] Adapters translate at both edges: DTOs/vendor types never cross into the core; core errors
      never leak vendor errors (wrap or map them).
- [ ] Aggregates own their children; no `Arc<Mutex<child>>` escaping a root; equality of entities
      is by ID.
- [ ] Time, randomness, and IDs used by core logic arrive via a port or parameter, not
      `SystemTime::now()` / `rand::random()` inline.
- [ ] Domain events are returned from aggregate methods, published by the application layer.
- [ ] Each port has ≥2 implementations (real + fake) or a written reason it exists anyway.
- [ ] Test cost matches zone: core tests need no doubles or infra; only adapter tests touch the
      real dependency.
- [ ] The structure is the smallest that keeps the core pure — no traversal-only layers, no
      field-copy mapping ceremonies, no `shared` module accreting orphans.

---

## Sources

- [Master Hexagonal Architecture in Rust (howtocodeit)](https://www.howtocodeit.com/guides/master-hexagonal-architecture-in-rust) —
  the most thorough Rust-specific treatment: ports as traits, generic services, model separation,
  wrapping third-party crates, domain boundaries and events.
- [How to apply hexagonal architecture to Rust (Barrage)](https://www.barrage.net/blog/technology/how-to-apply-hexagonal-architecture-to-rust)
  and [Hexagonal Architecture in Rust (Cogs and Levers)](http://tuttlem.github.io/2025/08/31/hexagonal-architecture-in-rust.html).
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html) and
  [Microsoft Pragmatic Rust Guidelines](https://microsoft.github.io/rust-guidelines/guidelines/checklist/index.html) —
  crate/workspace/module-level rules referenced throughout.
- [Cargo Workspace Best Practices for Large Rust Projects (Reintech)](https://reintech.io/blog/cargo-workspace-best-practices-large-rust-projects)
  and [The best way to structure Rust web services (LogRocket)](https://blog.logrocket.com/best-way-structure-rust-web-services/).
- [Vertical Slice Architecture (Jimmy Bogard)](https://www.jimmybogard.com/vertical-slice-architecture/),
  [Vertical Slice Architecture (Milan Jovanović)](https://milanjovanovic.tech/blog/vertical-slice-architecture),
  and [N-Layered vs Clean vs Vertical Slice Architecture](https://antondevtips.com/blog/n-layered-vs-clean-vs-vertical-slice-architecture).
- [DDD, CQRS and Event Sourcing using Rust (cqrs-es book)](https://doc.rust-cqrs.org/theory_ddd.html)
  and [Event Sourcing with Aggregates in Rust (Kevin Hoffman)](https://medium.com/capital-one-tech/event-sourcing-with-aggregates-in-rust-4022af41cf67) —
  aggregates-as-ownership and events-as-return-values.
