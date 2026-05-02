#![warn(unused_crate_dependencies)]
#![warn(missing_docs)]
//! # Neolink-Core
//!
//! Neolink-Core is a rust library for interacting with reolink and family cameras.
//!
//! Most high level camera controls are in the [`bc_protocol`] module
//!
//! A camera can be initialised with
//!
//! ```no_run
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! use neolink_core::bc_protocol::{BcCamera, BcCameraOpt, DiscoveryMethods, ConnectionProtocol, Credentials};
//! let options = BcCameraOpt {
//!     name: "CamName".to_string(),
//!     channel_id: 0,
//!     addrs: ["192.168.1.1".parse().unwrap()].to_vec(),
//!     port: Some(9000),
//!     uid: Some("CAMUID".to_string()),
//!     protocol: ConnectionProtocol::TcpUdp,
//!     discovery: DiscoveryMethods::Relay,
//!     credentials: Credentials {
//!         username: "username".to_string(),
//!         password: Some("password".to_string()),
//!     },
//!     debug: false,
//!     max_discovery_retries: 10,
//! };
//! let mut camera = BcCamera::new(&options).await.unwrap();
//! # })
//! ```
//!
//! After that login can be conducted with
//!
//! ```no_run
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! # use neolink_core::bc_protocol::{BcCamera, BcCameraOpt, DiscoveryMethods, ConnectionProtocol, Credentials};
//! # let options = BcCameraOpt {
//! #    name: "CamName".to_string(),
//! #    channel_id: 0,
//! #    addrs: ["192.168.1.1".parse().unwrap()].to_vec(),
//! #    port: Some(9000),
//! #    uid: Some("CAMUID".to_string()),
//! #    protocol: ConnectionProtocol::TcpUdp,
//! #    discovery: DiscoveryMethods::Relay,
//! #    credentials: Credentials {
//! #        username: "username".to_string(),
//! #        password: Some("password".to_string()),
//! #    },
//! #    debug: false,
//! #    max_discovery_retries: 10,
//! # };
//! # let mut camera = BcCamera::new(&options).await.unwrap();
//! camera.login().await;
//! # })
//! ```
//! For further commands see the [`bc_protocol::BcCamera`] struct.
//!

/// Contains low level BC structures and formats
pub mod bc;
/// Contains high level interfaces for the camera
pub mod bc_protocol;
/// Contains low level structures and formats for the media substream
pub mod bcmedia;
///  Contains low level structures and formats for the udpstream
pub mod bcudp;

/// This is the top level error structure of the library
///
/// Most commands will either return their `Ok(result)` or this `Err(Error)`
pub use bc_protocol::Error;

pub(crate) use bc_protocol::{Credentials, Result};

pub(crate) type NomErrorType<'a> = nom::error::VerboseError<&'a [u8]>;

/// Thin public shims around otherwise `pub(crate)` parsers so the
/// out-of-tree fuzz harness can drive them. Gated on the `fuzz-api`
/// Cargo feature.
#[cfg(feature = "fuzz-api")]
pub mod fuzz_api {
	use bytes::BytesMut;

	/// Drive `Bc::deserialize` on arbitrary input under the
	/// `Unencrypted` codec context.
	pub fn parse_bc(input: &[u8]) -> Result<crate::bc::model::Bc, crate::Error> {
		let ctx = crate::bc::model::BcContext::new_with_encryption(
			crate::bc::crypto::EncryptionProtocol::Unencrypted,
		);
		let mut buf = BytesMut::from(input);
		crate::bc::model::Bc::deserialize(&ctx, &mut buf)
	}

	/// Drive `BcXml::try_parse` on arbitrary input.
	pub fn parse_bc_xml(input: &[u8]) -> Result<crate::bc::xml::BcXml, quick_xml::de::DeError> {
		crate::bc::xml::BcXml::try_parse(input)
	}

	pub use crate::bc_protocol::connection::udpsource::{UdpFlowState, REORDER_CAP};
	pub use crate::bcudp::model::{UdpAck, UdpData};

	/// Drive a sequence of `UdpFlowState` operations parsed from
	/// arbitrary bytes. Asserts the bounded-state invariants
	/// (REORDER_CAP cap on `received`, no panic on u32::MAX corners)
	/// survive every input.
	///
	/// Encoding: each op consumes 1 tag byte + payload:
	///
	/// - tag % 4 == 0: handle_data — 5 bytes (4 = packet_id LE, 1 =
	///   payload-length-cap byte). The synthetic UdpData payload is
	///   filled with the tag value to keep the fuzz corpus dense.
	/// - tag % 4 == 1: handle_ack — 5 bytes (4 = ack.packet_id LE,
	///   1 = ack.payload length, capped at remaining input).
	/// - tag % 4 == 2: enqueue_send — 1 byte (length of synthetic
	///   payload, capped at remaining input).
	/// - tag % 4 == 3: drain_contiguous + build_send_ack — no params.
	///
	/// Input length is clamped to 8 KiB so the harness drives at most
	/// ~1 k operations per iteration — `sent` is unbounded by design
	/// (the camera-side ack stream shrinks it in production but the
	/// fuzzer doesn't model that), so a multi-MB input would OOM the
	/// process with synthetic enqueue_send growth that has nothing to
	/// do with real bug-finding.
	pub fn flow_state_drive_arbitrary(data: &[u8]) {
		let data = &data[..data.len().min(8 * 1024)];
		let mut s = UdpFlowState::new(0xABCD, 0x1234);
		let mut p = 0;
		while p < data.len() {
			let tag = data[p];
			p += 1;
			match tag % 4 {
				0 => {
					if data.len() - p < 5 {
						break;
					}
					let packet_id =
						u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]);
					let payload_len = data[p + 4] as usize;
					p += 5;
					let _ = s.handle_data(UdpData {
						connection_id: 0xABCD,
						packet_id,
						payload: vec![tag; payload_len.min(64)],
					});
				}
				1 => {
					if data.len() - p < 5 {
						break;
					}
					let pkt = u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]);
					let plen = data[p + 4] as usize;
					p += 5;
					let payload_len = plen.min(data.len().saturating_sub(p));
					let payload: Vec<u8> = data[p..p + payload_len].to_vec();
					p += payload_len;
					s.handle_ack(UdpAck {
						connection_id: 0x1234,
						packet_id: pkt,
						group_id: 0,
						maybe_latency: 0,
						payload,
					});
				}
				2 => {
					if data.len() - p < 1 {
						break;
					}
					let n = data[p] as usize;
					p += 1;
					let take = n.min(data.len().saturating_sub(p)).min(4096);
					let _ = s.enqueue_send(&data[p..p + take]);
					p += take;
				}
				_ => {
					let _ = s.drain_contiguous();
					let _ = s.build_send_ack();
				}
			}
			// Bounded-state invariants — these are the load-bearing
			// claims the production code makes. Crash → fuzzer flag.
			assert!(s.received_len() <= REORDER_CAP);
		}
	}
}
