# Rust Practices & Design Patterns

A standing reference for how Rust is written here — language-level best practice plus the
architectural patterns those practices imply. Derived from the upstream Rust API Guidelines,
Microsoft's Pragmatic Rust Guidelines, and the design-pattern literature (DDD, hexagonal /
ports-and-adapters, composition-over-inheritance), reconciled into one document.

This document describes the **target state**, independent of any code currently in the tree.
Where existing code disagrees, this document wins for *new* work; see § Applying to existing code.

---

## How to use this document

Every rule has a stable ID (`TY-1`, `ER-4`, …) and a strength marker:

| Marker | Meaning |
|--------|---------|
| **MUST** | Violations are defects. Fix them or don't merge. A reviewer should block on these. |
| **SHOULD** | Correct by default. Deviating is allowed but requires a stated reason in the PR or a code comment. |
| **CONSIDER** | A prompt to think, not a requirement. No justification needed either way. |

For agents:

- Read § Quick decision tables and § Review checklist before writing code; read the relevant
  section in full before designing a new module, trait, or crate boundary.
- Cite rule IDs when explaining a design choice (`"boxed here per OW-3"`). This makes review
  arguments checkable instead of stylistic.
- **Precedence**: `CLAUDE.md` (repo-specific conventions) > this document > upstream guidelines >
  personal preference. When `CLAUDE.md` and this document conflict, follow `CLAUDE.md` and flag
  the conflict.
- Rules are not a licence to churn. Applying a rule to code you are not otherwise touching is
  out of scope unless asked.

---

# Part I — Language practice

## 1. Type-driven design (`TY-`)

The central discipline. Most other rules are corollaries.

### `TY-1` **MUST** make illegal states unrepresentable

Push validity into the type, so the compiler enforces it and no runtime check is needed downstream.

```rust
// No: four independent fields, 2^4 combinations, most nonsense.
struct Connection {
    socket: Option<TcpStream>,
    session_id: Option<u32>,
    authenticated: bool,
    closed: bool,
}

// Yes: three states, all valid, exhaustively matchable.
enum Connection {
    Disconnected,
    Connected { socket: TcpStream },
    Authenticated { socket: TcpStream, session_id: SessionId },
}
```

The test: count the representable values and ask how many are meaningful. A wide gap means the
type is wrong, and every consumer will pay for it with defensive checks.

### `TY-2` **MUST** use newtypes for domain values, not primitives

`u64` is not a user id; `String` is not an email address. A newtype costs nothing at runtime and
buys type-checked argument order, a place to hang validation and `Display`, and a name that shows
up in errors and docs.

```rust
pub struct UserId(u64);
pub struct Percentage(u8);

impl Percentage {
    pub fn new(raw: u8) -> Result<Self, OutOfRange> { … }
    pub fn get(self) -> u8 { self.0 }
}
```

Rule of thumb: if two parameters of the same primitive type sit next to each other in a signature,
at least one of them wants a newtype. Swapping them must not compile.

### `TY-3` **MUST** have newtypes guard their invariant

A newtype whose field is `pub`, or that has a `From<Raw>` that can't fail, guarantees nothing.
Construction goes through a fallible constructor; the field stays private; there is no way to build
an invalid instance from outside the module. If the invariant is genuinely "any value is fine", the
newtype is for naming only — say so in the doc comment so nobody adds validation later and breaks
callers.

### `TY-4` **MUST NOT** use `bool` or bare `Option` as a parameter that conveys meaning

`open(path, true, false)` is unreadable and unauditable. Use a two-variant enum or a newtype.

```rust
// No
fn open_file(&self, path: &Path, append: bool, create: bool);

// Yes
fn open_file(&self, path: &Path, mode: OpenMode);
```

This applies to return types too: `Option<T>` as a return is fine when "absent" is the whole story,
but if absence has a *reason*, return `Result<T, E>` or a purpose-built enum.

### `TY-5` **SHOULD** prefer enums over trait objects for closed sets

If you know every variant at compile time and the set changes rarely, an enum is better than
`Box<dyn Trait>`: exhaustive matching, no allocation, no dispatch, `Clone`/`PartialEq`/`Debug` derive
for free, and adding a variant produces compile errors at exactly the places that need updating —
which is a feature, not a cost. Reach for `dyn` when the set is genuinely open (plugins, downstream
implementors, test doubles).

### `TY-6` **SHOULD** use the strongest available type family

`Duration` not `u64` seconds. `Path`/`PathBuf` not `String`. `NonZeroU32` where zero is invalid.
`SocketAddr` not `(String, u16)`. `IpAddr` not `u32`. Each one deletes a class of bug and a class
of conversion code.

### `TY-7` **CONSIDER** encoding state transitions in types (typestate)

When an object has a lifecycle and operations are only legal in some states, make each state a
distinct type and let transitions consume `self`. See § Typestate for the full treatment.

### `TY-8` **SHOULD** mark public enums and structs `#[non_exhaustive]` when variants/fields may grow

Applies to library crates with external consumers. Within a workspace where all consumers are in
tree, `#[non_exhaustive]` is usually noise — you *want* the compile errors.

---

## 2. Ownership & API shape (`OW-`)

### `OW-1` **MUST** accept the least restrictive type that does the job

Take `&str` over `&String`, `&[T]` over `&Vec<T>`, `impl AsRef<Path>` over `&Path` where callers
plausibly hold strings. Prefer concrete return types, owned or borrowed as the semantics demand.
The general shape: be liberal in what you accept, precise in what you return.

```rust
// No
fn load(path: &PathBuf, tags: &Vec<String>) -> Box<dyn Iterator<Item = String>>;

// Yes
fn load(path: impl AsRef<Path>, tags: &[String]) -> Vec<String>;
```

Don't over-apply: `impl AsRef<_>` on a hot inner function is generic bloat. It pays at the crate's
public edge, not in every private helper.

### `OW-2` **MUST** let the caller decide where data lives

Don't allocate on the caller's behalf when a borrow would do, and don't force a clone by taking
`&T` when you're going to clone it anyway — take `T` and let the caller decide whether to clone.
Taking by value is a documented statement that you need ownership.

### `OW-3` **SHOULD NOT** put smart pointers in public signatures

`Arc<Mutex<Config>>` in a parameter forces every caller into that exact representation forever.
Take `&Config`; let the caller lock. Expose `Arc` only when shared ownership is genuinely part of
the contract (a handle type that is *defined* as cheap-to-clone shared state).

Corollary: a "service" or "handle" type SHOULD be `Clone` and hold its `Arc` internally, so
consumers pass it around by value without seeing the pointer:

```rust
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,   // private
}
```

### `OW-4` **MUST NOT** use out-parameters

`fn compute(&self, out: &mut Vec<u8>)` is a C idiom. Return the value. The exception is a
documented buffer-reuse API (`read_into`), where the whole point is avoiding allocation — and then
the name says so.

### `OW-5` **SHOULD** avoid `Rc<RefCell<T>>` / `Arc<Mutex<T>>` as a default

These are tools for genuine shared mutation, not a workaround for a borrow-checker complaint. A
borrow error usually means the ownership model is wrong. Before reaching for interior mutability,
try: restructuring so one owner holds the data, passing `&mut` down the call stack, splitting the
struct so disjoint fields borrow independently, or message-passing (§ Actor).

When shared mutation *is* correct: keep the critical section small, never hold a lock across
`.await` (`AS-4`), and prefer `RwLock` only when reads genuinely dominate and are non-trivial —
otherwise `Mutex` is faster and simpler.

### `OW-6` **SHOULD** prefer borrowed iteration over collecting

`impl Iterator<Item = &T>` beats `Vec<T>` when the caller may only need the first item or a count.
But don't contort a signature into an iterator that immediately gets collected by every caller —
`C-INTERMEDIATE`'s point is exposing intermediate results, not maximising laziness.

### `OW-7` **CONSIDER** boxed slices and boxed strs for immutable owned sequences

`Box<[T]>` / `Box<str>` over `Vec<T>` / `String` for data that is built once and never grows: saves
a word per value and documents immutability. Worth it in hot structs and long-lived collections;
not worth the friction in ordinary code.

---

## 3. Naming & conventions (`NM-`)

### `NM-1` **MUST** follow RFC 430 casing

`UpperCamelCase` types/traits/enum variants, `snake_case` functions/methods/modules/locals,
`SCREAMING_SNAKE_CASE` consts/statics. Acronyms are words: `HttpClient`, `Uuid`, `parse_tls_header` —
never `HTTPClient` or `parseTLSHeader`.

### `NM-2` **MUST** follow the `as_` / `to_` / `into_` cost convention

| Prefix | Cost | Ownership |
|--------|------|-----------|
| `as_` | Free | borrowed → borrowed |
| `to_` | Expensive (allocates/copies) | borrowed → owned |
| `into_` | Variable, but consuming | owned → owned |

A `to_` that's free or an `as_` that allocates will mislead every reader. Getters are `fn name()`,
not `fn get_name()`; the `get_` prefix is reserved for when there is a single obvious thing that
could be gotten (`Cell::get`).

### `NM-3` **MUST** use consistent word order across the crate

Pick verb-object or object-verb and hold it. `fetch_user` / `fetch_order` / `fetch_report`, not
`fetch_user` / `order_fetch` / `do_report`. Applies to error type names too: pick `ParseError` or
`ErrorParse` and never mix.

### `NM-4` **SHOULD** keep names short and free of weasel words

`Manager`, `Helper`, `Util`, `Handler`, `Service`, `Data`, `Info`, `Object` carry no information.
`ConnectionManager` that owns a pool is a `ConnectionPool`. A module named `utils` is an admission
that the code has no home; find it one. If a name needs a qualifier because two things clash,
the module path usually already supplies it — `bc::Header`, not `bc::BcHeader`.

### `NM-5` **SHOULD** name iterator types after their producing method

`iter()` → `Iter`, `iter_mut()` → `IterMut`, `into_iter()` → `IntoIter`, `drain()` → `Drain`.

### `NM-6` **MUST** name constants for the reason, not the value

`RETRY_BACKOFF_MAX` not `SIXTY_SECONDS`. Every magic value gets a named constant *and* a comment
explaining where the number came from — a spec section, a measured latency, a vendor quirk.

---

## 4. Errors, panics & failure (`ER-`)

### `ER-1` **MUST** use typed errors in libraries, `anyhow`-style context in binaries

Library crates define `thiserror` enums so callers can match and recover. The binary uses `anyhow`
(or equivalent) because its only recovery is "log it and decide the exit code". Never make a
library consumer parse a string to find out what happened.

```rust
// library crate
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("no record found for id {id}")]
    NotFound { id: String },
    #[error("credentials rejected")]
    Auth,
    #[error("transport failure")]
    Io(#[from] std::io::Error),
}
```

### `ER-2` **MUST** make error types well-behaved

Public errors implement `Debug` + `Display` + `std::error::Error`, are `Send + Sync + 'static`
(so they compose with `anyhow` and cross thread boundaries), and chain causes via `#[source]` /
`#[from]`. `Display` messages are lowercase, no trailing period, and describe *what failed* —
context is added by the caller, not baked into the leaf.

### `ER-3` **SHOULD** prefer `From` conversions over `map_err` chains

If `?` needs a `.map_err(…)` at every call site, the conversion belongs on the error type. Reserve
`map_err` for adding call-site-specific context that a blanket `From` couldn't know.

### `ER-4` **MUST** distinguish bugs from failures

| Situation | Mechanism |
|-----------|-----------|
| Environment misbehaved (I/O, network, bad input, missing file) | `Result` |
| Caller violated a documented contract | `panic!` / `assert!` / `debug_assert!` |
| Invariant this code is responsible for was broken | `panic!` — it's a bug, and it should stop |
| Unreachable by construction | `unreachable!("…why…")` |

Turning a bug into a `Result` pushes an impossible case onto every caller and hides the defect.
Turning a failure into a panic takes down a process over a dropped packet.

### `ER-5` **MUST NOT** `unwrap()` or `expect()` on anything that can fail at runtime

Permitted: statically-known-good values (a compiled regex literal, an in-bounds index just checked),
tests, and `main`-adjacent startup where failing fast is the intent. Every `expect` message states
the invariant that must hold — `.expect("config validated by check_config")` — not `"should work"`.

### `ER-6` **MUST** document panics and errors

Every public function that can panic has a `# Panics` section; every one returning `Result` has an
`# Errors` section describing the variants and when they occur. This is the contract; without it,
callers guess.

### `ER-7` **MUST NOT** fail in destructors

`Drop` cannot report failure and runs during unwinding. If cleanup can fail, provide an explicit
`close()`/`shutdown()` returning `Result`, and have `Drop` do a best-effort fallback that logs.
Same for blocking: a `Drop` that blocks (especially on async runtime shutdown) deadlocks; provide
an async alternative.

### `ER-8` **SHOULD** classify errors for retry at the point of definition

A caller deciding whether to retry shouldn't reason about variant names. Give the error type a
method: `fn is_retryable(&self) -> bool`, or better, split the enum so the distinction is
structural. Some classes are almost always terminal — rejected credentials, contract violations —
and blind retry just hammers the peer; make that classification part of the error's contract.

---

## 5. Traits & abstraction (`TR-`)

### `TR-1` **MUST** follow the dependency hierarchy: concrete type → generic → `dyn`

Reach for the least powerful mechanism that works.

| Mechanism | Use when | Cost |
|-----------|----------|------|
| Concrete type | One implementation, and you can name it | None. Best errors, best inlining. |
| `impl Trait` / generic `<T: Trait>` | Several implementations known at compile time | Monomorphisation (code size, compile time) |
| `Box<dyn Trait>` / `&dyn Trait` | Set is open, heterogeneous collection, or you need to break a compile-time dependency | Vtable indirection, object-safety constraints |

Adding a trait "for flexibility" with exactly one implementor and no test double is speculative
abstraction. Delete it and use the type.

### `TR-2` **MUST** define traits at the consumer, not the producer

This is dependency inversion and it's what makes hexagonal architecture work in Rust. The module
that *needs* a capability declares the trait describing its needs; the module that *provides* it
implements that trait. The consumer then depends on nothing but its own abstraction.

```rust
// orders/notify.rs — the order workflow declares what it needs
pub trait OrderNotifier: Send + Sync {
    fn order_shipped(&self, order: &Order) -> Result<(), NotifyError>;
}

// infrastructure crate implements it, knowing about both orders and SMTP
impl OrderNotifier for SmtpMailer { … }
```

The order workflow now knows nothing about SMTP, and the dependency arrow points inward. A trait
defined next to its only implementation, exported for others to consume, is usually the arrow
pointing the wrong way.

### `TR-3` **SHOULD** keep traits narrow and role-shaped

Interface segregation. A trait with fifteen methods forces every test double to stub fifteen
methods, and forces implementors to care about capabilities they don't have. Split by role:
`JobQueue`, `JobStatus`, `Scheduling` — not one `Backend` trait with everything. Compose at
the use site with `T: JobQueue + JobStatus` or by holding several handles.

Capabilities that are genuinely optional per-implementation are a signal to split, not to add
`fn supports_scheduling(&self) -> bool` plus methods that return `Err(Unsupported)`.

### `TR-4` **MUST** keep traits dyn-compatible if they might be used as trait objects

Generic methods, `Self: Sized` returns, and `async fn` (pre-`dyn`-compatible-async) break object
safety. If a trait is a seam that will hold test doubles or multiple runtime impls, keep it
object-safe: move generic methods to an extension trait with a blanket impl, or take `&dyn` /
`impl AsRef` parameters instead of generic ones.

### `TR-5` **SHOULD** seal traits that exist to model a closed set

If downstream implementations would break your invariants, seal the trait (private supertrait in a
private module). Sealing lets you add methods later without a breaking change. Don't seal traits
that exist as extension points — that's the opposite of the point.

### `TR-6` **MUST NOT** use `Deref` for inheritance

`Deref` is for smart pointers only. Using it to inherit methods from an inner type produces
confusing method resolution, breaks on generic contexts, and is the classic "faking inheritance in
Rust" anti-pattern. Write the delegating methods, or hold the inner value in a named field and let
callers reach it.

### `TR-7` **SHOULD** eagerly derive the common traits

`Debug` on every public type (this is effectively a MUST — `Debug` is what makes logs and test
failures readable). `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash`, `PartialOrd`, `Ord`, `Default`
wherever semantically correct. Deriving is nearly free and its absence blocks consumers in ways
they can't fix. Do not derive `Copy` on anything that might grow a heap field.

### `TR-8` **SHOULD** implement standard conversion traits rather than bespoke ones

`From`/`TryFrom` over `to_foo()`, `FromStr` over `parse_foo()`, `Display` over `to_string_pretty()`,
`FromIterator`/`Extend`/`IntoIterator` on collections. Standard traits mean your type composes with
generic code that has never heard of it.

### `TR-9` **SHOULD** put essential functionality in inherent methods

A type whose core operations are only reachable via a trait forces callers into an import dance
and shows up badly in docs and completion. Trait impls should delegate to inherent methods where
both exist.

---

## 6. Modules, crates & visibility (`MD-`)

### `MD-1` **MUST** default to private and widen deliberately

Every item starts private. `pub(crate)` for cross-module internals, `pub(super)` for a submodule's
parent, `pub` only for the actual API surface. A `pub` item is a promise; an accidental one is a
promise you didn't mean to make.

### `MD-2` **MUST** give struct fields private visibility with accessors where needed

Public fields freeze layout, prevent validation, and prevent adding invariants later. Exception:
plain data-transfer structs with no invariants at all (parsed wire records, config structs) —
those may have public fields, and their doc comment should say the type is a plain record.

### `MD-3` **MUST NOT** glob re-export

`pub use inner::*;` makes the API surface invisible in the source, causes ambiguous name collisions
on upgrade, and makes it impossible to tell what's public. Re-export named items explicitly, marked
`#[doc(inline)]`.

### `MD-4` **SHOULD** expose each item through exactly one path

Multiple paths to the same type fragment documentation, split search results, and confuse both
humans and tooling reading the crate. Pick the canonical location, re-export from there, and don't
also leave the original module public.

### `MD-5` **SHOULD** keep modules balanced in size and scope

A 3000-line module and a 12-line module in the same crate is a smell in both directions. When a
module grows past comfortable reading, split along a seam that has a name — not `mod2`. A module
should have a one-sentence description; if it needs "and", split it.

### `MD-6` **SHOULD** split the crate when in doubt

Separate crates give real compile-time enforcement of layering (a dependency cycle simply won't
compile), parallel compilation, and independent test surfaces. The cost is workspace bookkeeping.
When a boundary is architecturally important — domain vs. infrastructure, protocol vs. transport —
make it a crate, not a module.

### `MD-7` **MUST NOT** define a prelude in workspace-internal crates

Preludes obscure where names come from and encourage glob imports. Explicit `use` lines are the
point. (Public library crates with very large surfaces are the narrow exception.)

### `MD-8` **SHOULD** avoid mutable statics and global state

Globals defeat testing, create hidden coupling, and make initialisation order a problem. Pass
dependencies explicitly — usually via a context struct threaded from `main`. `OnceLock` for genuinely
immutable process-wide config is acceptable; `static mut` is not.

---

## 7. Async & concurrency (`AS-`)

### `AS-1` **MUST** treat cancellation as a first-class case

Any `.await` is a point where the future can be dropped and never resumed. Every `select!` arm, every
`timeout`, every task abort exercises this. Before writing a `select!`, ask what state is
half-mutated if this branch loses.

Rules that follow:
- Prefer cancel-safe primitives in `select!` arms (`Notify::notified()`, `mpsc::Receiver::recv()`,
  `sleep`). Check the docs — cancel-safety is documented per-method in tokio.
- Don't `select!` over a future that has taken an item out of a queue but not yet committed it.
  Take-then-drop loses the item.
- If an operation must complete, run it in `tokio::spawn` and await the `JoinHandle`, or wrap it in
  `CancellationToken`-aware code that finishes its critical section before checking for cancellation.
- Document cancel-safety on any public async fn where dropping it mid-flight has observable effects.

### `AS-2` **MUST** use structured cancellation, not fire-and-forget tasks

Spawned tasks form a tree with a `CancellationToken` per level (global → subsystem → session).
Every long-lived task either holds a token it checks, or its handle is stored so it can be aborted.
A task nobody can stop is a leak, and it will outlive the thing it was serving.

### `AS-3` **MUST NOT** block in async context

No `std::thread::sleep`, no blocking file I/O, no `Mutex` from `std` held across await, no CPU-bound
loop longer than ~100 µs on a runtime thread. Blocking a worker thread stalls every other task
scheduled on it. Use `tokio::task::spawn_blocking` for blocking work and `rayon` (or a dedicated
thread) for CPU-bound work.

### `AS-4` **MUST NOT** hold a lock across `.await`

`std::sync::MutexGuard` isn't `Send`, so it won't compile in a spawned task — but the same mistake
with `tokio::sync::Mutex` compiles and deadlocks or serialises everything. Scope the lock:

```rust
// Yes
let snapshot = {
    let state = self.state.lock().unwrap();
    state.clone()
};                          // guard dropped here
self.send(snapshot).await?;
```

Prefer `std::sync::Mutex` for short non-async critical sections even inside async code; use
`tokio::sync::Mutex` only when the critical section genuinely must span an await.

### `AS-5` **SHOULD** prefer message passing over shared state

Channels turn a synchronisation problem into a data-flow problem: one owner per piece of state, no
locks, testable by feeding messages. `mpsc` for work queues, `broadcast` for fan-out, `watch` for
"latest value" state, `oneshot` for request/response. Bound your channels — an unbounded channel is
an unbounded memory leak with backpressure disabled.

### `AS-6` **MUST** put a timeout on every external interaction

Network reads, RPCs, subprocess waits, lock acquisition on a contended resource. A peer that goes away
without closing the socket will hang an await forever, and one hung task usually takes a subsystem
with it. Timeout values are named constants with a comment about where the number came from.

### `AS-7` **SHOULD** write `async fn` rather than returning `impl Future`

`async fn foo()` in traits and inherent impls is clearer and gives better error messages. Return
`impl Future` explicitly only when you need to control the `Send`/lifetime bounds or want the body
to run eagerly up to the first await.

### `AS-8` **SHOULD** insert yield points in long-running loops

A loop that processes a large batch without awaiting starves the runtime. `tokio::task::yield_now()`
periodically, or restructure into smaller awaited units.

### `AS-9` **MUST** ensure every test with a channel or mock has a timeout

A test awaiting a receiver whose sender is never invoked hangs the whole suite, forever, with no
useful output. Wrap in `tokio::time::timeout` — a failing assert is diagnosable, a hang is not.

---

## 8. Unsafe & soundness (`US-`)

### `US-1` **MUST** avoid `unsafe` unless there is no safe alternative

Valid reasons: FFI, a measured performance requirement that safe code cannot meet, and implementing
a primitive the standard library doesn't provide. "It's faster" without a benchmark is not a reason.

### `US-2` **MUST** keep all code sound

Sound means: no safe caller, using any input, can cause undefined behaviour. An `unsafe fn` whose
preconditions can be violated by a safe caller through a public API is a defect regardless of
whether anything currently triggers it. Soundness bugs are the one category where "no test fails"
is not evidence of correctness.

### `US-3` **MUST** document every `unsafe` block with a `// SAFETY:` comment

State the invariant that makes this block sound and why it holds *here*. Every `unsafe fn` gets a
`# Safety` doc section stating the caller's obligations.

### `US-4` **MUST** encapsulate unsafe behind a safe API in the smallest possible module

Unsafe code that is spread across a crate cannot be audited. Concentrate it, wrap it in a safe
type whose invariants are enforced at construction, and test that module under Miri where feasible.

---

## 9. Performance (`PF-`)

Order of operations: correct → clear → measured → fast. `PF-` rules apply *after* a profiler says so,
with two exceptions (`PF-1`, `PF-2`) which are about not designing yourself into a corner.

### `PF-1` **SHOULD** identify the hot path early and design for it

You don't need to optimise early, but you do need to know which loop dominates the workload and
avoid an architecture that forces an allocation into it. Structural performance decisions (data
layout, copy vs. borrow at a boundary, sync vs. async) are expensive to reverse; micro-optimisations
are cheap to add later.

### `PF-2` **SHOULD** avoid needless indirection in nested types

`Vec<Box<Thing>>` where `Vec<Thing>` works, `Arc<Arc<T>>`, `Option<Box<T>>` where `T` is small.
Each layer is a cache miss. `Box` is warranted for large enum variants (to shrink the whole enum),
recursive types, and trait objects.

### `PF-3` **CONSIDER** allocation strategy in hot loops

Reuse buffers (`vec.clear()` and refill, rather than a fresh `Vec` per iteration). Give collections
an initial capacity when the size is known or estimable. `shrink_to_fit` after building a
long-lived collection. Avoid `format!`/`to_string` in per-item hot loops.

### `PF-4` **CONSIDER** a faster hasher for internal maps

`std`'s SipHash is DoS-resistant, which matters for maps keyed by untrusted input and is pure
overhead for maps keyed by internal integers. `rustc-hash`/`ahash` are substantially faster —
but never for a map whose keys come from the network.

### `PF-5` **SHOULD** measure with benchmarks, not intuition

Criterion benchmarks for anything claimed to be a performance improvement. A PR that says "faster"
without numbers hasn't established anything, and Rust's optimiser routinely invalidates intuition.

### `PF-6` **CONSIDER** stack size of hot async functions

Async fn state machines embed all live-across-await locals. A large array or a deeply nested future
in a frequently-spawned task multiplies. Box large futures or move big buffers behind a pointer.

---

## 10. Testing (`TS-`)

### `TS-1` **MUST** test through trait seams, not live external systems

Every external dependency (network, device, clock, filesystem, message broker) sits behind a trait
defined by its consumer (`TR-2`). Tests substitute a fake. This is the single practice that most
determines whether a codebase can be tested at all, and it's a design decision, not a test decision.

### `TS-2` **MUST** feature-gate test doubles

Fakes, mocks, and builders live behind a `test-util` (or similar) Cargo feature, so a release build
physically cannot substitute a fake for the real thing. Exposing a fake unconditionally in a
library is a production hazard.

### `TS-3` **MUST NOT** write tautological tests

A test that asserts the implementation against itself — recomputing the expected value with the
same code path, or asserting a mock returns what the mock was configured to return — proves
nothing and blocks refactoring. Assert against independently-derived ground truth: a spec value, a
hand-computed result, a captured fixture, a property.

This is the failure mode that most often shows up in generated tests. When writing a test, state
what would have to break for it to fail; if the answer is "nothing", delete it.

### `TS-4` **SHOULD** keep unit tests in-module and integration tests in `tests/`

`#[cfg(test)] mod tests` inside the file for white-box unit tests of private functions; `tests/*.rs`
for anything exercising the public API. The `tests/` directory linking against the crate as an
external consumer is itself a check that the public API is usable.

### `TS-5` **SHOULD** prefer property-based tests for parsers, codecs and invariants

Round-trip properties (`decode(encode(x)) == x`), invariant properties (output always sorted, never
panics), and differential properties are worth far more than a handful of examples for anything
that consumes untrusted bytes. Add fuzz targets for wire-format parsers.

### `TS-6` **MUST** make tests deterministic and independent

No wall-clock sleeps as synchronisation (use notifications or paused time), no shared global state,
no ordering dependencies between tests, no reliance on network. A flaky test is worse than no
test — it trains everyone to ignore red.

### `TS-7` **SHOULD** name tests for the behaviour under test

`disconnect_during_handshake_releases_lease`, not `test_disconnect_2`. The name is the failure
report you'll read first.

---

## 11. Dependencies & build (`DP-`)

### `DP-1` **MUST** keep features additive

Enabling a feature may add API; it must never remove or change existing behaviour. Cargo unifies
features across the dependency graph, so a subtractive or mutually-exclusive feature will break a
consumer who never asked for it. No `no-std` feature that *disables* std — invert it.

### `DP-2` **MUST** centralise shared dependency versions in the workspace

`[workspace.dependencies]` with members using `dep.workspace = true`. Same for `edition`,
`rust-version`, `license`, and lint configuration. Divergent versions across members are a
compile-time and binary-size cost with no benefit.

### `DP-3` **SHOULD** justify each new dependency

Every crate added is a supply-chain, compile-time, and maintenance cost. Check: is it maintained,
is the API surface you need small enough to inline, does it pull a large tree, does it have unsafe
you'd be inheriting. For a 40-line utility, write the 40 lines.

### `DP-4` **MUST** commit `Cargo.lock` for binaries and build with `--locked` in CI

Reproducibility is a property you either have or don't. Also: no build-time timestamps, hostnames,
or absolute paths embedded in artefacts; if a build date is needed it comes from `SOURCE_DATE_EPOCH`.

### `DP-5` **SHOULD** update MSRV conservatively and deliberately

Raising the minimum supported Rust version is a breaking change for consumers. Bump it in its own
commit with a stated reason, not as a side effect of using a new API.

### `DP-6` **MUST** run the full static-verification gate

`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, plus coverage
if the project gates on it. Lint suppressions use `#[expect(lint, reason = "…")]` rather than
`#[allow]` — `expect` fails the build when the suppression becomes unnecessary, so dead suppressions
don't accumulate.

---

## 12. Documentation & observability (`DC-`)

### `DC-1` **MUST** write a one-line first sentence (~15 words) for every public item

Rustdoc uses it as the summary in listings. A first sentence that spans three lines renders as an
unreadable table cell.

### `DC-2` **MUST** document every public item, with the canonical sections where applicable

`# Examples`, `# Errors`, `# Panics`, `# Safety`. The example is the most valuable part — it is
compile-checked, it's what readers copy, and writing it is the fastest way to discover that an API
is awkward. Examples use `?`, not `unwrap()`.

### `DC-3` **MUST** have module-level docs (`//!`) explaining the module's job

What lives here, what doesn't, and how it relates to its neighbours. This is the map; without it a
reader has to reverse-engineer intent from the item list.

### `DC-4` **MUST** write comments that explain *why*, not *what*

The code says what. A comment earns its place by recording a hidden constraint, a workaround for
external behaviour, a non-obvious invariant, or a rejected alternative. `// increment the counter`
above `counter += 1` is noise. Never reference a task, PR, or ticket as if the reader has it open.

### `DC-5` **MUST** use structured logging, never `println!`, in library and service code

`tracing`-style structured fields (`debug!(peer = %addr, retries, "reconnecting")`) rather than
interpolated prose, so logs can be filtered and queried. Message templates stay constant; the
variables are fields.

### `DC-6` **MUST NOT** let logging tank performance

No formatting work outside the level check (the macros handle this; manual `format!` before the call
does not). No logging in a per-packet hot loop above `trace`. Be deliberate about secrets — no
credentials, tokens, or hashes at any level that's on by default.

### `DC-7` **SHOULD NOT** write meta-design documentation in the codebase

Docs that describe the process of building the code ("refactored in phase 2", "TODO from the
migration plan") rot immediately and mislead readers and agents. Document the code as it is.
Historical rationale belongs in commit messages; forward plans belong in issues.

---

# Part II — Design patterns

Structural/architectural patterns (hexagonal, DDD, vertical slices, workspace organisation) are
treated in survey form here; the in-depth structural reference is `rust-code-structure.md`.

## The organising principle: composition over inheritance

Rust has no inheritance, and this is a design position, not a gap. Inheritance couples subtype to
supertype implementation, makes behaviour non-local, and produces hierarchies that are wrong as
soon as a second axis of variation appears. Rust decomposes the several distinct things
inheritance conflates into separate mechanisms:

| Inheritance is used for… | Rust mechanism |
|---------------------------|----------------|
| Shared interface | `trait` |
| Shared implementation | Default trait methods, or a helper struct held as a field |
| Subtype polymorphism, open set | `dyn Trait` |
| Subtype polymorphism, closed set | `enum` + `match` |
| Code reuse via "is-a" | Composition: hold the other type as a field, delegate explicitly |
| Extending a foreign type | Extension trait, or newtype wrapper |
| Template method | Trait with default methods calling required methods |
| Abstract base class | Trait with default methods + associated types |

Practical consequences:

- **Delegate explicitly.** If `Reconnector` needs `Backoff` behaviour, it holds a `Backoff` field
  and calls it. The delegation is three visible lines instead of an invisible vtable walk. Do not
  use `Deref` to make it implicit (`TR-6`).
- **Prefer "has-a" and "can-do" to "is-a".** Model capabilities (`trait Cacheable`) rather than
  taxonomies (`trait AbstractResource`).
- **Let the compiler find the update sites.** A new enum variant breaking every `match` is the
  behaviour you want when adding a case to a closed set; a new subclass silently inheriting stale
  behaviour is the behaviour you don't.

---

## Newtype

**Problem:** primitives carry no meaning, and you can't implement a foreign trait on a foreign type.

**Shape:** `struct Meters(f64);` — a single-field tuple struct, private field, fallible constructor,
accessor, and the traits it deserves (`Display`, `FromStr`, `Serialize`).

**Use it for:**
- Type-checked distinctions between same-shaped values (`UserId` vs `OrderId`).
- Hanging validation on a value so it's checked once, at the boundary (`TY-3`).
- Working around orphan rules: `struct MyVec(Vec<T>)` to impl a foreign trait.
- Hiding an implementation type from your public API so you can change it later.

**Cost:** zero at runtime; some boilerplate at the boundary. Worth it every time the value crosses
a module or crate.

**Related rules:** `TY-2`, `TY-3`, `MD-2`.

---

## Typestate

**Problem:** an object has a lifecycle, and half its methods are illegal in any given state. Runtime
checks (`if !self.connected { return Err(…) }`) push the error to runtime and to every caller.

**Shape:** each state is a type; transitions consume `self` and return the next type. Invalid calls
don't compile, and the old handle is gone so it can't be reused.

```rust
pub struct Connected;
pub struct Authenticated;
pub struct Session<S> { conn: TcpStream, _state: PhantomData<S> }

impl Session<Connected> {
    pub async fn connect(addr: SocketAddr) -> Result<Self, ConnectError> { … }
    pub async fn login(self, creds: &Credentials) -> Result<Session<Authenticated>, AuthError> { … }
}
impl Session<Authenticated> {
    pub async fn query(&self, q: Query) -> Result<Rows, QueryError> { … }
}
```

**Use it when:** the state machine is small, transitions are linear-ish, and misuse is expensive —
protocol handshakes, transaction lifecycles, builder→built, hardware initialisation sequences.

**Don't use it when:** the state is dynamic (chosen at runtime, stored in a collection of mixed
states, or read from config). Type-level state and runtime dispatch fight each other; you end up
boxing everything and losing the benefit. Then use an enum-based state machine instead:
`enum ConnState { Disconnected, Connected(Session), … }` with `match`.

**Cost:** generic parameters propagate into every type that holds the object, and error messages get
worse. Use it at leaf APIs, not in the middle of a deeply generic stack.

**Related rules:** `TY-1`, `TY-7`.

---

## Builder

**Problem:** a type with many optional fields, or one whose construction must be validated as a
whole.

**Shape:** `Thing::builder()` → chainable setters taking `self` → `build() -> Result<Thing, Error>`.

**Rules:**
- **Validate in `build()`**, not in the setters. Cross-field invariants can only be checked once
  everything is present, and a setter that can fail makes chaining unusable.
- `build()` returns `Result` whenever any invariant exists. A `build()` returning `Thing` is a claim
  that no configuration is invalid.
- Consuming (`fn port(mut self, p: u16) -> Self`) is the default; use `&mut self` builders only when
  callers need to configure conditionally in a loop.
- Don't add a builder to a struct with three fields, all required. `Thing::new(a, b, c)` is better.

**Related rules:** `TY-1`, `TR-9`, `ER-2`.

---

## RAII guards

**Problem:** a resource, a counter, or a permission must be released exactly once, on every path
including panics and early returns.

**Shape:** a guard type acquires in its constructor and releases in `Drop`. The guard's *lifetime*
is the resource's lifetime, enforced by the compiler.

This is Rust's most under-used pattern relative to its value. It applies well beyond memory: lock
guards, in-flight request counters, connection leases, span timers, temp-file cleanup, feature
toggles restored on scope exit.

```rust
pub struct InFlightGuard { count: Arc<AtomicUsize>, notify: Arc<Notify> }

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.notify.notify_one();
        }
    }
}
```

**Constraints:** `Drop` can't fail (`ER-7`) and can't be async. If release can fail or must await,
provide an explicit `close().await` and make `Drop` a best-effort logged fallback. Never rely on
`Drop` running — `mem::forget`, process abort, and cycles all skip it — so it must not be the only
mechanism guaranteeing a *correctness* property outside the process.

---

## Sans-IO

**Problem:** protocol logic entangled with sockets is untestable without a network, unusable with a
different transport, and impossible to reason about in isolation.

**Shape:** the protocol implementation is a pure state machine — it accepts bytes and events, and
returns decisions and bytes to send. It performs no I/O and knows no runtime.

```rust
pub struct Protocol { state: State, outbound: VecDeque<Frame> }

impl Protocol {
    pub fn handle_input(&mut self, bytes: &[u8]) -> Result<Vec<Event>, ProtocolError>;
    pub fn poll_output(&mut self) -> Option<Frame>;
    pub fn handle_timeout(&mut self, now: Instant) -> Vec<Action>;
}
```

The async transport layer becomes a thin loop: read socket → `handle_input` → drain `poll_output` →
write socket.

**Why it matters:** the protocol becomes testable with byte-array fixtures and no runtime, it
works over TCP, UDP or an in-memory pipe unchanged, it's trivially fuzzable, and cancellation
(`AS-1`) becomes tractable because all mutation happens in synchronous non-cancellable methods.

**Cost:** an extra buffering layer and an explicit output queue. Worth it for anything with a wire
format; not worth it for a single HTTP call.

**Related:** this is the same insight as hexagonal architecture, applied at the protocol level.

---

## Ports & adapters (hexagonal)

**Problem:** business logic that imports a database driver, an HTTP client, and a broker SDK can
only be tested by standing all three up, and can never be reused.

**Shape:** three concentric zones with dependencies pointing strictly inward.

```
        ┌──────────── adapters (outer) ───────────┐
        │  HTTP API   queue consumer   CLI        │   ← driving (inbound)
        │  ────────────────────────────────       │
        │  ┌──────── application ───────┐         │
        │  │  use cases, orchestration  │         │
        │  │  ┌──── domain (core) ────┐ │         │
        │  │  │ entities, values,     │ │         │
        │  │  │ invariants, ports     │ │         │
        │  │  └───────────────────────┘ │         │
        │  └────────────────────────────┘         │
        │  database  mail queue  clock  telemetry │   ← driven (outbound)
        └─────────────────────────────────────────┘
```

**In Rust specifically:**

| Hexagonal concept | Rust realisation |
|-------------------|------------------|
| Port (outbound) | A trait defined in the inner layer describing what it needs (`TR-2`) |
| Port (inbound) | The public API of the application layer — usually inherent methods on a service type |
| Adapter | A struct in an outer crate implementing the port trait |
| Dependency injection | Generic parameter (`<S: Store>`) or `Arc<dyn Store>`, chosen per `TR-1` |
| Composition root | `main.rs` — the only place that names both a port and its concrete adapter |
| Layer enforcement | Crate boundaries. A cycle won't compile (`MD-6`). |

**The load-bearing rule:** the inner crates must not depend on the outer ones, in Cargo.toml terms.
If `domain/Cargo.toml` lists `tokio`, `sqlx`, or `reqwest`, the architecture has already failed —
this is checkable mechanically and is the best single indicator of whether the layering is real.

**Calibration.** Full hexagonal on a small tool is ceremony. The trigger for adopting it is: more
than one adapter for the same port (a real one and a test one counts), or a domain rule complex
enough to be worth testing without infrastructure. A single-adapter port with no test double is
speculative abstraction (`TR-1`).

---

## Domain-Driven Design tactical patterns in Rust

DDD's strategic side (ubiquitous language, bounded contexts) applies unchanged. The tactical
patterns map onto Rust unusually well, because Rust's ownership model expresses aggregate
boundaries directly.

| DDD pattern | Rust realisation | Notes |
|-------------|------------------|-------|
| **Value object** | Newtype or small `Copy`/`Clone` struct, immutable, `PartialEq` by value, no identity | The natural Rust default. `TY-2`. |
| **Entity** | Struct with an ID newtype; `PartialEq`/`Hash` delegate to the ID only | Never derive `PartialEq` on an entity — it would compare state, not identity. |
| **Aggregate** | One owning struct; children are private fields, not shared pointers; all mutation goes through the root's `&mut self` methods | Rust *enforces* the aggregate boundary: if children aren't reachable except through the root, invariants can't be bypassed. This is the pattern's strongest fit. |
| **Repository** | Trait defined in the domain, implemented in infrastructure | The canonical outbound port. `TR-2`. |
| **Domain service** | Free function or a struct with no state, taking domain types | Use when an operation doesn't belong to any single entity. |
| **Domain event** | Enum of variants, returned from aggregate methods rather than dispatched inside them | Returning events keeps the domain pure and I/O-free — the caller publishes. Pairs with sans-IO. |
| **Factory** | Associated constructor or builder returning `Result` | Validation lives here, so no invalid aggregate exists (`TY-1`). |
| **Specification** | Closure or small trait returning `bool` | Rarely worth a trait; a named predicate function is usually clearer. |
| **Anti-corruption layer** | A module converting external DTOs to domain types via `TryFrom` | Keeps wire/vendor shapes out of the domain. Every external representation gets one. |

**The single most valuable idea:** an aggregate root that owns its children as plain fields, mutated
only through its own methods, is a correctness guarantee in Rust rather than a convention. Sharing
children via `Arc<Mutex<Child>>` destroys it — which is another reason for `OW-5`.

**What to skip:** if the domain is a protocol translator with essentially no business rules, DDD's
tactical patterns add layers and buy nothing. Take the value objects and the anti-corruption layer,
leave the rest.

---

## Actor / message passing

**Problem:** a piece of state with several concurrent readers and writers, where lock-based sharing
produces contention, deadlock risk and await-across-lock hazards.

**Shape:** the state lives in exactly one task. Others send messages through an `mpsc` channel; a
`oneshot` sender inside the message carries the reply. The state is never locked because it's never
shared.

```rust
enum Command {
    Get { key: Key, reply: oneshot::Sender<Option<Value>> },
    Put { key: Key, value: Value, reply: oneshot::Sender<Result<(), StoreError>> },
    Shutdown,
}

#[derive(Clone)]
pub struct StoreHandle { tx: mpsc::Sender<Command> }   // cheap, Clone, hides the channel
```

**Why it fits Rust:** it converts a `Send`/`Sync`/lifetime problem into a plain data problem, it
makes ordering explicit, and the handle type gives you a natural `Clone` service type (`OW-3`).

**Costs and rules:** bound the channel or you've built a memory leak (`AS-5`); decide what happens
when the actor dies and the `oneshot` is dropped (callers get `RecvError` — map it to a domain
error, don't `unwrap`); avoid actor-calls-actor cycles, which deadlock exactly like locks do.

---

## Strategy, adapter, façade

The classic GoF patterns that survive translation, in their Rust forms:

- **Strategy** — a trait with the algorithm, injected per `TR-1`. Frequently just a closure field
  (`Box<dyn Fn(&Request) -> Decision>`) when there's one method.
- **Adapter** — a newtype wrapping a foreign type and implementing your trait. This is the
  hexagonal adapter and the orphan-rule workaround at once.
- **Façade** — a service struct with inherent methods over a complex subsystem (`TR-9`). Keeps the
  complex parts `pub(crate)`.
- **Observer** — channels (`broadcast`/`watch`), not callback registries. Callback lists in Rust
  drag in `Arc<Mutex<Vec<Box<dyn Fn>>>>` and lifetime pain; a broadcast channel does the same job
  with none of it.
- **Visitor** — usually unnecessary; `match` on an enum is the visitor, with exhaustiveness checked.
- **Singleton** — usually an anti-pattern (`MD-8`). Pass the dependency. `OnceLock` if truly
  process-global and immutable.
- **Iterator/Generator** — implement `Iterator`; it composes with the whole ecosystem for free.

Patterns that do **not** translate and should be treated as smells: Abstract Factory hierarchies,
Template Method via inheritance chains, Decorator stacks of `Box<dyn>` wrapping `Box<dyn>`
(prefer a config struct or a builder), and any "AbstractBaseImpl" naming.

---

## Anti-pattern catalogue (`AP-`)

Recognise and reject these. Each one has a rule that supersedes it.

| Anti-pattern | Why it's wrong | Instead |
|--------------|----------------|---------|
| `Rc<RefCell<T>>` graph as a default design | Runtime borrow panics, no thread safety, ownership becomes untraceable | Rethink ownership; message passing (`OW-5`, `AS-5`) |
| `Arc<Mutex<AppState>>` holding everything | One lock serialises the whole program; guaranteed await-across-lock bugs | Split by ownership; actor per state (`AS-4`) |
| `Deref` to fake inheritance | Confusing resolution, breaks generics | Explicit delegation (`TR-6`) |
| Stringly-typed APIs | No validation, no type checking, endless parsing | Newtypes and enums (`TY-2`, `TY-4`) |
| `unwrap()` everywhere, cleaned up "later" | Later never comes; each one is a latent crash | `?` and typed errors (`ER-5`) |
| One `Error` enum for the whole workspace | Every caller matches on variants that can't occur here | Per-module error types, `From` conversions (`ER-1`) |
| Trait with one impl, added "for flexibility" | Abstraction cost with no benefit; obscures the real type | Use the concrete type until a second impl exists (`TR-1`) |
| `utils` / `helpers` / `common` module | Unrelated code with no cohesion; grows without bound | Find each item its real home (`NM-4`) |
| Speculative generality (generics/features for hypothetical needs) | Cost is paid now, benefit may never arrive | Write the concrete version; generalise on the second case |
| Defensive checks for impossible states | Hides the real invariant, adds untestable branches | Make it unrepresentable (`TY-1`); `panic!` if it's a bug (`ER-4`) |
| Blocking call in async fn | Stalls every task on the worker thread | `spawn_blocking` (`AS-3`) |
| Unbounded channel | Unbounded memory, no backpressure | Bounded channel with a considered capacity (`AS-5`) |
| God struct / god module | Everything depends on it; no seam to test at | Split by responsibility (`MD-5`) |
| Comments restating the code | Rots, adds noise, hides real comments | Comment the why (`DC-4`) |
| Tests asserting mock behaviour | Proves nothing; blocks refactoring | Independent ground truth (`TS-3`) |

---

# Part III — Applying it

## Quick decision tables

**Abstraction mechanism**

| Situation | Choose |
|-----------|--------|
| One implementation, nameable | The concrete type |
| Closed, known set of variants | `enum` |
| Open set, compile-time known per call site | Generic `<T: Trait>` |
| Open set, heterogeneous or runtime-selected | `dyn Trait` |
| Need to break a compile-time dependency for layering | Trait + `dyn`, defined by the consumer |

**Error mechanism**

| Situation | Choose |
|-----------|--------|
| Library, caller may recover | `thiserror` enum, typed variants |
| Binary, caller logs and exits | `anyhow::Result` + `.context()` |
| Caller broke a documented contract | `panic!` |
| Our own invariant broken | `panic!` (it's a bug) |
| Absence with no reason to explain | `Option<T>` |

**Shared state mechanism**

| Situation | Choose |
|-----------|--------|
| Single owner, passed down | `&mut T` |
| Read-mostly, immutable after init | `Arc<T>` or `OnceLock<T>` |
| Short synchronous critical section | `std::sync::Mutex<T>` |
| Critical section must span `.await` | `tokio::sync::Mutex<T>` (and question the design) |
| Many writers, complex state | Actor: `mpsc` + owning task |
| Latest-value broadcast to many readers | `watch` channel |
| Event fan-out | `broadcast` channel |

---

## Review checklist

Run this on any non-trivial change before declaring it done.

**Types & API**
- [ ] Can any illegal state be represented? (`TY-1`)
- [ ] Any primitive parameter that should be a newtype, or `bool` that should be an enum? (`TY-2`, `TY-4`)
- [ ] Public struct fields — intended, or leaked? (`MD-2`)
- [ ] Signatures accept the least restrictive type and return concrete types? (`OW-1`)
- [ ] Any smart pointer in a public signature? (`OW-3`)

**Errors**
- [ ] Typed errors in libraries, context in the binary? (`ER-1`)
- [ ] Every `unwrap`/`expect` justified with a stated invariant? (`ER-5`)
- [ ] Bugs panic, failures return `Result`? (`ER-4`)
- [ ] `# Errors` / `# Panics` sections present? (`ER-6`)

**Abstraction**
- [ ] Each new trait has ≥2 implementations (a test double counts)? (`TR-1`)
- [ ] Traits defined by the consumer, so dependencies point inward? (`TR-2`)
- [ ] Traits narrow enough to fake in a test without stubbing irrelevant methods? (`TR-3`)

**Async**
- [ ] Every `select!` arm cancel-safe; nothing half-committed on cancellation? (`AS-1`)
- [ ] Every spawned task reachable by a cancellation token or handle? (`AS-2`)
- [ ] No lock held across `.await`; no blocking call in async? (`AS-3`, `AS-4`)
- [ ] Timeout on every external interaction? (`AS-6`)
- [ ] Every channel bounded? (`AS-5`)

**Tests**
- [ ] Tests assert independent ground truth, not the implementation? (`TS-3`)
- [ ] Test doubles feature-gated? (`TS-2`)
- [ ] Every mock/channel test wrapped in a timeout? (`AS-9`)
- [ ] Deterministic — no sleeps as synchronisation, no shared state? (`TS-6`)

**Hygiene**
- [ ] `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean? (`DP-6`)
- [ ] New dependency justified? (`DP-3`)
- [ ] Comments explain why; no meta/process commentary? (`DC-4`, `DC-7`)
- [ ] No secrets in logs; structured fields not interpolated prose? (`DC-5`, `DC-6`)

---

## Applying to existing code

- **New code**: follows this document.
- **Code you're modifying**: bring the part you touch into line if it's cheap and local. Don't
  restructure surrounding code to satisfy a rule.
- **Code you're only reading**: leave it. A drive-by refactor makes the diff unreviewable and mixes
  behavioural change with stylistic change.
- **A rule that fights the codebase repeatedly**: that's a signal to discuss the rule, not to
  silently ignore it. Raise it rather than accumulating exceptions.
- **Deviation**: state the reason in the PR description or a code comment, referencing the rule ID.
  A deviation with a reason is a decision; one without is a defect.

---

## Sources

- [Rust API Guidelines checklist](https://rust-lang.github.io/api-guidelines/checklist.html) — the
  upstream `C-*` conventions (naming, interoperability, type safety, future-proofing).
- [Microsoft Pragmatic Rust Guidelines](https://microsoft.github.io/rust-guidelines/guidelines/checklist/index.html)
  — the `M-*` rules, including the AI-oriented guidance (`M-TAUTOLOGICAL-TESTS`,
  `M-SINGLE-ITEM-PATH`, `M-NO-META-DESIGN-DOCUMENTATION`) reflected in `TS-3`, `MD-4`, `DC-7`.
- [Master Hexagonal Architecture in Rust](https://www.howtocodeit.com/guides/master-hexagonal-architecture-in-rust)
  and [How to apply hexagonal architecture to Rust](https://www.barrage.net/blog/technology/how-to-apply-hexagonal-architecture-to-rust)
  — ports-as-traits, adapters-as-structs, composition root.
- [The Typestate Pattern in Rust (Cliffle)](https://cliffle.com/blog/rust-typestate/) and
  [Microsoft Rust Patterns: Newtype and Typestate](https://microsoft.github.io/RustTraining/rust-patterns-book/ch03-the-newtype-and-type-state-patterns.html).
- [Cancelling async Rust (sunshowers)](https://sunshowers.io/posts/cancelling-async-rust/) and
  [Oxide RFD 400: Dealing with cancel safety in async Rust](https://rfd.shared.oxide.computer/rfd/0400)
  — the basis for `AS-1`.
- [Composition over inheritance](https://en.wikipedia.org/wiki/Composition_over_inheritance) —
  the general principle underlying Part II's opening table.
