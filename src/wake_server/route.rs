//! Resolve which local IP to *advertise* to a remote peer.
//!
//! Live-verify against a real Argus caught a protocol bug: when the
//! wake server is bound to `0.0.0.0` (the bairelay default), populating the `<reg>`
//! and `<log>` fields of an outgoing `M2D_Q_R` with the literal bind
//! address tells the camera "register at `0.0.0.0:58200`" — which the
//! firmware silently rejects, looping on `D2M_Q` forever without ever
//! moving on to `D2R_HB`.
//!
//! The fix is to ask the kernel which local IP it would use to reach the
//! peer that just sent us a packet. UDP `connect()` is the standard
//! trick — it performs no I/O, just configures the socket so that
//! `local_addr()` reports the chosen source IP for that destination.
//! Cheap, sync, and works across IPv4 and IPv6.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Mutex, OnceLock};

/// Maximum number of `(bind, peer_ip)` cache entries. The wake server
/// only ever resolves one entry per camera (and only when bound to
/// `0.0.0.0`), so a handful is the realistic working set; the cap
/// just bounds memory under a hostile-flood scenario.
const CACHE_CAP: usize = 256;

/// Cached `(bind, peer_ip) → advertised_ip` resolutions.
///
/// `advertise_ip` is called once per outbound BcUdp packet from both
/// the middleman and register hot paths. Each call previously did a
/// full `UdpSocket::bind` + `connect` + `local_addr` round-trip — three
/// syscalls per packet, all under a blocking std API in an async
/// context. For a stable LAN where the kernel's route table doesn't
/// shift between packets, the answer is constant per `(bind, peer_ip)`.
///
/// Soft cap — past [`CACHE_CAP`] entries, new keys still resolve
/// freshly but skip the cache write so a memory-exhaustion flood can't
/// grow this map without bound.
fn cache() -> &'static Mutex<HashMap<(IpAddr, IpAddr), IpAddr>> {
	static CACHE: OnceLock<Mutex<HashMap<(IpAddr, IpAddr), IpAddr>>> = OnceLock::new();
	CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pick the IP address to advertise to `peer` in outbound BcUdp payloads.
///
/// - If `bind` is a concrete address (not `0.0.0.0` / `[::]`), use it
///   verbatim — the operator wants every peer to see exactly that
///   address.
/// - Otherwise, ask the kernel via `UdpSocket::connect` which local IP
///   it would use to send to `peer`. Falls back to `127.0.0.1` (IPv4)
///   or `::1` (IPv6) if the probe fails — better something than
///   `0.0.0.0`.
///
/// Results are cached per `(bind, peer.ip())`; the per-packet hot path
/// no longer re-does the syscalls. Restart bairelay to pick up route-
/// table changes (interface up/down, default-route flip).
///
/// The fallback path emits a `tracing::warn!` so a misconfigured
/// host (no route to peer, IPv6-only host receiving IPv4 packet,
/// ENETUNREACH, etc.) doesn't silently advertise loopback to a real
/// LAN camera — without it this was diagnosable only by packet capture.
pub fn advertise_ip(bind: IpAddr, peer: SocketAddr) -> IpAddr {
	if !bind.is_unspecified() {
		return bind;
	}
	let key = (bind, peer.ip());
	if let Some(cached) = cache_lookup(&key) {
		return cached;
	}
	let resolved = resolve_advertise_ip(bind, peer);
	cache_insert(key, resolved);
	resolved
}

fn cache_lookup(key: &(IpAddr, IpAddr)) -> Option<IpAddr> {
	cache()
		.lock()
		.unwrap_or_else(|p| p.into_inner())
		.get(key)
		.copied()
}

fn cache_insert(key: (IpAddr, IpAddr), value: IpAddr) {
	let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
	if guard.len() >= CACHE_CAP && !guard.contains_key(&key) {
		// Soft cap: stop growing once we hit the cap; the legitimate
		// fleet fits inside CACHE_CAP many times over, so this only
		// fires under a hostile spray of unique peer IPs.
		return;
	}
	guard.insert(key, value);
}

/// Test-only: drop the cache so a fresh fixture starts clean.
#[cfg(test)]
fn clear_cache_for_tests() {
	cache().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

fn resolve_advertise_ip(bind: IpAddr, peer: SocketAddr) -> IpAddr {
	let probe_bind: SocketAddr = match peer.ip() {
		IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
		IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
	};
	let fallback = if peer.is_ipv4() {
		IpAddr::V4(Ipv4Addr::LOCALHOST)
	} else {
		IpAddr::V6(Ipv6Addr::LOCALHOST)
	};
	let _ = bind; // already consumed by caller for the unspecified-check
	match std::net::UdpSocket::bind(probe_bind) {
		Ok(probe) => match probe.connect(peer) {
			Ok(()) => match probe.local_addr() {
				Ok(a) => a.ip(),
				Err(e) => {
					tracing::warn!(
						%peer, %fallback, error = %e,
						"advertise_ip: local_addr() failed; falling back to loopback (camera will likely fail to reach us)",
					);
					fallback
				}
			},
			Err(e) => {
				tracing::warn!(
					%peer, %fallback, error = %e,
					"advertise_ip: probe-socket connect() failed; falling back to loopback (camera will likely fail to reach us)",
				);
				fallback
			}
		},
		Err(e) => {
			tracing::warn!(
				%peer, %fallback, error = %e,
				"advertise_ip: probe-socket bind() failed; falling back to loopback (camera will likely fail to reach us)",
			);
			fallback
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Tests share a process-global cache, so any pair that touches
	/// the cache races under `cargo test`'s default thread-per-test
	/// scheduling. Serialize via this lock — cheap, no extra dep.
	static TEST_LOCK: Mutex<()> = Mutex::new(());

	fn lock_and_clear() -> std::sync::MutexGuard<'static, ()> {
		let g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
		clear_cache_for_tests();
		g
	}

	#[test]
	fn concrete_bind_is_returned_as_is() {
		let _g = lock_and_clear();
		let bind: IpAddr = "192.168.1.50".parse().unwrap();
		let peer: SocketAddr = "203.0.113.10:9999".parse().unwrap();
		assert_eq!(advertise_ip(bind, peer), bind);
	}

	#[test]
	fn loopback_peer_resolves_to_loopback() {
		let _g = lock_and_clear();
		let bind: IpAddr = Ipv4Addr::UNSPECIFIED.into();
		let peer: SocketAddr = "127.0.0.1:9999".parse().unwrap();
		let ip = advertise_ip(bind, peer);
		// On every modern OS, the local IP for a loopback peer is also
		// loopback — we just want to confirm the function does not return
		// 0.0.0.0.
		assert!(!ip.is_unspecified(), "advertise_ip returned wildcard");
		assert!(ip.is_loopback(), "expected loopback, got {ip}");
	}

	#[test]
	fn ipv6_loopback_peer_resolves_to_v6_loopback() {
		let _g = lock_and_clear();
		let bind: IpAddr = Ipv6Addr::UNSPECIFIED.into();
		let peer: SocketAddr = "[::1]:9999".parse().unwrap();
		let ip = advertise_ip(bind, peer);
		assert!(!ip.is_unspecified(), "advertise_ip returned ::");
		assert!(ip.is_loopback(), "expected v6 loopback, got {ip}");
	}

	/// Cache hit: a second `advertise_ip` for the same (bind, peer.ip())
	/// must not reach the resolver. We can't directly observe the
	/// syscall count without instrumentation, so we verify the cache by
	/// pre-poisoning it with a sentinel and asserting it survives.
	#[test]
	fn cache_hit_bypasses_resolver() {
		let _g = lock_and_clear();
		let bind: IpAddr = Ipv4Addr::UNSPECIFIED.into();
		let peer_ip: IpAddr = "127.0.0.1".parse().unwrap();
		let sentinel: IpAddr = "10.99.99.99".parse().unwrap();
		cache_insert((bind, peer_ip), sentinel);
		let peer: SocketAddr = "127.0.0.1:9999".parse().unwrap();
		assert_eq!(
			advertise_ip(bind, peer),
			sentinel,
			"second call must hit the cache, not re-resolve"
		);
	}

	/// Soft cap: once the cache hits CACHE_CAP, new keys still resolve
	/// but the cache stops growing — defends against a hostile-flood
	/// memory exhaustion vector.
	#[test]
	fn cache_soft_cap_stops_growth_under_pressure() {
		let _g = lock_and_clear();
		// Stuff the cache up to the cap with distinct IPv4 keys. Use
		// a /16-spread so up to 65 536 indexes produce 65 536 distinct
		// addrs; const_assert that we stay within that budget.
		const _: () = assert!(CACHE_CAP <= 65_536, "test address spread assumes ≤ /16");
		let bind: IpAddr = Ipv4Addr::UNSPECIFIED.into();
		for i in 0..CACHE_CAP {
			let peer_ip = IpAddr::V4(Ipv4Addr::new(
				10,
				((i >> 8) & 0xff) as u8,
				(i & 0xff) as u8,
				42,
			));
			cache_insert((bind, peer_ip), peer_ip);
		}
		let len_before = cache().lock().unwrap_or_else(|p| p.into_inner()).len();
		assert_eq!(len_before, CACHE_CAP);
		// Inserting a new key past the cap must be a no-op.
		cache_insert(
			(bind, IpAddr::V4(Ipv4Addr::new(11, 0, 0, 1))),
			IpAddr::V4(Ipv4Addr::new(11, 0, 0, 1)),
		);
		let len_after = cache().lock().unwrap_or_else(|p| p.into_inner()).len();
		assert_eq!(len_after, CACHE_CAP, "soft cap must hold");
	}
}
