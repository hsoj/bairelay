//! Handles sending and recieving messages as complete packets
//!
//! BcUdpCodex is used with a `[tokio_util::codec::Framed]` to form complete packets
//!
use crate::bcudp::model::*;
use crate::{Error, Result};
use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use super::xml::UdpXml;

pub(crate) struct BcUdpCodex {}

impl BcUdpCodex {
	pub(crate) fn new() -> Self {
		Self {}
	}
}

impl Encoder<BcUdp> for BcUdpCodex {
	type Error = Error;

	fn encode(&mut self, item: BcUdp, dst: &mut BytesMut) -> Result<()> {
		log::trace!("Encoding: {item:?}");
		let buf: Vec<u8> = Default::default();
		let buf = item.serialize(buf)?;
		dst.extend_from_slice(buf.as_slice());
		log::trace!("  Encoding: Done: {}", buf.len());
		Ok(())
	}
}

impl Decoder for BcUdpCodex {
	type Item = BcUdp;
	type Error = Error;

	fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
		log::trace!("Decoding:");
		if src.is_empty() {
			return Ok(None);
		}
		match BcUdp::deserialize(src) {
			Ok(BcUdp::Discovery(UdpDiscovery {
				payload: UdpXml::R2cDisc(_),
				..
			})) => {
				log::trace!("   Decoding: Relay terminate");
				Err(Error::RelayTerminate)
			}
			Ok(BcUdp::Discovery(UdpDiscovery {
				payload: UdpXml::D2cDisc(_),
				..
			})) => {
				log::trace!("   Decoding:Camera terminate");
				Err(Error::CameraTerminate)
			}
			Ok(bc) => {
				log::trace!("   Decoding: Ok");
				Ok(Some(bc))
			}
			Err(Error::NomIncomplete(_)) => {
				log::trace!("   Decoding: Incomplete: {:0X?}", src);
				Ok(None)
			}
			Err(e) => {
				log::trace!("   Decoding: Err");
				Err(e)
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::bcudp::model::{UdpAck, UdpData, UdpDiscovery};
	use crate::bcudp::xml::{C2dDisc, D2cDisc, PortList, R2cDisc, UdpXml};
	use bytes::BytesMut;
	use tokio_util::codec::{Decoder, Encoder};

	fn disc(payload: UdpXml) -> BcUdp {
		BcUdp::Discovery(UdpDiscovery { tid: 42, payload })
	}

	#[test]
	fn encode_writes_bytes() {
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::new();
		codec
			.encode(BcUdp::Ack(UdpAck::empty(7)), &mut buf)
			.expect("encode");
		assert!(!buf.is_empty());
	}

	#[test]
	fn encode_then_decode_roundtrips_ack() {
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::new();
		codec
			.encode(BcUdp::Ack(UdpAck::empty(7)), &mut buf)
			.expect("encode");
		let out = codec.decode(&mut buf).expect("decode ok").expect("some");
		match out {
			BcUdp::Ack(a) => assert_eq!(a.connection_id, 7),
			other => panic!("expected Ack, got {other:?}"),
		}
	}

	#[test]
	fn encode_then_decode_roundtrips_data() {
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::new();
		codec
			.encode(
				BcUdp::Data(UdpData {
					connection_id: 1,
					packet_id: 2,
					payload: vec![9u8; 4],
				}),
				&mut buf,
			)
			.expect("encode");
		let out = codec.decode(&mut buf).expect("decode ok").expect("some");
		assert!(matches!(out, BcUdp::Data(_)));
	}

	#[test]
	fn decode_empty_buffer_returns_none() {
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::new();
		let out = codec.decode(&mut buf).expect("decode ok");
		assert!(out.is_none(), "empty buf must return None");
	}

	#[test]
	fn decode_incomplete_returns_none() {
		let mut codec = BcUdpCodex::new();
		// Valid Ack magic but truncated body.
		let mut buf = BytesMut::new();
		buf.extend_from_slice(&0x2a87cf20u32.to_le_bytes());
		buf.extend_from_slice(&[0u8; 4]);
		let out = codec.decode(&mut buf).expect("decode ok");
		assert!(out.is_none());
	}

	#[test]
	fn decode_bad_magic_bubbles_error() {
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::from(&0xdeadbeefu32.to_le_bytes()[..]);
		let result = codec.decode(&mut buf);
		assert!(result.is_err(), "bad magic must surface an Err");
	}

	#[test]
	fn decode_r2c_disc_raises_relay_terminate() {
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::new();
		codec
			.encode(disc(UdpXml::R2cDisc(R2cDisc { sid: 12 })), &mut buf)
			.expect("encode");
		match codec.decode(&mut buf) {
			Err(Error::RelayTerminate) => {}
			other => panic!("expected RelayTerminate, got {other:?}"),
		}
	}

	#[test]
	fn decode_d2c_disc_raises_camera_terminate() {
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::new();
		codec
			.encode(disc(UdpXml::D2cDisc(D2cDisc { cid: 1, did: 2 })), &mut buf)
			.expect("encode");
		match codec.decode(&mut buf) {
			Err(Error::CameraTerminate) => {}
			other => panic!("expected CameraTerminate, got {other:?}"),
		}
	}

	#[test]
	fn decode_c2d_disc_goes_through_ok_arm() {
		// C2dDisc is Discovery but neither R2cDisc nor D2cDisc, so it
		// takes the Ok(bc) arm of the decoder match.
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::new();
		codec
			.encode(disc(UdpXml::C2dDisc(C2dDisc { cid: 1, did: 2 })), &mut buf)
			.expect("encode");
		let out = codec.decode(&mut buf).expect("decode ok").expect("some");
		match out {
			BcUdp::Discovery(d) => {
				assert!(matches!(d.payload, UdpXml::C2dDisc(_)));
			}
			other => panic!("expected Discovery, got {other:?}"),
		}
	}

	#[test]
	fn decode_c2d_s_takes_ok_arm() {
		// Another plain-Discovery variant for the generic Ok-arm.
		let mut codec = BcUdpCodex::new();
		let mut buf = BytesMut::new();
		codec
			.encode(
				disc(UdpXml::C2dS(crate::bcudp::xml::C2dS {
					to: PortList { port: 3000 },
				})),
				&mut buf,
			)
			.expect("encode");
		let out = codec.decode(&mut buf).expect("decode ok").expect("some");
		assert!(matches!(out, BcUdp::Discovery(_)));
	}
}
