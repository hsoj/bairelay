# Bairelay — Remediation Plan

Working document, not a reference. Tracks the gap between the codebase as it
stands and Rust best practice / sensible defaults, ordered by risk. Items get
struck through and moved to **Landed** as they go in; when the list empties,
this file goes away.

Opened 2026-07-26 from a full-workspace review. Static structure lives in
`docs/architecture.md`; day-to-day gotchas in `docs/implementation.md`.

---

## Baseline

The project's own merge gate — `cargo fmt --all --check`, `cargo clippy
--all-targets -- -D warnings`, `cargo test`, `cargo tarpaulin` — was green on
`fmt` and `clippy` at the time of review, and red on `test` (P0-1, since
landed; all four green as of 2026-07-27).

This is above-average Rust. No `unsafe` in production paths, no unbounded
channels, no lock guards held across `.await`, `thiserror` in libraries and
`anyhow` only in the binary, sensible `Arc` / `CancellationToken` task trees,
capped maps at every network boundary, and unusually good load-bearing
comments. Everything below sits on that foundation — none of it is a rewrite.

Findings came from reading the network-facing and concurrency-critical paths
directly, plus a pedantic + nursery clippy sweep (1899 warnings, mostly style
noise, filtered down to the entries cited here).

### How risk was ranked

Risk = likelihood of biting × blast radius, calibrated for what bairelay
actually is: a daemon that binds `0.0.0.0:8554` by default on a home LAN,
speaks an undocumented binary protocol to cameras over UDP, and publishes four
crates to crates.io.

Effort: **S** under an hour · **M** half a day · **L** multi-day.

---

## P0 — Blocking or actively exposed

### P0-3. Poison-panic cascade in shared RTSP state · M

50 sites. Distribution:

| Location | Sites | Note |
|---|---|---|
| `src/rtsp/server/registry.rs` | 17 | shared by every connection |
| `src/baichuan/bc_protocol/fake_camera.rs` | 11 | test-util — leave alone |
| `src/wake_server/registry.rs` | 9 | |
| `src/rtsp/buffer.rs` | 6 | shared by every session |
| `src/rtsp/server/session_task.rs`, `server/udp_pool.rs` | 4 | |
| `src/startup_wake.rs:158,234`, `src/camera_provider.rs:95` | 3 | |

`src/stream_source.rs:82–105` documents precisely why this is wrong —
*"cascades a single bug across every other holder"* — and ships `lock_recover` /
`RwLockPoisonRecover` / `MutexPoisonRecover` to fix it. The binary uses them; the
two published library crates do not.

`SessionRegistry` is shared by every RTSP connection and `LastFrameBuffer` by
every session, so one panic under those locks takes down the whole server rather
than one client. `src/wake_server/route.rs:149` already uses
`unwrap_or_else(|p| p.into_inner())`, so the idiom exists in-tree and just is not
applied uniformly.

Fix: hoist the helpers out of `stream_source.rs` (see P3-1), then sweep.

---

## P1 — Latent correctness and silent failure

### P1-1. No supply-chain scanning · S

No `cargo-deny`, no `cargo-audit`, no Dependabot, no `deny.toml`. `codeql.yml`
runs weekly but CodeQL's Rust support will not catch a RUSTSEC advisory in a
transitive dependency.

379 crates in the tree, including `rustls` and `aws-lc-rs`. Combined with the
reproducible-builds contract — `Cargo.lock` committed, every CI invocation
`--locked` — a pinned lockfile with no advisory feed means known-vulnerable
versions are held indefinitely and silently.

Fix: add a `cargo-deny` CI job (advisories + licences + bans). The `bans` check
also stops P2-4 from regressing.

### P1-2. MSRV is declared but never verified · S

`rust-version = "1.93"` (`Cargo.toml:65`), toolchain pinned to `1.94.1`
(`rust-toolchain.toml`). Nothing builds against 1.93, so the published crates'
MSRV claim is unfalsified and will drift the first time someone uses a 1.94 API.
Add a CI job that builds with the declared MSRV, or drop the claim.

### P1-4. Live-verify log-marker contract is unpinned · S — *partially landed*

`tests/scripts/manual-verify.sh` is the live-hardware gate for the RTSP path,
and it drives the daemon purely by grepping stdout:

| Marker | Emitted at | Script use |
|---|---|---|
| `RTSP server listening` | `src/rtsp/server/listener.rs:78` | 30 s startup gate |
| `RTSP server started` | `src/main.rs:415` | same gate, alternate match |
| `Startup wake cycle complete` | `src/startup_wake.rs:102` | 60 s warm-cycle gate |
| `Grace period expired, disconnecting` | `src/camera.rs:1127` | battery-sleep stage |
| `Disconnected` | `src/camera.rs:1200` | battery-sleep stage |

Reword any of them and the script does not fail loudly — it stalls out its poll
window and reports a *misleading* FAIL, the failure mode most likely to be
written off as flaky hardware. Nothing else in the suite asserts on rendered log
output, so these strings were free to drift.

Landed: `src/log_capture.rs` (test-only global `tracing` capture, no new
dependencies) plus two tests pinning `Startup wake cycle complete` and asserting
the empty-map early return does *not* falsely claim completion.

Remaining: the two `src/camera.rs` lifecycle markers, and `RTSP server
listening` — the latter needs either a `tracing-subscriber` dev-dependency on
`src/rtsp` or a ~30-line bare `Subscriber` impl to stay dependency-free.

### P1-6. Stale `Server:` header · S

`src/rtsp/protocol/message.rs:161` hardcodes `bairelay/0.1.0` while the
workspace is at 1.1.2 — against the repo's own "version lives once in
`[workspace.package].version`" rule. Use `concat!("bairelay/",
env!("CARGO_PKG_VERSION"))`.

### P1-7. Fuzz targets exist but never run · S

Eight targets covering the whole untrusted-input surface — `bc_deserialize`,
`bcudp_deserialize`, `bcxml_try_parse`, `nal_split_decode`, `aac_parse_adts`,
`adpcm_decode_block`, `udp_flow_state`, `wake_server_decode_discovery` — plus
`scripts/fuzz.sh` to drive them, with zero CI wiring. A nightly smoke run at the
script's default 10 s/target turns dormant assets into an active regression net.

---

## P2 — API hygiene on published crates

All four library crates carry `version` / `license` / `repository` and no
`publish = false`. These are public-API commitments, not internal code.

### P2-1. No `[workspace.lints]`; doc lint on 1 of 4 crates · M

Only `src/baichuan` sets `#![warn(missing_docs)]` and
`#![warn(unused_crate_dependencies)]`. Building the other three with
`-W missing_docs` yields **196 undocumented public items**.

Rust 1.74+ `[workspace.lints]` in the root manifest plus `[lints] workspace =
true` per member makes this uniform, and moves lint policy out of CI flags into
the manifest — which fits the reproducible-builds contract better than
`-D warnings` at the call site.

Do this before P2-2 and P2-3: it is what makes them enforceable rather than
advisory.

### P2-2. `#[non_exhaustive]` on 1 of 46 public enums · S

Adding a variant to a public enum is a breaking change.
`bairelay_neolink_core::bc_protocol::Error` alone has ~40 variants describing an
evolving reverse-engineered wire protocol — it *will* grow, and today every
growth is a semver-major.

### P2-3. `#[must_use]` essentially absent · S

Two occurrences in 91k lines. The RAII guards are exactly where it matters:
`WakeLockGuard` (`src/wake_lock.rs:26`) and `SubscriptionHandle`.
`wake_lock.acquire();` as a bare statement acquires and instantly releases —
correct-looking, silently wrong, and the wake lock is the core of the
battery-camera design.

### P2-4. Duplicate dependency trees · M

`cargo tree -d`: `rand` 0.8 **and** 0.9, `getrandom` ×3, `rand_core` ×2,
`thiserror` 1 **and** 2, `hashbrown` ×2, `rustls-webpki` ×2.

`rand 0.8` is a *direct* dependency of `core`, `rtsp`, and `wake-server`; 0.9
arrives transitively. `thiserror 1` comes via `rtp-types 0.1.2`. Migrating the
first-party `rand` to 0.9 collapses most of the duplication.

RNG *usage* is correct and needs no change — `thread_rng` / `OsRng` for nonces
and session IDs, no weak PRNG in any security path.

### P2-5. Large futures · S

Hundreds in the 16–36 KB range; `src/oneshot/runner.rs:50` is 33,992 bytes. With
one task tree per camera plus per-session tasks, these inflate every `select!`
arm that holds them. `Box::pin` the worst handful — clippy's `large_futures`
names them.

---

## P3 — Structure and maintainability

### P3-1. Sync utilities live in the largest domain module · S

`lock_recover` / `rlock_recover` / `wlock_recover` / `RwLockPoisonRecover` /
`MutexPoisonRecover` are defined in `src/stream_source.rs` (5441 lines) and
imported by `src/camera.rs` (29 uses) and `src/status_cache.rs` (11). The same
file also exports `SDP_POLL_INTERVAL` as crate-wide config.

Move to `src/sync.rs`. This is a prerequisite for P0-3, so do it there.

### P3-2. Error-type layering inversion · M

The binary manufactures `bairelay_neolink_core::bc_protocol::Error::Other("...")`
for failures that are purely binary-layer concerns: `src/mqtt_dispatch.rs:195`
("PTZ preset name not in cache"), `:343` ("Command timed out"),
`src/startup_wake.rs:321,622`.

The core protocol error is being used as the binary's generic failure channel,
inverting the stated layering rule and making `Other(&'static str)` a
stringly-typed escape hatch. A binary-local `DispatchError` wrapping
`core::Error` restores the seam.

### P3-3. Camera trait width · L — **RESOLVED by S4-2**

Historical finding: the camera seam (then `CameraDriver`, later a 33-method
`trait Camera`) was too wide for any single consumer. Resolved by the
eight-flat-role-trait split in `src/camera.rs` (`Session`, `Video`,
`Stills`, `Events`, `Power`, `Lighting`, `Ptz`, `DeviceAdmin`);
consumers now take the narrowest role, per-role fakes exist in
`src/fake_camera/roles.rs`.

**Wants an explicit decision either way, recorded in
`docs/implementation.md`.** Keeping the fat trait is a defensible answer;
continued drift is not.

### P3-4. Comment debt · S

~30 stale build-plan references — `Task 3/5/6/11/19/22/23/24/27/33`, `Phase
1.5`, `Stage 6` — across `src/stream_source.rs`, `src/camera.rs`,
`src/mqtt/discovery/`, `src/baichuan/bc/xml_tests.rs`, and the RTSP
integration test header. Directly against `CLAUDE.md`'s *"Never reference the
current task/PR."* Several describe shipped work in future tense.

Plus one corrupted comment at `src/camera_provider.rs:91`, where a newline was
eaten mid-edit and left `§3.3 of the \t\t// design doc` inline.

### P3-5. Test data race on `RUST_LOG` · S

`src/cli_convert.rs:358,365,1016,1021,1025`. Two tests `set_var` / `remove_var`
on `RUST_LOG` and run in parallel threads of the same process — the exact UB
`set_var` was made `unsafe` to flag. The `// SAFETY (single-threaded test...)`
comment asserts something `cargo test` does not provide.

Best fix: make `verbosity_env_filter` take the value as a parameter, which
removes the `unsafe` entirely.

### P3-6. God modules · L

`src/stream_source.rs` at 5441 lines (2570 production) and `src/camera.rs` at
3070. Both are internally coherent and unusually well-commented, so this is
genuine debt rather than a defect. Split opportunistically when touching them —
P0-3 and P1-4 both land in these files — not as a standalone refactor.

---

## Sequencing

Batched by blast radius and by shared code path, so each commit is
independently reviewable and the gate is meaningful when it runs.

| # | Commit | Items | Why grouped |
|---|---|---|---|
| 1 | ~~Fix the discovery flood test~~ *(landed 2026-07-27)* | P0-1 | Nothing below is verifiable until the gate is green |
| 2 | ~~RTSP auth hardening~~ *(landed 2026-07-27)* | P0-2, P1-3 | Same function; one security review |
| 3 | Poison-recovery sweep + `src/sync.rs` | P0-3, P3-1 | The move is a prerequisite for the sweep |
| 4 | CI: cargo-deny, MSRV job, nightly fuzz smoke | P1-1, P1-2, P1-7 | Workflow-only, no source risk |
| 5 | Live-verify marker contract | P1-4 | Finish the two camera markers + the RTSP one |
| 6 | Version literal | ~~P1-5~~ (already fixed), P1-6 | Operator-facing correctness |
| 7 | `[workspace.lints]` + doc sweep | P2-1 | Large but mechanical; gates 8 |
| 8 | `non_exhaustive`, `must_use`, `Box::pin` | P2-2, P2-3, P2-5 | Enforced by 7; one semver-review commit |
| 9 | `rand` 0.9 migration | P2-4 | Isolated, touches three crates |
| 10 | Comment debt + `RUST_LOG` test fix | P3-4, P3-5 | No behaviour change |
| 11 | Decide `CameraDriver`; error-type seam | P3-2, P3-3 | Design decisions, not cleanups |

Commits 1–3 are the near-term batch. 7–9 are semver-relevant: land them together
before the next crates.io publish rather than dribbling breaking changes across
releases.

---

## Landed

### P0-2 + P1-3: RTSP auth hardening — 2026-07-27

- Basic auth is now gated on `ConnectionState::is_tls` at both ends of
  `authenticate()`: the 401 challenge only offers `Basic` on a TLS connection,
  and a `Basic` header volunteered over plaintext is never verified — it falls
  through to the digest path, fails as an unknown scheme, and the client is
  re-challenged with Digest only. Covered at three levels: unit
  (`describe_basic_creds_over_plaintext_rechallenged_without_basic`), plaintext
  integration (`plaintext_auth_never_offers_or_accepts_basic`), and TLS
  integration (`tls_connection_offers_and_accepts_basic_auth`).
- `verify_basic` and `verify_digest` now use `subtle::ConstantTimeEq` with no
  early exit; digest computes the MD5 response for every configured user
  regardless of username match, so response time no longer distinguishes
  "user exists" from "user unknown". `subtle` was already in-tree via rustls.
- New `warn_users_without_tls` fires from both daemon startup and
  `check-config` when `[[users]]` is set without `certificate`.
- README + `docs/architecture.md` updated (both described the old
  Basic-on-plaintext behaviour as intentional).

### P1-5: was already fixed — 2026-07-27

`validate_config` (src/config.rs:1344) has rejected `permitted_users` entries
that match no `[[users]]` name since before the review, with tests
(`permitted_users_unknown_user_is_rejected`). The review was misled by the two
stale future-tense *"Task 24 will add validation"* comments, which are now
corrected (along with the adjacent corrupted `§3.3` comment from P3-4's list).

### P0-1: merge gate green again — 2026-07-27

Three separate reddening causes, all test-side; no production code changed.

- `discoverer_reader_survives_same_tid_flood` — the reader was never parked.
  Trace logging showed it healthy on the `try_send` drop path; the probe
  datagram was being dropped by the *kernel* because the 200-packet flood
  overruns the socket receive buffer (~167 × ~1.2 KB skb truesize fills a
  212992-byte `rmem_default`, the WSL2 default and cap). Fixed by retrying the
  `tid_other` probe on a 100 ms interval inside the T-bounded timeout —
  verified to still fail against a reverted `send().await` reader, so the
  regression net holds.
- `stale_uid_reads_as_unknown` (wake-server) — under paused time the fixed
  20-yield wait raced the heartbeat upsert against `advance(10s)`: the runtime
  only polls the I/O driver every ~61 task polls while a task keeps yielding,
  so the upsert landed after the advance and the entry read back fresh
  (`rsp: 0`, wake burst to a "stale" camera). Now yield-polls the registry
  handle until the heartbeat is recorded before advancing; reply wait bound
  raised 200 → 10 000 yields (arrival observed as late as 477).
- `src/log_capture.rs::await_marker` — landed unused in the P1-4 partial and
  failed `clippy -D warnings` (dead_code) on lib-test. Deleted; the remaining
  P1-4 marker tests can restore it from history when they actually call it.

Local `cargo-tarpaulin` aligned to CI's 0.37.0 pin.

### RTSP pipelined requests stalled the connection — 2026-07-26

`src/rtsp/server/connection.rs:173`. The read loop called
`try_consume_request` once per `read()`, so a second request arriving in the
same TCP segment sat in the buffer until more bytes arrived. A client that
pipelines and then blocks for all its responses hung — until the slow-loris arm
fired, or forever once a session existed and that arm was disabled. RFC 7826
§9.2 explicitly permits pipelining, and go2rtc / ffmpeg are free to use it.

Fixed with a labelled outer loop and a drain loop over the buffer, plus a
per-iteration `cancel.is_cancelled()` check — a 64 KiB buffer holds thousands of
minimal requests and `dispatch_request` is not cancel-aware, so without it a
shutdown would have to wait out the whole drain.

Regression test: `pipelined_requests_all_receive_responses` in
`tests/rtsp_integration_test.rs`, verified to fail against the
pre-fix code with `timed out awaiting pipelined responses`.

### Live-verify startup-wake marker pinned — 2026-07-26

Partial P1-4. `src/log_capture.rs` plus
`startup_wake_completion_logs_live_verify_marker` and
`startup_wake_empty_map_does_not_log_completion_marker`.

---

## Checked and clean

Recorded so the next review does not re-derive them.

- **`src/wake_lock.rs`** — acquire/release/`idle_since` interleavings traced.
  The `is_idle()` re-check in `idle_since()` correctly masks the one
  stale-timestamp window; the `notify_one` + re-check loop in
  `wait_for_acquire` handles stale permits. Comments explaining the
  `notify_one` vs `notify_waiters` choice are accurate.
- **RTSP request framing** — `try_consume_request` rejects oversize and
  overflowing `Content-Length` before arithmetic, with `checked_add`. The
  slow-loris arm re-arms correctly once sessions drain.
- **No `await`-holding-lock anywhere in the workspace.**
  `CameraHandle::stream_source` (`src/camera.rs:597`) scopes every sync guard
  tightly and uses an async mutex for the create-critical-section, with a
  documented TOCTOU re-check.
- **No unbounded channels. No `unsafe` outside the `cli_convert` test env-var
  blocks (P3-5).**
- **Config parsing** — the placement scan already covers the exact top-level
  TLS-key shape `manual-verify.sh --tls` generates
  (`tests/config_test.rs:1520`), plus the misplaced-key failure cases. The
  `#[serde(rename = "bind")]` and `topic_prefix` default the script's awk
  depends on are both covered.
- **`src/wake_server`** — every in-memory map is capped at
  `MAX_MAP_ENTRIES = 1024` with refresh-vs-insert distinguished, and the route
  cache has its own `CACHE_CAP` soft cap. Hostile-flood memory amplification is
  handled.
- **`src/mqtt`** — `parse_control_message` validates the camera name against
  an ASCII allowlist before use.
- **Crypto** — the constant IV in `src/baichuan/bc/crypto.rs` is Reolink
  firmware's constraint and is correctly documented as such; freshness comes
  from the per-session derived key.
- **Cast truncations** — the flagged wire-path casts were spot-checked.
  `src/rtsp/server/transport.rs:75` is guarded by a preceding size check;
  `bcmedia/ser.rs` length casts are bounded by protocol limits. One loose end:
  `src/rtsp/codec/aac.rs:122` does `(au.len() as u16) << 3`, silently
  wrapping the 13-bit AU-size field for frames ≥ 8192 bytes. Not reachable with
  real AAC; a `debug_assert!` would document the invariant.
