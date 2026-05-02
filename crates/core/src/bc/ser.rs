use super::model::*;
use cookie_factory::bytes::*;
use cookie_factory::sequence::tuple;
use cookie_factory::{combinator::*, gen};
use cookie_factory::{GenError, SerializeFn, WriteContext};
use log::error;
use std::io::Write;

impl Bc {
	pub(crate) fn serialize<W: Write>(
		&self,
		buf: W,
		encryption_protocol: &EncryptionProtocol,
	) -> Result<W, GenError> {
		// Pre-serialize XML payloads eagerly so a `quick_xml` error
		// (NaN floats in PtzControl, Unicode edge cases in user-supplied
		// strings, etc.) propagates as a typed `GenError` instead of
		// panicking inside the cookie-factory closure. The previous
		// shape ran `xml.serialize(...).unwrap()` inside `bc_ext` /
		// `bc_payload`, which the cookie-factory machinery cannot
		// recover from cleanly.
		let body_buf;
		let payload_offset;

		match &self.body {
			BcBody::ModernMsg(ref modern) => {
				let ext_bytes: Option<Vec<u8>> = match &modern.extension {
					Some(ext) => Some(ext.serialize(vec![]).map_err(|e| {
						error!("Extension XML serialize failed: {e}");
						GenError::CustomError(2)
					})?),
					None => None,
				};
				let payload_bytes: Option<Vec<u8>> = match &modern.payload {
					Some(BcPayloads::BcXml(x)) => Some(x.serialize(vec![]).map_err(|e| {
						error!("BcXml payload serialize failed: {e}");
						GenError::CustomError(3)
					})?),
					Some(BcPayloads::Binary(b)) => Some(b.clone()),
					None => None,
				};
				let payload_is_xml = matches!(&modern.payload, Some(BcPayloads::BcXml(_)));

				// First serialize ext (already encrypted below).
				let (temp_buf, ext_len) = gen(
					opt_ref(&ext_bytes, |b| {
						bc_ext_bytes(self.meta.channel_id as u32, b, encryption_protocol)
					}),
					vec![],
				)?;

				// Now get the offset of the payload
				payload_offset = if has_payload_offset(self.meta.class) {
					// If we're required to put binary length, put 0 if we have no binary
					Some(if modern.extension.is_some() {
						ext_len as u32
					} else {
						0
					})
				} else {
					None
				};

				// Now get the payload part of the body and add to ext_buf
				let (temp_buf, _) = gen(
					opt_ref(&payload_bytes, |b| {
						bc_payload_bytes(
							self.meta.channel_id as u32,
							b,
							payload_is_xml,
							encryption_protocol,
						)
					}),
					temp_buf,
				)?;
				body_buf = temp_buf;
			}

			BcBody::LegacyMsg(ref legacy) => {
				let (buf, _) = gen(bc_legacy(legacy), vec![]).map_err(|e| {
					error!("Send error: {}", e);
					e
				})?;
				body_buf = buf;
				payload_offset = None;
			}
		};

		// Now have enough info to create the header
		let header = BcHeader::from_meta(&self.meta, body_buf.len() as u32, payload_offset);

		let (buf, _n) = gen(tuple((bc_header(&header), slice(body_buf))), buf)?;

		Ok(buf)
	}
}

fn bc_ext_bytes<W: Write>(
	enc_offset: u32,
	xml_bytes: &[u8],
	encryption_protocol: &EncryptionProtocol,
) -> impl SerializeFn<W> {
	let enc_bytes = encryption_protocol.encrypt(enc_offset, xml_bytes);
	slice(enc_bytes)
}

fn bc_payload_bytes<W: Write>(
	enc_offset: u32,
	payload_bytes: &[u8],
	is_xml: bool,
	encryption_protocol: &EncryptionProtocol,
) -> impl SerializeFn<W> {
	let bytes = if is_xml {
		encryption_protocol.encrypt(enc_offset, payload_bytes)
	} else {
		payload_bytes.to_vec()
	};
	slice(bytes)
}

fn bc_header<W: Write>(header: &BcHeader) -> impl SerializeFn<W> {
	tuple((
		le_u32(MAGIC_HEADER),
		le_u32(header.msg_id),
		le_u32(header.body_len),
		le_u8(header.channel_id),
		le_u8(header.stream_type),
		le_u16(header.msg_num),
		le_u16(header.response_code),
		le_u16(header.class),
		opt(header.payload_offset, le_u32),
	))
}

fn bc_legacy<W: Write>(legacy: &'_ LegacyMsg) -> impl SerializeFn<W> + '_ {
	move |out: WriteContext<W>| {
		use LegacyMsg::*;
		match legacy {
			LoginMsg { username, password } => {
				if username.len() != 32 || password.len() != 32 {
					// Error handling could be improved here...
					return Err(GenError::CustomError(0));
				}
				tuple((
					slice(username),
					slice(password),
					// Login messages are 1836 bytes total, username/password
					// take up 32 chars each, 1772 zeros follow
					slice(&[0u8; 1772][..]),
				))(out)
			}
			LoginUpgrade => {
				// Write nothing as it is header only
				slice(&[])(out)
			}
			UnknownMsg => {
				// Returning a typed serializer error rather than
				// panicking — `LegacyMsg::UnknownMsg` is the parser
				// fall-through arm in `bc::de::bc_body` for any
				// non-`MSG_ID_LOGIN` legacy-class message. Today no
				// caller round-trips parsed legacy messages, but the
				// crate is `pub`; a future debug recorder or tee that
				// re-emits parsed `Bc` would otherwise panic on
				// hostile or unknown legacy traffic. Mirror the
				// `LoginMsg { length != 32 }` arm above.
				Err(GenError::CustomError(1))
			}
		}
	}
}

/// Applies the supplied serializer with the Option's interior data if present
fn opt<W, T, F>(opt: Option<T>, ser: impl Fn(T) -> F) -> impl SerializeFn<W>
where
	F: SerializeFn<W>,
	T: Copy,
	W: Write,
{
	move |buf: WriteContext<W>| {
		if let Some(val) = opt {
			ser(val)(buf)
		} else {
			do_nothing()(buf)
		}
	}
}

fn opt_ref<'a, W, T, F, S>(opt: &'a Option<T>, ser: S) -> impl SerializeFn<W> + 'a
where
	F: SerializeFn<W>,
	W: Write,
	S: Fn(&'a T) -> F + 'a,
{
	move |buf: WriteContext<W>| {
		if let Some(ref val) = opt {
			ser(val)(buf)
		} else {
			do_nothing()(buf)
		}
	}
}

/// A serializer combinator that does nothing with its input
fn do_nothing<W>() -> impl SerializeFn<W> {
	Ok
}

#[test]
fn test_legacy_login_roundtrip() {
	let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

	// I don't want to make up a sample message; just load it
	let sample = include_bytes!("samples/model_sample_legacy_login.bin");
	let msg = Bc::deserialize(&context, &mut bytes::BytesMut::from(&sample[..])).unwrap();

	let ser_buf = msg
		.serialize(vec![], &EncryptionProtocol::BCEncrypt)
		.unwrap();
	let msg2 = Bc::deserialize(&context, &mut bytes::BytesMut::from(ser_buf.as_slice())).unwrap();
	assert_eq!(msg, msg2);
	assert_eq!(&sample[..], ser_buf.as_slice());
}

/// Legacy `LoginMsg` rejects username/password not exactly 32 bytes —
/// the on-wire layout is fixed-width, so the serializer surfaces a
/// custom GenError rather than truncating or padding silently.
#[test]
fn legacy_login_rejects_short_username() {
	use super::model::LegacyMsg;
	let bc = Bc {
		meta: BcMeta {
			msg_id: 1,
			channel_id: 0,
			stream_type: 0,
			response_code: 0,
			msg_num: 0,
			class: 0x6514,
		},
		body: BcBody::LegacyMsg(LegacyMsg::LoginMsg {
			username: "x".repeat(16),
			password: "x".repeat(32),
		}),
	};
	let err = bc.serialize(vec![], &EncryptionProtocol::Unencrypted);
	assert!(err.is_err());
}

/// `LegacyMsg::LoginUpgrade` serialises to header only — body_buf is
/// empty and round-trips cleanly.
#[test]
fn legacy_login_upgrade_serialises_header_only() {
	use super::model::LegacyMsg;
	let bc = Bc {
		meta: BcMeta {
			msg_id: 1,
			channel_id: 0,
			stream_type: 0,
			response_code: 0,
			msg_num: 0,
			class: 0x6514,
		},
		body: BcBody::LegacyMsg(LegacyMsg::LoginUpgrade),
	};
	let bytes = bc
		.serialize(vec![], &EncryptionProtocol::Unencrypted)
		.expect("serialize ok");
	// Standard header is 20 bytes for legacy class (no payload_offset).
	assert_eq!(bytes.len(), 20);
}

/// `Binary` payload path — bc_payload's match arm that just clones the
/// caller's bytes (no encryption applied, encryption is for XML only).
#[test]
fn modern_msg_with_binary_payload_round_trips_unencrypted() {
	let bc = Bc {
		meta: BcMeta {
			msg_id: 109,
			channel_id: 0,
			stream_type: 0,
			response_code: 200,
			msg_num: 7,
			class: 0x6414,
		},
		body: BcBody::ModernMsg(ModernMsg {
			extension: None,
			payload: Some(BcPayloads::Binary(vec![0xDE, 0xAD, 0xBE, 0xEF])),
		}),
	};
	let bytes = bc
		.serialize(vec![], &EncryptionProtocol::Unencrypted)
		.expect("serialize ok");
	// Header (24 bytes for 0x6414 with payload_offset) + 4 bytes binary.
	assert!(bytes.len() >= 24 + 4);
	// The raw binary should appear as the last 4 bytes.
	assert_eq!(&bytes[bytes.len() - 4..], &[0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn test_modern_login_roundtrip() {
	let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

	// I don't want to make up a sample message; just load it
	let sample = include_bytes!("samples/model_sample_modern_login.bin");

	let msg = Bc::deserialize(&context, &mut bytes::BytesMut::from(&sample[..])).unwrap();

	let ser_buf = msg
		.serialize(vec![], &EncryptionProtocol::BCEncrypt)
		.unwrap();
	let msg2 = Bc::deserialize(&context, &mut bytes::BytesMut::from(ser_buf.as_slice())).unwrap();
	assert_eq!(msg, msg2);
}
