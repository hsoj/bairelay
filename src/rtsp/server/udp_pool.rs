//! UDP port pool for RTSP UDP-unicast sessions.
//!
//! Each SETUP that negotiates UDP transport needs an even/odd port pair
//! (RTP + RTCP per RFC 3550 §11). The pool hands out pairs from a fixed
//! range and reclaims them on `UdpPortLease` drop.

use crate::sync::MutexPoisonRecover as _;
use std::collections::HashSet;
use std::sync::Mutex;

use thiserror::Error;

/// Start of the server's UDP port pool (inclusive).
pub const POOL_START: u16 = 40_000;

/// End of the server's UDP port pool (inclusive, even port).
pub const POOL_END: u16 = 40_998;

// RFC 3550 §11 requires the RTP port to be even. The pool hands out
// `(port, port + 1)` pairs where `port` must be even; if a future change
// makes `POOL_START` odd, every lease silently violates the RFC.
const _: () = assert!(
	POOL_START.is_multiple_of(2),
	"POOL_START must be an even RTP port (RFC 3550 §11)"
);
const _: () = assert!(
	POOL_END.is_multiple_of(2),
	"POOL_END must be an even RTP port (RFC 3550 §11)"
);
const _: () = assert!(POOL_START < POOL_END, "POOL_START must precede POOL_END");

/// Errors returned by `UdpPortPool::acquire`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PortPoolError {
	/// No even/odd pair is currently free.
	#[error("no free UDP port pair in range {POOL_START}..={POOL_END}")]
	Exhausted,
}

/// A thread-safe pool of UDP port pairs.
pub struct UdpPortPool {
	used: Mutex<HashSet<u16>>, // stores the even (RTP) port of each allocated pair
}

impl UdpPortPool {
	/// Create an empty pool over [`POOL_START`, `POOL_END`].
	pub fn new() -> Self {
		Self {
			used: Mutex::new(HashSet::new()),
		}
	}

	/// Acquire an even/odd pair. Returns a lease that frees the ports on drop.
	pub fn acquire(self: &std::sync::Arc<Self>) -> Result<UdpPortLease, PortPoolError> {
		let mut used = self.used.lock_recover();
		let mut port = POOL_START;
		while port <= POOL_END {
			if !used.contains(&port) {
				used.insert(port);
				return Ok(UdpPortLease {
					rtp_port: port,
					rtcp_port: port + 1,
					pool: std::sync::Arc::clone(self),
				});
			}
			port += 2;
		}
		Err(PortPoolError::Exhausted)
	}

	#[doc(hidden)]
	pub(crate) fn release(&self, rtp_port: u16) {
		let mut used = self.used.lock_recover();
		used.remove(&rtp_port);
	}
}

impl Default for UdpPortPool {
	fn default() -> Self {
		Self::new()
	}
}

/// An allocated even/odd port pair. Released on Drop.
pub struct UdpPortLease {
	/// Even port (RTP).
	pub rtp_port: u16,
	/// Odd port (RTCP).
	pub rtcp_port: u16,
	pool: std::sync::Arc<UdpPortPool>,
}

impl Drop for UdpPortLease {
	fn drop(&mut self) {
		self.pool.release(self.rtp_port);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Arc;

	#[test]
	fn acquires_even_odd_pairs() {
		let pool = Arc::new(UdpPortPool::new());
		let a = pool.acquire().unwrap();
		let b = pool.acquire().unwrap();
		assert!(a.rtp_port.is_multiple_of(2) && a.rtcp_port == a.rtp_port + 1);
		assert_ne!(a.rtp_port, b.rtp_port);
	}

	#[test]
	fn releases_on_drop() {
		let pool = Arc::new(UdpPortPool::new());
		let lease = pool.acquire().unwrap();
		let first_port = lease.rtp_port;
		drop(lease);
		let next = pool.acquire().unwrap();
		assert_eq!(next.rtp_port, first_port);
	}

	#[test]
	fn exhausts_when_range_full() {
		let pool = Arc::new(UdpPortPool::new());
		let mut leases = Vec::new();
		while let Ok(l) = pool.acquire() {
			leases.push(l);
		}
		// 500 pairs: 40000, 40002, ..., 40998
		assert_eq!(leases.len(), 500);
	}

	#[test]
	fn first_acquire_starts_at_pool_start() {
		let pool = Arc::new(UdpPortPool::new());
		let l = pool.acquire().unwrap();
		assert_eq!(l.rtp_port, POOL_START);
		assert_eq!(l.rtcp_port, POOL_START + 1);
	}

	#[test]
	fn default_matches_new() {
		let a: UdpPortPool = Default::default();
		let b = UdpPortPool::new();
		// Both should hand out the pool start on first call.
		let la = Arc::new(a).acquire().unwrap();
		let lb = Arc::new(b).acquire().unwrap();
		assert_eq!(la.rtp_port, lb.rtp_port);
	}
}
