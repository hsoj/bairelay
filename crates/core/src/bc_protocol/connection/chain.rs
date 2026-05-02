//! Pure, injectable model of the UID-discovery fallback chain in
//! `BcCamera::find_camera`.
//!
//! The real `find_camera` runs a `tokio::select!` with four branches —
//! local UDP broadcast, remote P2P, map, relay — gated by enable flags
//! derived from [`DiscoveryMethods`]. Each branch short-circuits the
//! chain on success and propagates `Err(DiscoveryTimeout)` on failure.
//!
//! We can't instantiate that select! in a unit test without real
//! sockets, but we CAN extract the branch-selection + short-circuit
//! semantics into a generic helper that takes four async closures and
//! a [`DiscoveryFlags`] struct. Stage 6 uses this helper to cover the
//! fallback chain, per-step timeouts, and the cellular skip-local path.
//!
//! The helper deliberately runs the branches sequentially (not in
//! parallel), mirroring the effect of `find_camera`'s `if allow_X`
//! guards: disabled branches are skipped, enabled branches are tried
//! in order, and the first `Ok` short-circuits. That differs from the
//! production `tokio::select!` (which races branches in parallel), but
//! for the first-success-wins guarantee the sequential model is
//! behaviourally equivalent and dramatically easier to reason about
//! under paused virtual time.

use super::super::resolution::DiscoveryFlags;
use crate::{Error, Result};
use std::future::Future;
use std::time::Duration;
use tokio::time::timeout;

/// Names the four UID-discovery steps so callers / assertions can tell
/// which branch produced the final outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscoveryStep {
	Local,
	Remote,
	Map,
	Relay,
}

/// Drive the four UID-discovery steps in order, honouring the
/// `DiscoveryFlags` gate and short-circuiting on the first success.
/// Each step is wrapped in `tokio::time::timeout(per_step_timeout)`.
///
/// Returns the winning step and its payload on first success, or
/// [`Error::DiscoveryTimeout`] if every enabled step failed / timed
/// out (or every step was disabled).
///
/// Semantics the unit tests pin down:
///
/// 1. First success short-circuits — later closures are never awaited.
/// 2. All four disabled → `Err(DiscoveryTimeout)` immediately.
/// 3. All four enabled but every step fails → `Err(DiscoveryTimeout)`.
/// 4. Per-step timeout fires → treated as a step failure, chain moves
///    on to the next enabled step.
/// 5. Cellular flags (local=false, remote=false, map=true, relay=true)
///    skip the first two closures entirely.
pub(crate) async fn try_discovery_chain<T, FLocal, FRemote, FMap, FRelay>(
	flags: DiscoveryFlags,
	per_step_timeout: Duration,
	local: FLocal,
	remote: FRemote,
	map: FMap,
	relay: FRelay,
) -> Result<(DiscoveryStep, T)>
where
	FLocal: Future<Output = Result<T>>,
	FRemote: Future<Output = Result<T>>,
	FMap: Future<Output = Result<T>>,
	FRelay: Future<Output = Result<T>>,
{
	// Hold each closure behind `Option` so we can `take()` it into the
	// timeout, avoiding a move-out issue when later branches are
	// skipped. This also lets us drop untaken futures cleanly.
	let mut local = Some(local);
	let mut remote = Some(remote);
	let mut map = Some(map);
	let mut relay = Some(relay);

	if flags.local {
		if let Some(fut) = local.take() {
			if let Ok(Ok(v)) = timeout(per_step_timeout, fut).await {
				return Ok((DiscoveryStep::Local, v));
			}
		}
	}
	if flags.remote {
		if let Some(fut) = remote.take() {
			if let Ok(Ok(v)) = timeout(per_step_timeout, fut).await {
				return Ok((DiscoveryStep::Remote, v));
			}
		}
	}
	if flags.map {
		if let Some(fut) = map.take() {
			if let Ok(Ok(v)) = timeout(per_step_timeout, fut).await {
				return Ok((DiscoveryStep::Map, v));
			}
		}
	}
	if flags.relay {
		if let Some(fut) = relay.take() {
			if let Ok(Ok(v)) = timeout(per_step_timeout, fut).await {
				return Ok((DiscoveryStep::Relay, v));
			}
		}
	}

	Err(Error::DiscoveryTimeout)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::bc_protocol::resolution::{discovery_flags_for, DiscoveryMethods};
	use std::sync::atomic::{AtomicU8, Ordering};
	use std::sync::Arc;
	use tokio::time::Duration as TkDuration;

	// Helper: AtomicU8 bitmask tracking which branches actually ran.
	const BIT_LOCAL: u8 = 1 << 0;
	const BIT_REMOTE: u8 = 1 << 1;
	const BIT_MAP: u8 = 1 << 2;
	const BIT_RELAY: u8 = 1 << 3;

	fn mark(bits: &Arc<AtomicU8>, bit: u8) {
		bits.fetch_or(bit, Ordering::SeqCst);
	}

	#[tokio::test]
	async fn local_success_short_circuits_later_steps() {
		let bits = Arc::new(AtomicU8::new(0));
		let b = bits.clone();
		let result = try_discovery_chain(
			discovery_flags_for(DiscoveryMethods::Relay),
			Duration::from_secs(1),
			async {
				mark(&b, BIT_LOCAL);
				Ok::<u32, Error>(42)
			},
			async {
				mark(&bits.clone(), BIT_REMOTE);
				Err::<u32, _>(Error::DiscoveryTimeout)
			},
			async {
				mark(&bits.clone(), BIT_MAP);
				Err::<u32, _>(Error::DiscoveryTimeout)
			},
			async {
				mark(&bits.clone(), BIT_RELAY);
				Err::<u32, _>(Error::DiscoveryTimeout)
			},
		)
		.await
		.expect("chain should succeed at Local");
		assert_eq!(result, (DiscoveryStep::Local, 42));
		let observed = bits.load(Ordering::SeqCst);
		assert_eq!(
			observed, BIT_LOCAL,
			"only Local should have run, got bitmask {:08b}",
			observed
		);
	}

	#[tokio::test]
	async fn local_fail_falls_through_to_remote() {
		let bits = Arc::new(AtomicU8::new(0));
		let b1 = bits.clone();
		let b2 = bits.clone();
		let b3 = bits.clone();
		let b4 = bits.clone();
		let result = try_discovery_chain(
			discovery_flags_for(DiscoveryMethods::Relay),
			Duration::from_secs(1),
			async move {
				mark(&b1, BIT_LOCAL);
				Err::<u32, _>(Error::DiscoveryTimeout)
			},
			async move {
				mark(&b2, BIT_REMOTE);
				Ok::<u32, Error>(7)
			},
			async move {
				mark(&b3, BIT_MAP);
				Ok::<u32, Error>(99)
			},
			async move {
				mark(&b4, BIT_RELAY);
				Ok::<u32, Error>(100)
			},
		)
		.await
		.expect("chain should succeed at Remote");
		assert_eq!(result, (DiscoveryStep::Remote, 7));
		let observed = bits.load(Ordering::SeqCst);
		assert_eq!(observed, BIT_LOCAL | BIT_REMOTE);
	}

	#[tokio::test]
	async fn all_four_fail_returns_discovery_timeout() {
		let result = try_discovery_chain::<u32, _, _, _, _>(
			discovery_flags_for(DiscoveryMethods::Relay),
			Duration::from_secs(1),
			async { Err(Error::DiscoveryTimeout) },
			async { Err(Error::DiscoveryTimeout) },
			async { Err(Error::DiscoveryTimeout) },
			async { Err(Error::DiscoveryTimeout) },
		)
		.await;
		assert!(
			matches!(result, Err(Error::DiscoveryTimeout)),
			"expected DiscoveryTimeout, got {:?}",
			result
		);
	}

	#[tokio::test]
	async fn all_disabled_returns_discovery_timeout() {
		// None flags → chain skips every branch, error out.
		let bits = Arc::new(AtomicU8::new(0));
		let b1 = bits.clone();
		let b2 = bits.clone();
		let b3 = bits.clone();
		let b4 = bits.clone();
		let result = try_discovery_chain::<u32, _, _, _, _>(
			discovery_flags_for(DiscoveryMethods::None),
			Duration::from_secs(1),
			async move {
				mark(&b1, BIT_LOCAL);
				Ok(1)
			},
			async move {
				mark(&b2, BIT_REMOTE);
				Ok(1)
			},
			async move {
				mark(&b3, BIT_MAP);
				Ok(1)
			},
			async move {
				mark(&b4, BIT_RELAY);
				Ok(1)
			},
		)
		.await;
		assert!(matches!(result, Err(Error::DiscoveryTimeout)));
		assert_eq!(
			bits.load(Ordering::SeqCst),
			0,
			"no branch should have run when every flag is false"
		);
	}

	#[tokio::test(flavor = "current_thread", start_paused = true)]
	async fn per_step_timeout_treats_hang_as_failure() {
		// Local hangs forever; per-step timeout fires at 500 ms and the
		// chain moves on to Remote which answers immediately. Verified
		// under paused virtual time so the test itself never actually
		// sleeps.
		let result = try_discovery_chain(
			discovery_flags_for(DiscoveryMethods::Relay),
			Duration::from_millis(500),
			async {
				// Sleep longer than the per-step timeout.
				tokio::time::sleep(TkDuration::from_secs(10)).await;
				Ok::<u32, Error>(1)
			},
			async { Ok::<u32, Error>(2) },
			async { Ok::<u32, Error>(3) },
			async { Ok::<u32, Error>(4) },
		)
		.await
		.expect("should fall through to Remote after Local times out");
		assert_eq!(result, (DiscoveryStep::Remote, 2));
	}

	#[tokio::test]
	async fn cellular_flags_skip_local_and_remote() {
		// Cellular (map+relay only) must not invoke the local/remote
		// closures even if they would succeed.
		let bits = Arc::new(AtomicU8::new(0));
		let b1 = bits.clone();
		let b2 = bits.clone();
		let b3 = bits.clone();
		let b4 = bits.clone();
		let result = try_discovery_chain(
			discovery_flags_for(DiscoveryMethods::Cellular),
			Duration::from_secs(1),
			async move {
				mark(&b1, BIT_LOCAL);
				Ok::<u32, Error>(1)
			},
			async move {
				mark(&b2, BIT_REMOTE);
				Ok::<u32, Error>(2)
			},
			async move {
				mark(&b3, BIT_MAP);
				Ok::<u32, Error>(3)
			},
			async move {
				mark(&b4, BIT_RELAY);
				Ok::<u32, Error>(4)
			},
		)
		.await
		.expect("map should win under Cellular");
		assert_eq!(result, (DiscoveryStep::Map, 3));
		let observed = bits.load(Ordering::SeqCst);
		assert_eq!(
			observed, BIT_MAP,
			"cellular must skip local/remote, got {:08b}",
			observed
		);
	}

	#[tokio::test]
	async fn cellular_falls_through_map_to_relay() {
		// Cellular with map failing should still reach relay and
		// succeed there.
		let result = try_discovery_chain(
			discovery_flags_for(DiscoveryMethods::Cellular),
			Duration::from_secs(1),
			async { Ok::<u32, Error>(1) },
			async { Ok::<u32, Error>(2) },
			async { Err(Error::DiscoveryTimeout) },
			async { Ok::<u32, Error>(4) },
		)
		.await
		.expect("relay should win when cellular map fails");
		assert_eq!(result, (DiscoveryStep::Relay, 4));
	}
}
