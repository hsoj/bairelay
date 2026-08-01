//! Create a tokio encoder/decoder for turning a AsyncRead/Write stream into
//! a Bc packet
//!
//! BcCodex is used with a `[tokio_util::codec::Framed]` to form complete packets
//!
use crate::baichuan::bc::model::*;
use crate::baichuan::bc::xml::*;
use crate::baichuan::{Credentials, Error, Result};
use bytes::BytesMut;
use nom::AsBytes;
use tokio_util::codec::{Decoder, Encoder};

pub(crate) struct BcCodex {
	context: BcContext,
	/// sigV3 only: the signed login's `(msg_num, key, iv)`, captured while
	/// encoding it. Applied — switching the session from BCEncrypt to FullAes
	/// (control commands AND the media stream are AES, the latter only its
	/// leading `encryptLen` bytes) — when THIS login's `200` reply is decoded
	/// (matched by `msg_num`; that reply itself is still BCEncrypt). Disarmed on
	/// any reply to that `msg_num`, so a rejected login never leaves it armed.
	/// `None` on every other path.
	pending_session_aes: Option<(u16, [u8; 16], [u8; 16])>,
}

impl BcCodex {
	pub(crate) fn new_with_debug(credentials: Credentials) -> Self {
		let mut context = BcContext::new(credentials);

		context.debug_on();
		Self {
			context,
			pending_session_aes: None,
		}
	}
	pub(crate) fn new(credentials: Credentials) -> Self {
		Self {
			context: BcContext::new(credentials),
			pending_session_aes: None,
		}
	}
}

impl Encoder<Bc> for BcCodex {
	type Error = Error;

	fn encode(&mut self, item: Bc, dst: &mut BytesMut) -> Result<()> {
		// let context = self.context.read().unwrap();
		const BC_ENCRYPTED: EncryptionProtocol = EncryptionProtocol::BCEncrypt;
		let buf: Vec<u8> = Default::default();
		// The sigV3 direct login (cloud-bound cameras) is sent with no prior
		// Bc Encryption negotiation: the nonce + ECDHE arrive in the P2P
		// handshake instead. That login — and the camera's DeviceInfo reply —
		// travel under BCEncrypt (confirmed on the wire). Detect the signed
		// login (a LoginUser carrying `publicKey`) and pin the context to
		// BCEncrypt so both this encode AND the reply decode use it.
		let signed_login_aes = match &item.body {
			BcBody::ModernMsg(ModernMsg {
				payload: Some(BcPayloads::BcXml(BcXml {
					login_user: Some(lu),
					..
				})),
				..
			}) if lu.public_key.is_some() => Some(lu.session_aes),
			_ => None,
		};
		if let Some(session_aes) = signed_login_aes {
			self.context.set_encrypted(EncryptionProtocol::BCEncrypt);
			// Arm the post-login switch, tagged with this login's msg_num so it
			// applies only to THIS login's reply. Applied once that 200 decodes.
			self.pending_session_aes = session_aes.map(|(k, iv)| (item.meta.msg_num, k, iv));
		}
		// The modern login (msg_id 1) is always sent under BCEncrypt even when
		// AES is negotiated — including the sigV3 / authLogin signed logins.
		// The official app's `setXmlEncryptVersion(2)` only re-keys the
		// *post-login* session; the login message itself stays BCEncrypt
		// (confirmed on the wire against a cloud-bound camera).
		let enc_protocol: &EncryptionProtocol = match self.context.get_encrypted() {
			EncryptionProtocol::Aes { .. } | EncryptionProtocol::FullAes { .. }
				if item.meta.msg_id == 1 =>
			{
				// During login the encyption protocol cannot go higher than BCEncrypt
				// even if we support AES. (BUt it can go lower i.e. None)
				&BC_ENCRYPTED
			}
			n => n,
		};
		let buf = item.serialize(buf, enc_protocol)?;
		dst.extend_from_slice(buf.as_slice());
		Ok(())
	}
}

impl Decoder for BcCodex {
	type Item = Bc;
	type Error = Error;

	fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>> {
		match self.decode(buf)? {
			Some(frame) => Ok(Some(frame)),
			None => {
				if buf.is_empty() {
					Ok(None)
				} else {
					tracing::debug!(
						"bytes remaining on BC stream: {:X?}",
						buf.as_bytes().chunks(25).next()
					);
					Ok(None)
				}
			}
		}
	}

	fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
		// trace!("Decoding: {:X?}", src);
		let bc = Bc::deserialize(&self.context, src);
		// trace!("As: {:?}", bc);
		let bc = match bc {
			Ok(bc) => bc,
			Err(Error::NomIncomplete(_)) => return Ok(None),
			Err(e) => return Err(e),
		};
		// sigV3: the camera accepted the signed login (msg 1, code 200). That
		// reply itself arrived under BCEncrypt; every message after it on the
		// control AND media are AES keyed by the ECDHE-derived (key, iv).
		// Apply the FullAes switch armed while encoding the login.
		if let Some((msg_num, key, iv)) = self.pending_session_aes {
			if bc.meta.msg_id == 1 && bc.meta.msg_num == msg_num {
				if bc.meta.response_code == 200 {
					self.context
						.set_encrypted(EncryptionProtocol::full_aes_with_iv(key, iv));
				}
				// Resolved (accepted or rejected) — disarm either way.
				self.pending_session_aes = None;
			}
		}
		// Update context
		if let Bc {
			meta: BcMeta {
				msg_id: 1,
				response_code,
				..
			},
			body:
				BcBody::ModernMsg(ModernMsg {
					payload:
						Some(BcPayloads::BcXml(BcXml {
							encryption: Some(Encryption { nonce, .. }),
							..
						})),
					..
				}),
		} = &bc
		{
			if response_code >> 8 == 0xdd {
				// Login reply has the encryption info
				// Set that the encryption type now
				let encryption_protocol_byte = (response_code & 0xff) as usize;
				match encryption_protocol_byte {
					0x00 => self.context.set_encrypted(EncryptionProtocol::Unencrypted),
					0x01 => self.context.set_encrypted(EncryptionProtocol::BCEncrypt),
					0x02 => self.context.set_encrypted(EncryptionProtocol::aes(
						self.context.credentials.make_aeskey(nonce),
					)),
					0x12 => self.context.set_encrypted(EncryptionProtocol::full_aes(
						self.context.credentials.make_aeskey(nonce),
					)),
					_ => {
						return Err(Error::UnknownEncryption(encryption_protocol_byte));
					}
				}
			}
		}

		if let BcBody::ModernMsg(ModernMsg {
			extension: Some(Extension {
				binary_data: Some(on_off),
				..
			}),
			..
		}) = bc.body
		{
			if on_off == 0 {
				self.context.binary_off(bc.meta.msg_num);
			} else {
				self.context.binary_on(bc.meta.msg_num);
			}
		}

		Ok(Some(bc))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn empty_creds() -> Credentials {
		// Anonymous-login shape: empty username, no password. Replaces
		// the earlier `Credentials::default()` which baked in the
		// Reolink factory password — that impl is gone, see
		// `crates/core/src/bc_protocol/credentials.rs`.
		Credentials::new("", None::<String>)
	}

	fn meta_login(response_code: u16) -> BcMeta {
		BcMeta {
			msg_id: 1,
			channel_id: 0,
			stream_type: 0,
			response_code,
			msg_num: 0,
			class: 0x6614,
		}
	}

	#[test]
	fn new_constructs_codex_without_debug() {
		let _codex = BcCodex::new(empty_creds());
	}

	#[test]
	fn new_with_debug_constructs_codex_with_debug_on() {
		let _codex = BcCodex::new_with_debug(empty_creds());
	}

	#[test]
	fn encode_then_decode_modern_login_round_trip() {
		// Encode + decode in one frame, drive the encode side's BCEncrypt
		// substitution + the decode side's normal path.
		let mut codex = BcCodex::new(empty_creds());
		let bc = Bc::new_from_xml(
			meta_login(0x6611),
			BcXml {
				encryption: Some(Encryption {
					version: "1.1".to_string(),
					nonce: "AB".to_string(),
					type_: "md5".to_string(),
					..Default::default()
				}),
				..Default::default()
			},
		);

		let mut dst = BytesMut::new();
		codex.encode(bc, &mut dst).expect("encode ok");
		let got = codex.decode(&mut dst).expect("decode ok");
		assert!(got.is_some(), "decode produced a frame");
	}

	#[test]
	fn decode_login_response_negotiates_unencrypted_protocol() {
		let mut codex = BcCodex::new(empty_creds());
		let reply = Bc::new_from_xml(
			meta_login(0xdd00),
			BcXml {
				encryption: Some(Encryption {
					version: "1.1".to_string(),
					nonce: "AB".to_string(),
					type_: "none".to_string(),
					..Default::default()
				}),
				..Default::default()
			},
		);
		// 0xdd00 → decoder treats payload as Unencrypted, so encode the
		// same way to match.
		let bytes = reply
			.serialize(vec![], &EncryptionProtocol::Unencrypted)
			.unwrap();
		let mut buf = BytesMut::from(bytes.as_slice());
		let _ = codex.decode(&mut buf).expect("decode ok");
		assert!(matches!(
			codex.context.get_encrypted(),
			EncryptionProtocol::Unencrypted
		));
	}

	#[test]
	fn decode_login_response_negotiates_bcencrypt_protocol() {
		let mut codex = BcCodex::new(empty_creds());
		let reply = Bc::new_from_xml(
			meta_login(0xdd01),
			BcXml {
				encryption: Some(Encryption {
					version: "1.1".to_string(),
					nonce: "AB".to_string(),
					type_: "md5".to_string(),
					..Default::default()
				}),
				..Default::default()
			},
		);
		let bytes = reply
			.serialize(vec![], &EncryptionProtocol::BCEncrypt)
			.unwrap();
		let mut buf = BytesMut::from(bytes.as_slice());
		let _ = codex.decode(&mut buf).expect("decode ok");
		assert!(matches!(
			codex.context.get_encrypted(),
			EncryptionProtocol::BCEncrypt
		));
	}

	#[test]
	fn decode_unknown_encryption_byte_returns_err() {
		let mut codex = BcCodex::new(empty_creds());
		let reply = Bc::new_from_xml(
			meta_login(0xdd0a),
			BcXml {
				encryption: Some(Encryption {
					version: "1.1".to_string(),
					nonce: "AB".to_string(),
					type_: "md5".to_string(),
					..Default::default()
				}),
				..Default::default()
			},
		);
		let bytes = reply
			.serialize(vec![], &EncryptionProtocol::BCEncrypt)
			.unwrap();
		let mut buf = BytesMut::from(bytes.as_slice());
		let err = codex.decode(&mut buf).expect_err("should reject");
		assert!(matches!(err, Error::UnknownEncryption(0x0a)));
	}

	#[test]
	fn decode_eof_with_partial_buffer_logs_and_returns_none() {
		let mut codex = BcCodex::new(empty_creds());
		let mut buf = BytesMut::from(&[0xF0u8, 0xDE, 0xBC][..]);
		let got = codex.decode_eof(&mut buf).expect("decode_eof ok");
		assert!(got.is_none());
	}

	#[test]
	fn decode_eof_with_empty_buffer_returns_none() {
		let mut codex = BcCodex::new(empty_creds());
		let mut buf = BytesMut::new();
		let got = codex.decode_eof(&mut buf).expect("decode_eof ok");
		assert!(got.is_none());
	}

	#[test]
	fn encode_login_with_aes_context_substitutes_bcencrypt() {
		// Set the codex's context to Aes via the decode 0xdd02 reply.
		let mut codex = BcCodex::new(empty_creds());
		// Force AES context directly via internal API.
		codex
			.context
			.set_encrypted(EncryptionProtocol::aes([0u8; 16]));
		// Encoding a login (msg_id 1) under Aes should fall to BCEncrypt
		// per the constraint, not panic.
		let bc = Bc::new_from_xml(
			meta_login(0x6611),
			BcXml {
				encryption: Some(Encryption {
					version: "1.1".to_string(),
					nonce: "AB".to_string(),
					type_: "md5".to_string(),
					..Default::default()
				}),
				..Default::default()
			},
		);
		let mut dst = BytesMut::new();
		codex.encode(bc, &mut dst).expect("encode ok");
		assert!(!dst.is_empty());
	}

	#[test]
	fn encode_login_with_full_aes_context_substitutes_bcencrypt() {
		let mut codex = BcCodex::new(empty_creds());
		codex
			.context
			.set_encrypted(EncryptionProtocol::full_aes([0u8; 16]));
		let bc = Bc::new_from_xml(
			meta_login(0x6611),
			BcXml {
				encryption: Some(Encryption {
					version: "1.1".to_string(),
					nonce: "AB".to_string(),
					type_: "md5".to_string(),
					..Default::default()
				}),
				..Default::default()
			},
		);
		let mut dst = BytesMut::new();
		codex.encode(bc, &mut dst).expect("encode ok");
		assert!(!dst.is_empty());
	}

	#[test]
	fn decode_login_response_negotiates_aes_protocol() {
		let mut codex = BcCodex::new(empty_creds());
		let reply = Bc::new_from_xml(
			meta_login(0xdd02),
			BcXml {
				encryption: Some(Encryption {
					version: "1.1".to_string(),
					nonce: "ABCDEFGH".to_string(),
					type_: "md5".to_string(),
					..Default::default()
				}),
				..Default::default()
			},
		);
		let bytes = reply
			.serialize(vec![], &EncryptionProtocol::BCEncrypt)
			.unwrap();
		let mut buf = BytesMut::from(bytes.as_slice());
		let _ = codex.decode(&mut buf).expect("decode ok");
		assert!(matches!(
			codex.context.get_encrypted(),
			EncryptionProtocol::Aes { .. }
		));
	}

	#[test]
	fn decode_login_response_negotiates_full_aes_protocol() {
		let mut codex = BcCodex::new(empty_creds());
		let reply = Bc::new_from_xml(
			meta_login(0xdd12),
			BcXml {
				encryption: Some(Encryption {
					version: "1.1".to_string(),
					nonce: "ABCDEFGH".to_string(),
					type_: "md5".to_string(),
					..Default::default()
				}),
				..Default::default()
			},
		);
		let bytes = reply
			.serialize(vec![], &EncryptionProtocol::BCEncrypt)
			.unwrap();
		let mut buf = BytesMut::from(bytes.as_slice());
		let _ = codex.decode(&mut buf).expect("decode ok");
		assert!(matches!(
			codex.context.get_encrypted(),
			EncryptionProtocol::FullAes { .. }
		));
	}

	#[test]
	fn decode_eof_with_complete_buffer_returns_some() {
		// Drive the `Some(frame)` arm of decode_eof.
		let mut codex = BcCodex::new(empty_creds());
		let bc = Bc::new_from_meta(BcMeta {
			msg_id: 23,
			channel_id: 0,
			stream_type: 0,
			response_code: 200,
			msg_num: 0,
			class: 0x6414,
		});
		let bytes = bc
			.serialize(vec![], &EncryptionProtocol::Unencrypted)
			.unwrap();
		let mut buf = BytesMut::from(bytes.as_slice());
		let got = codex.decode_eof(&mut buf).expect("decode_eof ok");
		assert!(got.is_some());
	}

	#[test]
	fn decode_extension_with_binary_data_zero_flips_binary_mode_off() {
		let mut codex = BcCodex::new(empty_creds());
		let ext = Extension {
			binary_data: Some(0),
			..Default::default()
		};
		let bc = Bc::new_from_ext(
			BcMeta {
				msg_id: 100,
				channel_id: 0,
				stream_type: 0,
				response_code: 200,
				msg_num: 9,
				class: 0x6414,
			},
			ext,
		);
		let bytes = bc
			.serialize(vec![], &EncryptionProtocol::Unencrypted)
			.unwrap();
		let mut buf = BytesMut::from(bytes.as_slice());
		let _ = codex.decode(&mut buf).expect("decode ok");
	}

	#[test]
	fn decode_extension_with_binary_data_one_flips_binary_mode_on() {
		let mut codex = BcCodex::new(empty_creds());
		let ext = Extension {
			binary_data: Some(1),
			..Default::default()
		};
		let bc = Bc::new_from_ext(
			BcMeta {
				msg_id: 100,
				channel_id: 0,
				stream_type: 0,
				response_code: 200,
				msg_num: 7,
				class: 0x6414,
			},
			ext,
		);
		let bytes = bc
			.serialize(vec![], &EncryptionProtocol::Unencrypted)
			.unwrap();
		let mut buf = BytesMut::from(bytes.as_slice());
		let _ = codex.decode(&mut buf).expect("decode ok");
	}

	// ---- sigV3 post-login FullAes switch ----

	fn signed_login(msg_num: u16, key: [u8; 16], iv: [u8; 16]) -> Bc {
		Bc::new_from_xml(
			BcMeta {
				msg_id: 1,
				channel_id: 0,
				stream_type: 0,
				response_code: 0,
				msg_num,
				class: 0x0000,
			},
			BcXml {
				login_user: Some(LoginUser {
					version: "1.1".to_string(),
					user_name: "u".to_string(),
					public_key: Some("PK".to_string()),
					session_aes: Some((key, iv)),
					..Default::default()
				}),
				..Default::default()
			},
		)
	}

	fn login_reply(msg_num: u16, response_code: u16) -> BytesMut {
		// Camera reply to the sigV3 login (decoded under BCEncrypt — the codec
		// forces BCEncrypt for msg_id==1 regardless of session state).
		let bytes = Bc::new_from_xml(
			BcMeta {
				msg_id: 1,
				channel_id: 0,
				stream_type: 0,
				response_code,
				msg_num,
				class: 0x6614,
			},
			BcXml::default(),
		)
		.serialize(vec![], &EncryptionProtocol::BCEncrypt)
		.unwrap();
		BytesMut::from(bytes.as_slice())
	}

	#[test]
	fn sigv3_encode_arms_pending_with_msg_num() {
		let mut codex = BcCodex::new(empty_creds());
		let (key, iv) = ([1u8; 16], [2u8; 16]);
		let mut dst = BytesMut::new();
		codex.encode(signed_login(42, key, iv), &mut dst).unwrap();
		assert_eq!(codex.pending_session_aes, Some((42, key, iv)));
	}

	#[test]
	fn sigv3_matching_200_reply_switches_to_full_aes() {
		let mut codex = BcCodex::new(empty_creds());
		let (key, iv) = ([3u8; 16], [4u8; 16]);
		let mut dst = BytesMut::new();
		codex.encode(signed_login(42, key, iv), &mut dst).unwrap();
		let _ = codex.decode(&mut login_reply(42, 200)).expect("decode");
		assert!(matches!(
			codex.context.get_encrypted(),
			EncryptionProtocol::FullAes { .. }
		));
		assert!(codex.pending_session_aes.is_none());
	}

	#[test]
	fn sigv3_non_200_reply_disarms_without_switching() {
		let mut codex = BcCodex::new(empty_creds());
		let mut dst = BytesMut::new();
		codex
			.encode(signed_login(7, [1u8; 16], [2u8; 16]), &mut dst)
			.unwrap();
		let _ = codex.decode(&mut login_reply(7, 417)).expect("decode");
		// Rejected login must NOT flip the session, and must disarm.
		assert!(matches!(
			codex.context.get_encrypted(),
			EncryptionProtocol::BCEncrypt
		));
		assert!(codex.pending_session_aes.is_none());
	}

	#[test]
	fn sigv3_switch_ignores_mismatched_msg_num() {
		let mut codex = BcCodex::new(empty_creds());
		let (key, iv) = ([1u8; 16], [2u8; 16]);
		let mut dst = BytesMut::new();
		codex.encode(signed_login(7, key, iv), &mut dst).unwrap();
		// A 200 to a DIFFERENT msg_num must not apply, and must stay armed.
		let _ = codex.decode(&mut login_reply(9, 200)).expect("decode");
		assert!(matches!(
			codex.context.get_encrypted(),
			EncryptionProtocol::BCEncrypt
		));
		assert_eq!(codex.pending_session_aes, Some((7, key, iv)));
	}
}
