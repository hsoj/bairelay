//! Wire-format round-trip tests for BC headers + payloads.
//!
//! //!
//! Covers:
//! - Every supported MsgKind round-trips through encode + decode.
//! - XOR-encrypted payload round-trip (BCEncrypt).
//! - AES-CFB encrypted payload round-trip (Aes / FullAes).
//! - Rejection of buffers that are shorter than the 20-byte header.
//! - Rejection of headers that claim a body larger than the buffer
//!   actually contains (short body).
//! - Rejection of headers with a bad magic number.

use bytes::BytesMut;

use super::crypto::EncryptionProtocol;
use super::model::*;
use super::xml::*;

// ---------------------------------------------------------------------------
// MsgKind round-trip — encode every known MSG_ID through serialize +
// deserialize, assert meta is preserved.
// ---------------------------------------------------------------------------

fn roundtrip_modern_meta(meta: BcMeta) {
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::Unencrypted);
	let bc = Bc::new_from_meta(meta);
	let encoded = bc
		.serialize(vec![], &EncryptionProtocol::Unencrypted)
		.expect("serialize");
	let decoded = Bc::deserialize(&ctx, &mut BytesMut::from(encoded.as_slice()))
		.expect("deserialize roundtripped bc");
	assert_eq!(decoded.meta, bc.meta, "meta differs after round-trip");
	match decoded.body {
		BcBody::ModernMsg(ModernMsg {
			extension: None,
			payload: None,
		}) => {}
		other => panic!("expected empty ModernMsg, got {other:?}"),
	}
}

#[test]
fn bc_header_msg_kind_login_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_LOGIN,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 42,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_logout_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_LOGOUT,
		channel_id: 0,
		stream_type: 0,
		response_code: 200,
		msg_num: 7,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_video_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_VIDEO,
		channel_id: 1,
		stream_type: 0,
		response_code: 0,
		msg_num: 1024,
		class: 0x6414,
	});
}

#[test]
fn bc_header_msg_kind_ping_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_PING,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 1,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_reboot_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_REBOOT,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 9,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_motion_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_MOTION,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 512,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_battery_info_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_BATTERY_INFO,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 300,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_battery_info_list_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_BATTERY_INFO_LIST,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 301,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_snap_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_SNAP,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 111,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_floodlight_manual_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_FLOODLIGHT_MANUAL,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 42,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_floodlight_status_list_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_FLOODLIGHT_STATUS_LIST,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 50,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_ptz_control_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_PTZ_CONTROL,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 200,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_get_ptz_preset_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_GET_PTZ_PRESET,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 190,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_get_support_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_GET_SUPPORT,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 99,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_version_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_VERSION,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 80,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_play_audio_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_PLAY_AUDIO,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 60,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_set_general_roundtrip() {
	// set-clock
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_SET_GENERAL,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 105,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_get_led_status_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_GET_LED_STATUS,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 208,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_uid_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_UID,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 114,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_stream_info_list_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_STREAM_INFO_LIST,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 146,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_get_zoom_focus_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_GET_ZOOM_FOCUS,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 294,
		class: 0x0000,
	});
}

#[test]
fn bc_header_msg_kind_start_pir_alarm_roundtrip() {
	roundtrip_modern_meta(BcMeta {
		msg_id: MSG_ID_START_PIR_ALARM,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 213,
		class: 0x0000,
	});
}

#[test]
fn bc_header_max_values_roundtrip() {
	// Defensive test: boundary values across every header field.
	roundtrip_modern_meta(BcMeta {
		msg_id: u32::MAX,
		channel_id: u8::MAX,
		stream_type: u8::MAX,
		response_code: u16::MAX,
		msg_num: u16::MAX,
		class: 0x0000, // must be a valid class (has payload offset)
	});
}

// ---------------------------------------------------------------------------
// Payload encryption round-trips.
// ---------------------------------------------------------------------------

fn ptz_control_xml_payload() -> BcXml {
	BcXml {
		ptz_control: Some(PtzControl {
			version: "1.1".to_string(),
			channel_id: 0,
			speed: 32.0,
			command: "left".to_string(),
		}),
		..BcXml::default()
	}
}

#[test]
fn bc_payload_unencrypted_roundtrip_preserves_xml() {
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::Unencrypted);
	let meta = BcMeta {
		msg_id: MSG_ID_PTZ_CONTROL,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 1,
		class: 0x0000,
	};
	let bc = Bc::new_from_xml(meta, ptz_control_xml_payload());
	let bytes = bc
		.serialize(vec![], &EncryptionProtocol::Unencrypted)
		.unwrap();
	let decoded = Bc::deserialize(&ctx, &mut BytesMut::from(bytes.as_slice())).unwrap();
	match decoded.body {
		BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(xml)),
			..
		}) if xml.ptz_control.is_some() => {
			let pc = xml.ptz_control.expect("guard checked ptz_control");
			assert_eq!(pc.command, "left");
			assert!((pc.speed - 32.0).abs() < 0.0001);
		}
		other => panic!("expected ptz_control payload, got {other:?}"),
	}
}

#[test]
fn bc_payload_xor_encrypted_roundtrip_preserves_xml() {
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);
	let meta = BcMeta {
		msg_id: MSG_ID_PTZ_CONTROL,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 2,
		class: 0x0000,
	};
	let bc = Bc::new_from_xml(meta, ptz_control_xml_payload());
	let bytes = bc
		.serialize(vec![], &EncryptionProtocol::BCEncrypt)
		.unwrap();
	let decoded = Bc::deserialize(&ctx, &mut BytesMut::from(bytes.as_slice())).unwrap();
	match decoded.body {
		BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(xml)),
			..
		}) if xml.ptz_control.is_some() => {
			let pc = xml.ptz_control.expect("guard checked ptz_control");
			assert_eq!(pc.command, "left");
		}
		other => panic!("expected ptz_control payload, got {other:?}"),
	}
}

#[test]
fn bc_payload_aes_encrypted_roundtrip_preserves_xml() {
	// Known key (test-only) — the real key derives from the camera nonce +
	// password hash via Credentials::make_aeskey(). We don't need that
	// flow here; we only need a symmetric key / IV match between sides.
	let key: [u8; 16] = *b"bairelay_testkey";
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::aes(key));
	let meta = BcMeta {
		msg_id: MSG_ID_PTZ_CONTROL,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 3,
		class: 0x0000,
	};
	let bc = Bc::new_from_xml(meta, ptz_control_xml_payload());
	let enc = EncryptionProtocol::aes(key);
	let bytes = bc.serialize(vec![], &enc).unwrap();
	let decoded = Bc::deserialize(&ctx, &mut BytesMut::from(bytes.as_slice())).unwrap();
	match decoded.body {
		BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(xml)),
			..
		}) if xml.ptz_control.is_some() => {
			let pc = xml.ptz_control.expect("guard checked ptz_control");
			assert_eq!(pc.command, "left");
		}
		other => panic!("expected ptz_control payload, got {other:?}"),
	}
}

#[test]
fn bc_payload_full_aes_encrypted_roundtrip_preserves_xml() {
	let key: [u8; 16] = *b"bairelay_fullaes";
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::full_aes(key));
	let meta = BcMeta {
		msg_id: MSG_ID_PTZ_CONTROL,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 4,
		class: 0x0000,
	};
	let bc = Bc::new_from_xml(meta, ptz_control_xml_payload());
	let enc = EncryptionProtocol::full_aes(key);
	let bytes = bc.serialize(vec![], &enc).unwrap();
	let decoded = Bc::deserialize(&ctx, &mut BytesMut::from(bytes.as_slice())).unwrap();
	match decoded.body {
		BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(_)),
			..
		}) => {}
		other => panic!("expected BcXml payload, got {other:?}"),
	}
}

// ---------------------------------------------------------------------------
// Malformed / short-buffer rejections.
// ---------------------------------------------------------------------------

#[test]
fn bc_header_short_buffer_rejected() {
	// 20 bytes is the minimum header for a "legacy" class (no payload
	// offset). Feed 15 bytes and assert the decoder reports incomplete
	// rather than silently parsing.
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);
	let mut buf =
		BytesMut::from(&b"\xF0\xDE\xBC\x0A\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"[..]);
	let result = Bc::deserialize(&ctx, &mut buf);
	match result {
		Err(crate::baichuan::Error::NomIncomplete(_)) => {}
		other => panic!("expected NomIncomplete for 15-byte buf, got {other:?}"),
	}
}

#[test]
fn bc_header_empty_buffer_rejected() {
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);
	let mut buf = BytesMut::new();
	let result = Bc::deserialize(&ctx, &mut buf);
	match result {
		Err(crate::baichuan::Error::NomIncomplete(_)) => {}
		other => panic!("expected NomIncomplete for empty buf, got {other:?}"),
	}
}

#[test]
fn bc_header_short_body_rejected() {
	// Construct a valid header that claims body_len = 100, but provide
	// zero body bytes. Decoder must return NomIncomplete (streaming parser
	// wants more bytes), not Ok.
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::Unencrypted);
	let mut buf = BytesMut::new();
	// magic (LE 0x0abcdef0)
	buf.extend_from_slice(&0x0abcdef0u32.to_le_bytes());
	// msg_id = 1 (LOGIN)
	buf.extend_from_slice(&1u32.to_le_bytes());
	// body_len = 100
	buf.extend_from_slice(&100u32.to_le_bytes());
	// channel_id, stream_type
	buf.extend_from_slice(&[0u8, 0u8]);
	// msg_num
	buf.extend_from_slice(&0u16.to_le_bytes());
	// response_code
	buf.extend_from_slice(&0u16.to_le_bytes());
	// class = 0x0000 (modern, has payload_offset)
	buf.extend_from_slice(&0u16.to_le_bytes());
	// payload_offset = 0
	buf.extend_from_slice(&0u32.to_le_bytes());
	// ... but no body bytes. 24-byte header only.
	let result = Bc::deserialize(&ctx, &mut buf);
	match result {
		Err(crate::baichuan::Error::NomIncomplete(_)) => {}
		other => panic!("expected NomIncomplete for truncated body, got {other:?}"),
	}
}

#[test]
fn bc_header_bad_magic_rejected() {
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);
	let mut buf = BytesMut::new();
	// bad magic (not 0x0abcdef0 nor 0x0fedcba0)
	buf.extend_from_slice(&0xdeadbeefu32.to_le_bytes());
	// padding so the decoder gets past the header length check before
	// rejecting on magic
	buf.extend_from_slice(&[0u8; 24]);
	let result = Bc::deserialize(&ctx, &mut buf);
	match result {
		Err(_) => {}
		Ok(bc) => panic!("expected Err for bad magic, got Ok({bc:?})"),
	}
}

#[test]
fn bc_header_reversed_magic_accepted() {
	// 0x0fedcba0 is the "big-endian-looking" magic used on some replies
	// (documented in model.rs). It MUST be accepted.
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::Unencrypted);
	let mut buf = BytesMut::new();
	buf.extend_from_slice(&0x0fedcba0u32.to_le_bytes()); // MAGIC_HEADER_REV
	buf.extend_from_slice(&1u32.to_le_bytes()); // msg_id = LOGIN
	buf.extend_from_slice(&0u32.to_le_bytes()); // body_len = 0
	buf.extend_from_slice(&[0u8, 0u8]); // channel_id + stream_type
	buf.extend_from_slice(&0u16.to_le_bytes()); // msg_num
	buf.extend_from_slice(&0u16.to_le_bytes()); // response_code
	buf.extend_from_slice(&0u16.to_le_bytes()); // class = modern
	buf.extend_from_slice(&0u32.to_le_bytes()); // payload_offset = 0
	let bc = Bc::deserialize(&ctx, &mut buf).expect("reversed magic must parse");
	assert_eq!(bc.meta.msg_id, 1);
}

// ---------------------------------------------------------------------------
// BcHeader::is_modern / has_payload_offset tests.
// ---------------------------------------------------------------------------

#[test]
fn bc_meta_to_meta_preserves_all_fields() {
	// Ensures the BcHeader -> BcMeta conversion (via round-trip) doesn't
	// drop a field, using a non-trivial meta and asserting equality.
	let meta = BcMeta {
		msg_id: MSG_ID_BATTERY_INFO,
		channel_id: 3,
		stream_type: 1,
		response_code: 200,
		msg_num: 17,
		class: 0x0000,
	};
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::Unencrypted);
	let bc = Bc::new_from_meta(meta);
	let bytes = bc
		.serialize(vec![], &EncryptionProtocol::Unencrypted)
		.unwrap();
	let decoded = Bc::deserialize(&ctx, &mut BytesMut::from(bytes.as_slice())).unwrap();
	assert_eq!(decoded.meta.msg_id, MSG_ID_BATTERY_INFO);
	assert_eq!(decoded.meta.channel_id, 3);
	assert_eq!(decoded.meta.stream_type, 1);
	assert_eq!(decoded.meta.response_code, 200);
	assert_eq!(decoded.meta.msg_num, 17);
	assert_eq!(decoded.meta.class, 0x0000);
}

#[test]
fn bc_legacy_class_no_payload_offset_roundtrip() {
	// Class 0x6514 is the legacy class with no payload_offset byte.
	let ctx = BcContext::new_with_encryption(EncryptionProtocol::Unencrypted);
	// Can't use new_from_meta because legacy class body requires a legacy
	// msg. Instead encode manually with an empty body.
	let meta = BcMeta {
		msg_id: 9999, // unknown msg_id so we fall through to UnknownMsg
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 0,
		class: 0x6514,
	};
	let bc = Bc {
		meta,
		body: BcBody::LegacyMsg(LegacyMsg::LoginUpgrade),
	};
	let bytes = bc
		.serialize(vec![], &EncryptionProtocol::Unencrypted)
		.unwrap();
	let decoded = Bc::deserialize(&ctx, &mut BytesMut::from(bytes.as_slice())).unwrap();
	assert_eq!(decoded.meta.class, 0x6514);
	matches!(decoded.body, BcBody::LegacyMsg(_));
}

#[test]
fn bc_constructors_set_extension_and_payload_correctly() {
	let make_meta = || BcMeta {
		msg_id: 1,
		channel_id: 0,
		stream_type: 0,
		response_code: 0,
		msg_num: 0,
		class: 0x6414,
	};
	// new_from_xml: meta + xml only.
	let bc = Bc::new_from_xml(make_meta(), BcXml::default());
	match bc.body {
		BcBody::ModernMsg(ModernMsg {
			extension: None,
			payload: Some(BcPayloads::BcXml(_)),
		}) => {}
		_ => panic!("new_from_xml: wrong body shape"),
	}

	// new_from_ext: meta + extension only.
	let bc = Bc::new_from_ext(make_meta(), Extension::default());
	match bc.body {
		BcBody::ModernMsg(ModernMsg {
			extension: Some(_),
			payload: None,
		}) => {}
		_ => panic!("new_from_ext: wrong body shape"),
	}

	// new_from_ext_xml: meta + extension + xml.
	let bc = Bc::new_from_ext_xml(make_meta(), Extension::default(), BcXml::default());
	match bc.body {
		BcBody::ModernMsg(ModernMsg {
			extension: Some(_),
			payload: Some(BcPayloads::BcXml(_)),
		}) => {}
		_ => panic!("new_from_ext_xml: wrong body shape"),
	}
}

#[test]
fn bc_context_binary_mode_toggles() {
	use crate::baichuan::bc::model::BcContext;
	let mut ctx = BcContext::new_with_encryption(EncryptionProtocol::Unencrypted);
	ctx.binary_on(42);
	ctx.binary_on(43);
	ctx.binary_off(42);
	// Public API on BcContext is `pub(crate)` only; we just exercise
	// that the calls don't panic and the encryption getter mirrors what
	// we set.
	assert!(matches!(
		ctx.get_encrypted(),
		EncryptionProtocol::Unencrypted
	));
	ctx.set_encrypted(EncryptionProtocol::BCEncrypt);
	assert!(matches!(ctx.get_encrypted(), EncryptionProtocol::BCEncrypt));
	ctx.debug_on();
}
