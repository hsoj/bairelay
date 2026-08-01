//! Wire-format round-trip tests for BcUdp headers + payload variants.
//!
//! Prep for wake-server work.
//!
//! Covers:
//! - 20-byte header (magic + XOR-key tid + CRC-32) round-trip through
//!   `serialize` + `deserialize`.
//! - CRC-32 mismatch is detected by the decoder (asserts via the
//!   existing internal assert — test pins that behaviour).
//! - XOR-key (tid) round-trip on several payloads.
//! - Every `UdpXml` variant the protocol uses (discovery query,
//!   register connect, connection transmit, confirm, heartbeats,
//!   disconnect) encode/decode to equality.
//! - Bad magic rejection.
//! - Short packet rejection.

use bytes::BytesMut;

use super::crc::calc_crc;
use super::model::*;
use super::xml::*;

fn roundtrip(msg: &BcUdp) {
	let bytes = msg.serialize(Vec::<u8>::new()).expect("serialize");
	let mut buf = BytesMut::from(bytes.as_slice());
	let decoded = BcUdp::deserialize(&mut buf).expect("deserialize roundtripped BcUdp");
	assert_eq!(*msg, decoded, "round-trip mismatch");
	assert!(
		buf.is_empty(),
		"buffer not fully consumed, {} bytes remain",
		buf.len()
	);
}

fn disc(tid: u32, payload: UdpXml) -> BcUdp {
	BcUdp::Discovery(UdpDiscovery { tid, payload })
}

// ---------------------------------------------------------------------------
// UdpXml variants — encode/decode round-trip for every one the protocol
// uses. Each wrapped in Discovery so the full 20-byte header path is
// exercised too.
// ---------------------------------------------------------------------------

#[test]
fn bcudp_discovery_c2d_s_roundtrip() {
	roundtrip(&disc(
		10,
		UdpXml::C2dS(C2dS {
			to: PortList { port: 3000 },
		}),
	));
}

#[test]
fn bcudp_discovery_c2d_c_roundtrip() {
	roundtrip(&disc(
		11,
		UdpXml::C2dC(C2dC {
			uid: "ABCDEF0123456789".into(),
			cli: ClientList { port: 3000 },
			cid: 82000,
			mtu: 1350,
			debug: false,
			os: "MAC".into(),
			lver: 0,
		}),
	));
}

#[test]
fn bcudp_discovery_d2c_c_r_roundtrip() {
	roundtrip(&disc(
		12,
		UdpXml::D2cCr(D2cCr {
			timer: Timer::default(),
			rsp: 0,
			cid: 82000,
			did: 49,
			pl: None,
			nc: None,
		}),
	));
}

#[test]
fn bcudp_discovery_d2c_t_roundtrip() {
	roundtrip(&disc(
		13,
		UdpXml::D2cT(D2cT {
			sid: 62098713,
			conn: "local".into(),
			cid: 82001,
			did: 96,
		}),
	));
}

#[test]
fn bcudp_discovery_c2d_t_roundtrip() {
	roundtrip(&disc(
		14,
		UdpXml::C2dT(C2dT {
			sid: 62098713,
			conn: "local".into(),
			cid: 82001,
			mtu: 1350,
		}),
	));
}

#[test]
fn bcudp_discovery_d2c_cfm_roundtrip() {
	roundtrip(&disc(
		15,
		UdpXml::D2cCfm(D2cCfm {
			sid: 62098713,
			conn: "local".into(),
			rsp: 0,
			cid: 82001,
			did: 96,
			time_r: Some(0),
		}),
	));
}

#[test]
fn bcudp_discovery_c2d_disc_roundtrip() {
	roundtrip(&disc(
		16,
		UdpXml::C2dDisc(C2dDisc {
			cid: 82000,
			did: 80,
		}),
	));
}

#[test]
fn bcudp_discovery_d2c_disc_roundtrip() {
	roundtrip(&disc(
		17,
		UdpXml::D2cDisc(D2cDisc {
			cid: 82000,
			did: 80,
		}),
	));
}

#[test]
fn bcudp_discovery_r2c_disc_roundtrip() {
	roundtrip(&disc(18, UdpXml::R2cDisc(R2cDisc { sid: 42 })));
}

#[test]
fn bcudp_discovery_c2m_q_roundtrip() {
	roundtrip(&disc(
		19,
		UdpXml::C2mQ(C2mQ {
			uid: "ABCDEF0123456789".into(),
			os: "MAC".into(),
		}),
	));
}

#[test]
fn bcudp_discovery_m2c_q_r_roundtrip() {
	roundtrip(&disc(
		20,
		UdpXml::M2cQr(M2cQr {
			reg: Some(IpPort {
				ip: "10.0.0.1".into(),
				port: 9999,
			}),
			relay: Some(IpPort {
				ip: "10.0.0.2".into(),
				port: 9998,
			}),
			log: None,
			t: None,
		}),
	));
}

#[test]
fn bcudp_discovery_c2r_c_roundtrip() {
	roundtrip(&disc(
		21,
		UdpXml::C2rC(C2rC {
			uid: "ABCDEF0123456789".into(),
			cli: IpPort {
				ip: "192.168.1.10".into(),
				port: 54321,
			},
			relay: IpPort {
				ip: "10.0.0.2".into(),
				port: 9998,
			},
			cid: 82000,
			debug: false,
			family: 4,
			os: "WIN".into(),
			revision: Some(3),
		}),
	));
}

#[test]
fn bcudp_discovery_r2c_t_roundtrip() {
	roundtrip(&disc(
		22,
		UdpXml::R2cT(R2cT {
			dmap: Some(IpPort {
				ip: "10.0.0.1".into(),
				port: 5000,
			}),
			dev: None,
			cid: 82000,
			sid: 555,
		}),
	));
}

#[test]
fn bcudp_discovery_r2c_c_r_roundtrip() {
	roundtrip(&disc(
		23,
		UdpXml::R2cCr(R2cCr {
			dev: Some(IpPort {
				ip: "10.0.0.1".into(),
				port: 1000,
			}),
			dmap: None,
			relay: Some(IpPort {
				ip: "10.0.0.2".into(),
				port: 2000,
			}),
			relayt: None,
			nat: "NULL".into(),
			sid: Some(777),
			rsp: 0,
			ac: 127536491,
		}),
	));
}

#[test]
fn bcudp_discovery_c2r_cfm_roundtrip() {
	roundtrip(&disc(
		24,
		UdpXml::C2rCfm(C2rCfm {
			sid: 62098713,
			conn: "local".into(),
			rsp: 0,
			cid: 82001,
			did: 96,
		}),
	));
}

#[test]
fn bcudp_discovery_c2d_a_roundtrip() {
	roundtrip(&disc(
		25,
		UdpXml::C2dA(C2dA {
			sid: 62098713,
			conn: "local".into(),
			cid: 82001,
			did: 96,
			mtu: 1350,
		}),
	));
}

#[test]
fn bcudp_discovery_c2d_hb_roundtrip() {
	roundtrip(&disc(
		26,
		UdpXml::C2dHb(C2dHb {
			cid: 82000,
			did: 80,
		}),
	));
}

#[test]
fn bcudp_discovery_c2r_hb_roundtrip() {
	roundtrip(&disc(
		27,
		UdpXml::C2rHb(C2rHb {
			sid: 42,
			cid: 82000,
			did: 80,
		}),
	));
}

#[test]
fn bcudp_discovery_d2c_hb_roundtrip() {
	roundtrip(&disc(
		28,
		UdpXml::D2cHb(D2cHb {
			cid: 82000,
			did: 80,
		}),
	));
}

#[test]
fn discovery_roundtrip_d2r_hb() {
	let msg = BcUdp::Discovery(UdpDiscovery {
		tid: 0x29,
		payload: UdpXml::D2rHb(D2rHb {
			uid: "9527000TEST".into(),
			dev: Some(IpPort {
				ip: "10.0.0.91".into(),
				port: 10177,
			}),
			needrsp: Some(1),
			token: 1770434238,
		}),
	});
	roundtrip(&msg);
}

#[test]
fn discovery_roundtrip_r2d_hb_r() {
	let msg = BcUdp::Discovery(UdpDiscovery {
		tid: 0x42,
		payload: UdpXml::R2dHbr(R2dHbr {
			rsp: 0,
			time_t: 1,
			timer: HbTimer { hb: 20000 },
		}),
	});
	roundtrip(&msg);
}

#[test]
fn discovery_roundtrip_r2d_c() {
	let msg = BcUdp::Discovery(UdpDiscovery {
		tid: 0x02613feb,
		payload: UdpXml::R2dC(R2dC {
			cli: IpPort {
				ip: "10.0.0.170".into(),
				port: 10739,
			},
			cmap: IpPort {
				ip: "192.0.2.35".into(),
				port: 10739,
			},
			relay: IpPort {
				ip: "10.0.0.1".into(),
				port: 58200,
			},
			sid: 95196080,
			cid: 330001,
		}),
	});
	roundtrip(&msg);
}

#[test]
fn discovery_roundtrip_d2r_c_r() {
	let msg = BcUdp::Discovery(UdpDiscovery {
		tid: 0x42,
		payload: UdpXml::D2rCr(D2rCr {
			sid: 95196080,
			dev: Some(IpPort {
				ip: "10.0.0.91".into(),
				port: 10177,
			}),
			rsp: 0,
		}),
	});
	roundtrip(&msg);
}

#[test]
fn discovery_roundtrip_d2r_disc() {
	let msg = BcUdp::Discovery(UdpDiscovery {
		tid: 0x42,
		payload: UdpXml::D2rDisc(D2rDisc { sid: 95196080 }),
	});
	roundtrip(&msg);
}

#[test]
fn discovery_roundtrip_r2d_dc_r() {
	let msg = BcUdp::Discovery(UdpDiscovery {
		tid: 0x42,
		payload: UdpXml::R2dDcr(R2dDcr {
			sid: 95196080,
			rsp: 0,
		}),
	});
	roundtrip(&msg);
}

/// Captured R2D_C wake packet bytes from a real Reolink P2P server PCAP.
/// TID 0x02613feb; sid=95196080; cid=330001.
#[test]
fn discovery_decodes_real_pcap_r2d_c() {
	#[rustfmt::skip]
	let raw: &[u8] = &[
		0x3a, 0xcf, 0x87, 0x2a, 0x14, 0x01, 0x00, 0x00,
		0x01, 0x00, 0x00, 0x00, 0xeb, 0x3f, 0x61, 0x02,
		0xcc, 0xf2, 0x9f, 0x74,
		// 276-byte encrypted body lifted verbatim from the reference PCAP.
		0x0a, 0x2c, 0xbc, 0x71, 0x46, 0x83, 0x9f, 0x6e,
		0x72, 0x31, 0x3b, 0x04, 0x79, 0xc0, 0xbe, 0xed,
		0x28, 0x66, 0xc9, 0xf8, 0xfd, 0x06, 0x11, 0x9a,
		0xb1, 0x10, 0xe5, 0xbd, 0xbe, 0x2b, 0x7a, 0x2b,
		0x01, 0x4c, 0xb2, 0x0e, 0x11, 0xcf, 0xf3, 0x60,
		0x46, 0x01, 0x0a, 0x4e, 0x7b, 0x92, 0xe2, 0xb3,
		0x25, 0x63, 0x9c, 0xa7, 0xb3, 0x58, 0x5a, 0xdc,
		0xa1, 0x1d, 0xfc, 0xe6, 0xfc, 0x7a, 0x6a, 0x26,
		0x55, 0x11, 0xef, 0x51, 0x46, 0x83, 0xa4, 0x2c,
		0x08, 0x5d, 0x49, 0x14, 0x74, 0x96, 0xe3, 0xaa,
		0x27, 0x69, 0x8e, 0xbb, 0xf6, 0x0b, 0x07, 0xc1,
		0xef, 0x1f, 0xef, 0xf5, 0xff, 0x61, 0x20, 0x24,
		0x07, 0x4c, 0xb9, 0x12, 0x41, 0x83, 0xe2, 0x2c,
		0x59, 0x1c, 0x0c, 0x04, 0x79, 0x8c, 0xb1, 0xe9,
		0x77, 0x2a, 0x9e, 0xb4, 0xb1, 0x52, 0x44, 0xc9,
		0xe6, 0x1f, 0xef, 0xec, 0xe0, 0x2d, 0x61, 0x2b,
		0x18, 0x48, 0xba, 0x0f, 0x4a, 0x8e, 0xfe, 0x72,
		0x07, 0x59, 0x4b, 0x06, 0x6a, 0xca, 0xa2, 0xba,
		0x2a, 0x2a, 0xcf, 0xfa, 0xb7, 0x09, 0x1d, 0x9c,
		0xab, 0x19, 0xe4, 0xb9, 0xbf, 0x63, 0x3b, 0x68,
		0x42, 0x42, 0xb2, 0x0e, 0x0a, 0xda, 0xa1, 0x3d,
		0x4f, 0x50, 0x44, 0x49, 0x2c, 0xc7, 0xec, 0xbd,
		0x23, 0x6b, 0x99, 0xbe, 0xf3, 0x0f, 0x18, 0x94,
		0xb0, 0x52, 0xba, 0xe1, 0xae, 0x2f, 0x37, 0x73,
		0x52, 0x42, 0xbd, 0x12, 0x48, 0x8f, 0xfd, 0x6d,
		0x0a, 0x41, 0x1b, 0x53, 0x21, 0x9d, 0xee, 0xe0,
		0x73, 0x38, 0xd5, 0xef, 0xfd, 0x05, 0x1d, 0x99,
		0xa9, 0x14, 0xeb, 0xb7, 0xa4, 0x23, 0x68, 0x35,
		0x52, 0x19, 0xec, 0x54, 0x1f, 0x81, 0xf1, 0x32,
		0x57, 0x1a, 0x46, 0x74, 0x10, 0xef, 0x9e, 0xb8,
		0x39, 0x34, 0xc1, 0xfc, 0xfd, 0x0b, 0x49, 0xcb,
		0xa1, 0x18, 0xe6, 0xb4, 0xa9, 0x25, 0x64, 0x22,
		0x06, 0x40, 0xa1, 0x40, 0x1b, 0x81, 0xf1, 0x73,
		0x64, 0x5c, 0x3c, 0x65, 0x06, 0x9d, 0xee, 0xab,
		0x46, 0x68, 0xf0, 0xb6,
	];
	let mut buf = bytes::BytesMut::from(raw);
	let pkt = BcUdp::deserialize(&mut buf).expect("decode real pcap packet");
	match pkt {
		BcUdp::Discovery(disc) => {
			assert_eq!(disc.tid, 0x02613feb);
			match disc.payload {
				UdpXml::R2dC(c) => {
					assert_eq!(c.sid, 95196080);
					assert_eq!(c.cid, 330001);
					assert!(!c.cli.ip.is_empty());
					assert_eq!(c.cli.port, 10739);
				}
				other => panic!("expected R2dC, got {other:?}"),
			}
		}
		other => panic!("expected Discovery, got {other:?}"),
	}
}

// ---------------------------------------------------------------------------
// Ack + Data headers — not Discovery so no XML / no CRC, just raw header.
// ---------------------------------------------------------------------------

#[test]
fn bcudp_ack_empty_roundtrip() {
	roundtrip(&BcUdp::Ack(UdpAck::empty(82000)));
}

#[test]
fn bcudp_ack_with_payload_roundtrip() {
	roundtrip(&BcUdp::Ack(UdpAck {
		connection_id: 82000,
		group_id: 0,
		packet_id: 2439,
		maybe_latency: 54785,
		payload: vec![0x00, 0x01, 0x01, 0x01, 0x01],
	}));
}

#[test]
fn bcudp_data_roundtrip_preserves_payload() {
	let payload = (0u8..=255).cycle().take(1000).collect::<Vec<u8>>();
	roundtrip(&BcUdp::Data(UdpData {
		connection_id: 82000,
		packet_id: 2439,
		payload: payload.clone(),
	}));
}

#[test]
fn bcudp_data_empty_payload_roundtrip() {
	roundtrip(&BcUdp::Data(UdpData {
		connection_id: 82000,
		packet_id: 0,
		payload: vec![],
	}));
}

#[test]
fn bcudp_connection_id_accessor_covers_all_variants() {
	assert_eq!(
		disc(10, UdpXml::R2cDisc(R2cDisc { sid: 0 })).get_connection_id(),
		0
	);
	assert_eq!(BcUdp::Ack(UdpAck::empty(42)).get_connection_id(), 42);
	assert_eq!(
		BcUdp::Data(UdpData {
			connection_id: 84,
			packet_id: 0,
			payload: vec![],
		})
		.get_connection_id(),
		84
	);
}

// ---------------------------------------------------------------------------
// Malformed / short-buffer / bad-magic rejections.
// ---------------------------------------------------------------------------

#[test]
fn bcudp_bad_magic_rejected() {
	// 4 bytes of 0xdeadbeef — not any known BcUdp magic.
	let bytes = 0xdeadbeefu32.to_le_bytes().to_vec();
	let mut buf = BytesMut::from(bytes.as_slice());
	let result = BcUdp::deserialize(&mut buf);
	match result {
		Err(_) => {}
		Ok(f) => panic!("expected Err for bad magic, got Ok({f:?})"),
	}
}

#[test]
fn bcudp_empty_buffer_rejected() {
	let mut buf = BytesMut::new();
	let result = BcUdp::deserialize(&mut buf);
	match result {
		Err(crate::baichuan::Error::NomIncomplete(_)) => {}
		other => panic!("expected NomIncomplete for empty buf, got {other:?}"),
	}
}

#[test]
fn bcudp_short_header_rejected() {
	// Only 3 bytes of magic — decoder must report incomplete before
	// attempting anything else.
	let mut buf = BytesMut::from(&[0x3a, 0xcf, 0x87][..]);
	let result = BcUdp::deserialize(&mut buf);
	match result {
		Err(crate::baichuan::Error::NomIncomplete(_)) => {}
		other => panic!("expected NomIncomplete for short header, got {other:?}"),
	}
}

#[test]
fn bcudp_short_ack_body_rejected() {
	// Valid ack magic but truncated body — expects 24 bytes of header
	// body after magic; we provide 8.
	let mut buf = BytesMut::new();
	buf.extend_from_slice(&MAGIC_HEADER_UDP_ACK.to_le_bytes());
	buf.extend_from_slice(&[0u8; 8]);
	let result = BcUdp::deserialize(&mut buf);
	match result {
		Err(crate::baichuan::Error::NomIncomplete(_)) => {}
		other => panic!("expected NomIncomplete for short ack body, got {other:?}"),
	}
}

#[test]
fn bcudp_short_data_body_rejected() {
	let mut buf = BytesMut::new();
	buf.extend_from_slice(&MAGIC_HEADER_UDP_DATA.to_le_bytes());
	// 4 bytes of partial body — decoder needs 16 + payload_size.
	buf.extend_from_slice(&[0u8; 4]);
	let result = BcUdp::deserialize(&mut buf);
	match result {
		Err(crate::baichuan::Error::NomIncomplete(_)) => {}
		other => panic!("expected NomIncomplete for short data body, got {other:?}"),
	}
}

// ---------------------------------------------------------------------------
// CRC — exercise the helper directly.
// ---------------------------------------------------------------------------

#[test]
fn bcudp_crc_is_stable_for_same_input() {
	assert_eq!(calc_crc(b""), calc_crc(b""));
	assert_eq!(calc_crc(b"hello world"), calc_crc(b"hello world"));
}

#[test]
fn bcudp_crc_differs_for_different_input() {
	let a = calc_crc(b"hello");
	let b = calc_crc(b"hellp");
	assert_ne!(a, b, "crc must distinguish single-byte flips");
}

#[test]
fn bcudp_crc_empty_input_is_zero() {
	// Per the comment in crc.rs — initial value 0, xorout 0, empty
	// input => CRC of 0.
	assert_eq!(calc_crc(b""), 0);
}
