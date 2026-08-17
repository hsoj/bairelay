//! This is a helper module to resolve either to a UID or a SockerAddr

use serde::{Deserialize, Serialize};
use std::{
	io::Error,
	net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs},
};

/// Reolink "short form" UID — exactly 16 chars, digits + uppercase ASCII
/// letters. The 20-char "long form" the firmware reports in
/// `D2R_HB` / `D2M_Q` / `D2R_R` is the short form plus a 4-char firmware
/// suffix; that variant never reaches `to_socket_addrs_or_uid`, which
/// only sees operator-supplied (short-form) UIDs from `config.toml`.
/// See `docs/cloud-interception.md` § I.6.2.
fn is_uid_shape(s: &str) -> bool {
	s.len() == 16
		&& s.bytes()
			.all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

/// Convert an operator-typed string into a `SocketAddrOrUid::Uid` if it
/// matches the documented short-form UID shape, else propagate the
/// caller's `to_socket_addrs` error. Centralises the shape test that
/// `&str` and `String` both ran inline before — DRY across both impls.
fn classify_uid_or_propagate(
	s: &str,
	dns_err: Error,
) -> Result<std::vec::IntoIter<SocketAddrOrUid>, Error> {
	if is_uid_shape(s) {
		Ok(vec![SocketAddrOrUid::Uid(
			s.to_string(),
			None,
			DiscoveryMethods::Local,
		)]
		.into_iter())
	} else {
		Err(dns_err)
	}
}

/// Select permitted discovery methods
///
/// This is used for UID lookup, it is unused with
/// TPC/known ip address cameras
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiscoveryMethods {
	/// Forbid all discovery methods. Only TCP connections with known addresses will work
	#[serde(alias = "none")]
	None,
	/// Allow local discovery on the local network using broadcasts
	/// This method does NOT contact reolink servers
	#[serde(alias = "local")]
	Local,
	/// Allow contact with the reolink servers to learn the ip address but DO NOT
	/// allow the camera/clinet to communicate through the reolink servers.
	///
	/// **This also enabled `Local` discovery**
	#[serde(alias = "remote")]
	Remote,
	/// Allow contact with the reolink servers to learn the ip address and map the connection
	/// from dev to client through those servers.
	///
	/// **This also enabled `Local` and `Remote` discovery**
	#[serde(alias = "map")]
	Map,
	/// Allow contact with the reolink servers to learn the ip address and relay the connection
	/// client to dev through those servers.
	///
	/// **This also enabled `Local`, `Map` and `Remote` discovery**
	#[serde(alias = "relay")]
	Relay,
	/// Cellular camera only support relay and map, by choosing this option
	/// only those are tried
	#[serde(alias = "cellular")]
	Cellular,
	/// Account ("cloud") camera. The camera is bound to a Reolink account and
	/// only accepts the sigV3 login with a cloud-minted token. Enables the same
	/// discovery reach as [`Relay`](Self::Relay) (local + remote + map + relay)
	/// and advertises `lver=3` so the camera issues the sigV3 handshake.
	/// Requires top-level `cloud_account` / `cloud_password`.
	#[serde(alias = "cloud")]
	Cloud,
	#[doc(hidden)]
	#[serde(alias = "debug")]
	/// Used for debugging it is set to whatever the dev is currently testing
	Debug,
}

/// Used to return either the SocketAddr or the UID
pub enum SocketAddrOrUid {
	/// When the result is a addr it will be this
	SocketAddr(SocketAddr),
	/// When the result is a UID
	Uid(String, Option<Vec<SocketAddr>>, DiscoveryMethods),
}

/// An extension of ToSocketAddrs that will also resolve to a camera UID
pub trait ToSocketAddrsOrUid: ToSocketAddrs {
	/// The return type of the function
	type UidIter: Iterator<Item = SocketAddrOrUid>;

	/// This handles the actual resolution. It should first check the
	/// normal [.to_socket_addrs()] and if that fails it should check
	/// if it looks like a uid
	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error>;
}

impl ToSocketAddrsOrUid for SocketAddr {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl ToSocketAddrsOrUid for str {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		match self.to_socket_addrs() {
			Ok(addrs) => Ok(addrs
				.map(SocketAddrOrUid::SocketAddr)
				.collect::<Vec<_>>()
				.into_iter()),
			Err(e) => classify_uid_or_propagate(self, e),
		}
	}
}

impl ToSocketAddrsOrUid for String {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		match self.to_socket_addrs() {
			Ok(addrs) => Ok(addrs
				.map(SocketAddrOrUid::SocketAddr)
				.collect::<Vec<_>>()
				.into_iter()),
			Err(e) => classify_uid_or_propagate(self, e),
		}
	}
}

impl ToSocketAddrsOrUid for (&str, u16) {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl ToSocketAddrsOrUid for (IpAddr, u16) {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl ToSocketAddrsOrUid for (String, u16) {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl ToSocketAddrsOrUid for (Ipv4Addr, u16) {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl ToSocketAddrsOrUid for (Ipv6Addr, u16) {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl ToSocketAddrsOrUid for SocketAddrV4 {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl ToSocketAddrsOrUid for SocketAddrV6 {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl ToSocketAddrsOrUid for &'_ [SocketAddr] {
	type UidIter = std::vec::IntoIter<SocketAddrOrUid>;

	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		Ok(self
			.to_socket_addrs()?
			.map(SocketAddrOrUid::SocketAddr)
			.collect::<Vec<_>>()
			.into_iter())
	}
}

impl<T: ToSocketAddrsOrUid + ?Sized> ToSocketAddrsOrUid for &T {
	type UidIter = T::UidIter;
	fn to_socket_addrs_or_uid(&self) -> Result<Self::UidIter, Error> {
		(**self).to_socket_addrs_or_uid()
	}
}

/// Flags telling `find_camera` which UID-discovery steps the configured
/// [`DiscoveryMethods`] value permits. Each field gates one branch of
/// `find_camera`'s `tokio::select!` fallback chain
/// (local UDP broadcast → remote P2P → map → relay).
///
/// Extracted as a pure function so the table is coverable in unit
/// tests without standing up the real UDP sockets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiscoveryFlags {
	pub local: bool,
	pub remote: bool,
	pub map: bool,
	pub relay: bool,
}

/// Translate [`DiscoveryMethods`] into the (local, remote, map, relay)
/// enable flags used by `BcCamera::find_camera`. The mapping follows
/// neolink's precedent: each level includes every cheaper level, except
/// `Cellular` which skips local/remote (cellular cameras are never on
/// the local subnet and P2P-remote rarely works through carrier NAT).
pub(crate) fn discovery_flags_for(method: DiscoveryMethods) -> DiscoveryFlags {
	match method {
		DiscoveryMethods::None => DiscoveryFlags {
			local: false,
			remote: false,
			map: false,
			relay: false,
		},
		DiscoveryMethods::Local => DiscoveryFlags {
			local: true,
			remote: false,
			map: false,
			relay: false,
		},
		DiscoveryMethods::Remote => DiscoveryFlags {
			local: true,
			remote: true,
			map: false,
			relay: false,
		},
		DiscoveryMethods::Map => DiscoveryFlags {
			local: true,
			remote: true,
			map: true,
			relay: false,
		},
		DiscoveryMethods::Relay | DiscoveryMethods::Cloud => DiscoveryFlags {
			local: true,
			remote: true,
			map: true,
			relay: true,
		},
		DiscoveryMethods::Cellular => DiscoveryFlags {
			local: false,
			remote: false,
			map: true,
			relay: true,
		},
		DiscoveryMethods::Debug => DiscoveryFlags {
			local: false,
			remote: false,
			map: true,
			relay: false,
		},
	}
}

#[cfg(test)]
mod to_socket_addrs_or_uid_tests {
	//! Covers the `ToSocketAddrsOrUid` impl blocks. Each variant is a
	//! thin shim over `to_socket_addrs()` or a UID-alphanumeric check —
	//! these tests pin the happy + UID-fallback + reject-with-separator
	//! branches so we don't lose coverage when adding new variants.
	use super::*;

	fn take_addrs<I: Iterator<Item = SocketAddrOrUid>>(iter: I) -> Vec<SocketAddrOrUid> {
		iter.collect()
	}

	fn is_socket(item: &SocketAddrOrUid) -> bool {
		matches!(item, SocketAddrOrUid::SocketAddr(_))
	}

	fn is_uid(item: &SocketAddrOrUid) -> bool {
		matches!(item, SocketAddrOrUid::Uid(_, _, _))
	}

	#[test]
	fn sockaddr_direct_resolves_socket() {
		let sa: SocketAddr = "127.0.0.1:9000".parse().unwrap();
		let v = take_addrs(sa.to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn str_socketaddr_resolves_socket() {
		let v = take_addrs("127.0.0.1:9000".to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn str_uid_alphanumeric_resolves_uid() {
		// Pure alphanum isn't a valid host:port → regex path fires.
		let v = take_addrs("ABCDEF0123456789".to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_uid(&v[0]));
		if let SocketAddrOrUid::Uid(uid, addrs, method) = &v[0] {
			assert_eq!(uid, "ABCDEF0123456789");
			assert!(addrs.is_none());
			assert_eq!(*method, DiscoveryMethods::Local);
		}
	}

	#[test]
	fn str_real_argus_uid_shape_resolves() {
		// Argus UID shape: digits + uppercase letters, exactly 16 chars
		// (16-char short form per docs/cloud-interception.md § I.6.2).
		// The previous `[0-9A-Za-z]+` matched this too, but the new
		// tighter regex must keep matching it — guards against
		// accidental over-tightening.
		let v = take_addrs("9527000XXXXXXXXX".to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_uid(&v[0]));
	}

	#[test]
	fn str_short_alphanumeric_is_no_longer_a_uid() {
		// Used to be accepted by `[0-9A-Za-z]+` (any length); now
		// rejected — real Reolink UIDs are exactly 16 chars and
		// accepting "ABCDEF" hides operator typos as discovery work.
		assert!("ABCDEF".to_socket_addrs_or_uid().is_err());
	}

	#[test]
	fn str_lowercase_alphanumeric_is_no_longer_a_uid() {
		// Lowercase 16-char input would have matched the old regex.
		// Real UIDs are uppercase; refuse early so a lowercase typo
		// surfaces as a usage error instead of a 30 s discovery
		// timeout.
		assert!("abcdef0123456789".to_socket_addrs_or_uid().is_err());
	}

	#[test]
	fn str_seventeen_char_alphanumeric_is_rejected() {
		// One past the 16-char short form — neither short nor long
		// form (long form is 20 chars) — operator typo.
		assert!("ABCDEF01234567890".to_socket_addrs_or_uid().is_err());
	}

	#[test]
	fn str_invalid_returns_err() {
		// Contains `:` but not a port — neither a valid SocketAddr nor
		// a valid alphanumeric UID.
		let err = "not-a-host-or-uid:abc".to_socket_addrs_or_uid();
		assert!(err.is_err());
	}

	#[test]
	fn string_socketaddr_resolves_socket() {
		let s = "127.0.0.1:9000".to_string();
		let v = take_addrs(s.to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn string_uid_alphanumeric_resolves_uid() {
		// 16-char uppercase alphanumeric — the documented short-form
		// shape. The previous test used "CAMUID1234" (10 chars), which
		// the old regex accepted but the tightened one correctly does
		// not.
		let s = "CAMUID0123456789".to_string();
		let v = take_addrs(s.to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_uid(&v[0]));
	}

	#[test]
	fn string_invalid_returns_err() {
		// Dash isn't alphanumeric under the regex and not a valid host.
		let s = "nope-not-valid:x".to_string();
		assert!(s.to_socket_addrs_or_uid().is_err());
	}

	#[test]
	fn tuple_str_port_resolves() {
		let v = take_addrs(("127.0.0.1", 9000u16).to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn tuple_ipaddr_port_resolves() {
		let ip: IpAddr = "127.0.0.1".parse().unwrap();
		let v = take_addrs((ip, 9000u16).to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn tuple_string_port_resolves() {
		let v = take_addrs(
			("127.0.0.1".to_string(), 9000u16)
				.to_socket_addrs_or_uid()
				.unwrap(),
		);
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn tuple_ipv4_port_resolves() {
		let ip: Ipv4Addr = "127.0.0.1".parse().unwrap();
		let v = take_addrs((ip, 9000u16).to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn tuple_ipv6_port_resolves() {
		let ip: Ipv6Addr = "::1".parse().unwrap();
		let v = take_addrs((ip, 9000u16).to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn sockaddrv4_resolves() {
		let sa: SocketAddrV4 = "127.0.0.1:9000".parse().unwrap();
		let v = take_addrs(sa.to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn sockaddrv6_resolves() {
		let sa: SocketAddrV6 = "[::1]:9000".parse().unwrap();
		let v = take_addrs(sa.to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
		assert!(is_socket(&v[0]));
	}

	#[test]
	fn slice_of_sockaddrs_resolves() {
		let addrs: Vec<SocketAddr> = vec![
			"127.0.0.1:9000".parse().unwrap(),
			"127.0.0.1:9001".parse().unwrap(),
		];
		let slice: &[SocketAddr] = &addrs[..];
		let v = take_addrs(slice.to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 2);
		assert!(v.iter().all(is_socket));
	}

	#[test]
	fn ref_delegates_to_inner_impl() {
		// `impl<T> for &T` delegates through the Deref — covers the
		// trampoline in the blanket impl.
		let s: String = "127.0.0.1:9000".to_string();
		let by_ref: &String = &s;
		let v = take_addrs(by_ref.to_socket_addrs_or_uid().unwrap());
		assert_eq!(v.len(), 1);
	}
}

#[cfg(test)]
mod discovery_flags_tests {
	use super::*;

	#[test]
	fn none_disables_everything() {
		let f = discovery_flags_for(DiscoveryMethods::None);
		assert!(!f.local && !f.remote && !f.map && !f.relay);
	}

	#[test]
	fn local_enables_only_local() {
		let f = discovery_flags_for(DiscoveryMethods::Local);
		assert!(f.local);
		assert!(!f.remote && !f.map && !f.relay);
	}

	#[test]
	fn remote_enables_local_and_remote() {
		let f = discovery_flags_for(DiscoveryMethods::Remote);
		assert!(f.local && f.remote);
		assert!(!f.map && !f.relay);
	}

	#[test]
	fn map_enables_local_remote_map() {
		let f = discovery_flags_for(DiscoveryMethods::Map);
		assert!(f.local && f.remote && f.map);
		assert!(!f.relay);
	}

	#[test]
	fn relay_enables_all_four() {
		let f = discovery_flags_for(DiscoveryMethods::Relay);
		assert!(f.local && f.remote && f.map && f.relay);
	}

	#[test]
	fn cellular_skips_local_and_remote() {
		// Cellular cameras aren't reachable via subnet broadcast and
		// rarely traverse carrier NAT for direct P2P — only map/relay
		// are useful.
		let f = discovery_flags_for(DiscoveryMethods::Cellular);
		assert!(!f.local && !f.remote);
		assert!(f.map && f.relay);
	}

	#[test]
	fn debug_enables_only_map() {
		// Debug is a dev-only knob that sidesteps other paths while
		// exercising the map code path in isolation.
		let f = discovery_flags_for(DiscoveryMethods::Debug);
		assert!(!f.local && !f.remote && !f.relay);
		assert!(f.map);
	}

	#[test]
	fn level_inclusivity_monotonic_through_relay() {
		// Each level in the None→Local→Remote→Map→Relay ladder must be
		// a strict superset of the previous. This invariant is what
		// lets operators pick the least-privileged method that still
		// reaches their camera.
		let ladder = [
			DiscoveryMethods::None,
			DiscoveryMethods::Local,
			DiscoveryMethods::Remote,
			DiscoveryMethods::Map,
			DiscoveryMethods::Relay,
		];
		for pair in ladder.windows(2) {
			let lower = discovery_flags_for(pair[0]);
			let upper = discovery_flags_for(pair[1]);
			assert!(
				(!lower.local || upper.local)
					&& (!lower.remote || upper.remote)
					&& (!lower.map || upper.map)
					&& (!lower.relay || upper.relay),
				"monotonicity broken between {:?} and {:?}",
				pair[0],
				pair[1],
			);
		}
	}
}
