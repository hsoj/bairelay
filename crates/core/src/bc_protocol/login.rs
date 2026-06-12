use super::{md5_string, BcCamera, Error, Result, Truncate};
use crate::bc::{model::*, xml::*};
use std::sync::atomic::Ordering;

#[cfg(test)]
use crate::bc_protocol::connection::mock::{reply_200_xml, MockConnection};

/// Legacy-login `response_code` byte the client sends to announce the
/// highest encryption it's willing to negotiate. The camera replies
/// with the actual encryption that will be used — never higher than the
/// requested ceiling.
const LEGACY_LOGIN_ENC_NONE: u16 = 0xdc00;
const LEGACY_LOGIN_ENC_BCENCRYPT: u16 = 0xdc01;
const LEGACY_LOGIN_ENC_AES: u16 = 0xdc12;

/// Baichuan message-class bytes for the login exchange. Legacy login
/// uses class `0x6514` (LegacyMsg body), the modern follow-up uses
/// `0x6414` (ModernMsg body with the BcXml LoginUser/LoginNet payload).
/// `0x6414` is the generic modern-msg class re-used across many other
/// commands (abilityinfo, version, etc.); the constant name here pins
/// its meaning at this call site.
const MSG_CLASS_LEGACY_LOGIN: u16 = 0x6514;
const MSG_CLASS_MODERN_LOGIN: u16 = 0x6414;

/// The requested encryption level to request
/// to the camera
///
/// The camera may use a lower one depending on support
///
/// Note the reolink camera only encrypt the control messages
/// the camera feed is always accessible
#[derive(Debug, Clone, Copy)]
pub enum MaxEncryption {
	/// No encryption
	None,
	/// BCEncrypt is a simple XOR algortirhm with a fixed key
	/// used in many older models
	BcEncrypt,
	/// AES is used in newer model
	Aes,
}

impl BcCamera {
	/// Login to the camera.
	///
	/// This should be called before most other commands
	pub async fn login(&self) -> Result<DeviceInfo> {
		self.login_with_maxenc(MaxEncryption::Aes).await
	}
	/// Login to the camera.
	///
	/// This should be called before most other commands
	pub async fn login_with_maxenc(&self, max_encryption: MaxEncryption) -> Result<DeviceInfo> {
		let device_info;
		// This { is here due to the connection and set_credentials both requiring a mutable borrow
		{
			let credentials = self.get_credentials();
			let connection = self.get_connection();
			let msg_num = self.new_message_num();
			let mut sub_login = connection.subscribe(MSG_ID_LOGIN, msg_num).await?;

			// Login flow is: Send legacy login message, expect back a modern message with Encryption
			// details.  Then, re-send the login as a modern login message.  Expect back a device info
			// congratulating us on logging in.

			// In the legacy scheme, username/password are MD5'd if they are encrypted (which they need
			// to be to "upgrade" to the modern login flow), then the hex of the MD5 is sent.
			// Note: I suspect there may be a buffer overflow opportunity in the firmware since in the
			// Baichuan library, these strings are capped at 32 bytes with a null terminator.  This
			// could also be a mistake in the library, the effect being it only compares 31 chars, not 32.
			// let md5_username = md5_string(&credentials.username, ZeroLast);
			// let md5_password = credentials
			//     .password
			//     .as_ref()
			//     .map(|p| md5_string(p, ZeroLast))
			//     .unwrap_or_else(|| EMPTY_LEGACY_PASSWORD.to_owned());

			// Refuse to log in with an absent / empty password under
			// AES or BcEncrypt: the wire path silently wraps `None` to
			// `""`, MD5s `format!("{}-{}", "", nonce)`, and the camera
			// answers with a generic 401 — the operator has no way to
			// tell that the misconfiguration is at THEIR end vs. a real
			// auth failure. Anonymous-login (`None` encryption) is the
			// only path where empty password is meaningful.
			match max_encryption {
				MaxEncryption::Aes | MaxEncryption::BcEncrypt => {
					let pw_present = credentials
						.password
						.as_deref()
						.map(|p| !p.is_empty())
						.unwrap_or(false);
					if !pw_present {
						log::warn!(
							"login refused: {:?} requires a non-empty password",
							max_encryption
						);
						return Err(Error::AuthFailed);
					}
				}
				MaxEncryption::None => {}
			}

			let enc_byte = match max_encryption {
				MaxEncryption::None => LEGACY_LOGIN_ENC_NONE,
				MaxEncryption::BcEncrypt => LEGACY_LOGIN_ENC_BCENCRYPT,
				MaxEncryption::Aes => LEGACY_LOGIN_ENC_AES,
			};
			let legacy_login = Bc {
				meta: BcMeta {
					msg_id: MSG_ID_LOGIN,
					channel_id: self.channel_id,
					msg_num,
					stream_type: 0,
					response_code: enc_byte,
					class: MSG_CLASS_LEGACY_LOGIN,
				},
				body: BcBody::LegacyMsg(LegacyMsg::LoginUpgrade),
			};

			sub_login.send(legacy_login).await?;

			let legacy_reply = sub_login.recv().await?;

			let nonce;
			match &legacy_reply.body {
				BcBody::ModernMsg(ModernMsg {
					payload:
						Some(BcPayloads::BcXml(BcXml {
							encryption: Some(encryption),
							..
						})),
					..
				}) => {
					nonce = &encryption.nonce;
				}
				_ => {
					return Err(Error::UnintelligibleReply {
						reply: std::sync::Arc::new(legacy_reply),
						why: "Expected an Encryption message back",
					})
				}
			}

			// In the modern login flow, the username/password are concat'd with the server's nonce
			// string, then MD5'd, then the hex of this MD5 is sent as the password.  This nonce
			// prevents replay attacks if the server were to require modern flow, but not rainbow table
			// attacks (since  the plain user/password MD5s have already been sent).  The upshot is that
			// you should use a very strong random password that is not found in a rainbow table and
			// not feasibly crackable with John the Ripper.

			let modern_password = credentials.password.clone().unwrap_or_default();
			let concat_username = format!("{}{}", credentials.username, nonce);
			let concat_password = format!("{}{}", modern_password, nonce);
			let md5_username = md5_string(&concat_username, Truncate);
			let md5_password = md5_string(&concat_password, Truncate);

			let modern_login = Bc::new_from_xml(
				BcMeta {
					msg_id: MSG_ID_LOGIN,
					channel_id: self.channel_id,
					msg_num,
					stream_type: 0,
					response_code: 0,
					class: MSG_CLASS_MODERN_LOGIN,
				},
				BcXml {
					login_user: Some(LoginUser {
						version: xml_ver(),
						user_name: md5_username,
						password: md5_password,
						user_ver: 1,
					}),
					login_net: Some(LoginNet::default()),
					..Default::default()
				},
			);

			sub_login.send(modern_login).await?;
			let modern_reply = sub_login.recv().await?;
			if modern_reply.meta.response_code != 200 {
				log::warn!(
					"login rejected: camera replied response_code={} to the modern login",
					modern_reply.meta.response_code
				);
				return Err(Error::CameraLoginFail);
			}

			match modern_reply.body {
				BcBody::ModernMsg(ModernMsg {
					payload:
						Some(BcPayloads::BcXml(BcXml {
							device_info: Some(info),
							..
						})),
					..
				}) => {
					// Login succeeded!
					self.logged_in.store(true, Ordering::Relaxed);
					device_info = info;
				}
				BcBody::ModernMsg(ModernMsg {
					extension: None,
					payload: None,
				}) => {
					log::warn!(
						"login rejected: camera replied 200 with an empty body (no DeviceInfo)"
					);
					return Err(Error::AuthFailed);
				}
				other => {
					return Err(Error::UnintelligibleReply {
						reply: std::sync::Arc::new(Bc {
							meta: modern_reply.meta,
							body: other,
						}),
						why: "Expected a DeviceInfo message back from login",
					})
				}
			}
		}

		// Populate the list of abilities this user has with the camera
		log::debug!("Populating abilities");
		self.populate_abilities().await?;
		Ok(device_info)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// A full login happy-path covers three scripted exchanges (legacy
	// login -> encryption xml, modern login -> device_info, then
	// MSG_ID_ABILITY_INFO for `populate_abilities`) plus valid md5
	// nonce math. Out of scope for this harness — exercised via the
	// real-hardware live-fire tests. We cover the legacy-reply-parse
	// error path here instead.

	#[tokio::test]
	async fn login_legacy_reply_missing_encryption_returns_err() {
		// Camera replies 200 but with no Encryption xml — login
		// should report `UnintelligibleReply`.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.login_with_maxenc(MaxEncryption::None)
			.await
			.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[test]
	fn max_encryption_enum_derives_debug_and_clone() {
		// Lock the three-variant matchup and the derive trait set.
		let a = MaxEncryption::None;
		let _ = format!("{:?}", a);
		let _b = a;
		let _ = format!("{:?}", MaxEncryption::BcEncrypt);
		let _ = format!("{:?}", MaxEncryption::Aes);
	}

	fn encryption_reply(nonce: &str) -> BcXml {
		BcXml {
			encryption: Some(Encryption {
				version: "1".into(),
				nonce: nonce.into(),
				type_: "md5".into(),
			}),
			..Default::default()
		}
	}

	#[tokio::test]
	async fn login_all_three_max_encryption_variants_hit_enc_byte_branch() {
		// The enc_byte match has three arms. We don't need the full
		// crypto flow — the legacy-reply step alone exercises the
		// match. Fail-path in the legacy reply short-circuits before
		// any crypto runs. Seed a non-empty password so the AES /
		// BcEncrypt fail-fast (added 2026-05-01) doesn't intercept.
		use crate::bc_protocol::Credentials;
		for max_enc in [
			MaxEncryption::None,
			MaxEncryption::BcEncrypt,
			MaxEncryption::Aes,
		] {
			let mock = MockConnection::new()
				.expect_msg(MSG_ID_LOGIN)
				.reply_with(|req| reply_200_xml(req, BcXml::default()))
				.build()
				.await;
			let creds = Credentials::new("admin", Some("hunter2"));
			let cam = BcCamera::from_mock_connection_with_credentials(mock, creds).await;
			let err = cam.login_with_maxenc(max_enc).await.expect_err("err");
			assert!(matches!(err, Error::UnintelligibleReply { .. }));
		}
	}

	#[tokio::test]
	async fn login_modern_reply_non_200_returns_credential_error() {
		// Legacy reply has an Encryption nonce → advance to modern
		// step; modern reply comes back with response_code != 200 →
		// CameraLoginFail.
		use crate::bc_protocol::connection::mock::reply_err_code;
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| reply_200_xml(req, encryption_reply("abc123")))
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| reply_err_code(req, 401))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.login_with_maxenc(MaxEncryption::None)
			.await
			.expect_err("err");
		assert!(matches!(err, Error::CameraLoginFail));
	}

	#[tokio::test]
	async fn login_modern_reply_empty_modern_msg_is_auth_failed() {
		// Modern reply: ModernMsg { extension: None, payload: None } →
		// AuthFailed branch.
		use crate::bc_protocol::connection::mock::reply_200_empty;
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| reply_200_xml(req, encryption_reply("abc123")))
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.login_with_maxenc(MaxEncryption::None)
			.await
			.expect_err("err");
		assert!(matches!(err, Error::AuthFailed));
	}

	#[tokio::test]
	async fn login_happy_path_full_three_exchange_round_trip() {
		// Legacy → Encryption; Modern → DeviceInfo; then ability-info
		// subscription for populate_abilities. This exercises the
		// DeviceInfo success arm (lines 142-154) + the tail
		// populate_abilities + Ok(device_info) return (lines 168-171).
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| reply_200_xml(req, encryption_reply("nonce-xyz")))
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						device_info: Some(DeviceInfo {
							version: Some("1".into()),
							resolution: None,
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_ABILITY_INFO)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						ability_info: Some(AbilityInfo {
							username: "admin".into(),
							system: Some(AbilityInfoToken {
								sub_module: vec![AbilityInfoSubModule {
									channel_id: Some(0),
									ability_value: "reboot_rw".into(),
								}],
							}),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let info = cam
			.login_with_maxenc(MaxEncryption::None)
			.await
			.expect("ok");
		assert_eq!(info.version.as_deref(), Some("1"));
		assert!(cam.logged_in.load(Ordering::Relaxed));
	}

	#[tokio::test]
	async fn login_default_calls_login_with_maxenc_aes() {
		// `login()` is a thin delegate to login_with_maxenc(Aes). The
		// legacy-reply-parse error short-circuits before crypto runs, so
		// we just need a mock that replies with a 200 empty body. AES
		// requires a non-empty password (fail-fast); seed one so we
		// reach the wire.
		use crate::bc_protocol::Credentials;
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let creds = Credentials::new("admin", Some("hunter2"));
		let cam = BcCamera::from_mock_connection_with_credentials(mock, creds).await;
		let err = cam.login().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn login_aes_with_no_password_fails_before_wire_io() {
		// `password = None` under Aes used to silently MD5(nonce) and
		// hand the camera an obviously-wrong digest. Now refused
		// up-front with `AuthFailed`. Mock has no `expect_msg` — proves
		// no MSG_ID_LOGIN was ever sent.
		use crate::bc_protocol::Credentials;
		let mock = MockConnection::new().build().await;
		let creds = Credentials::new("admin", None::<String>);
		let cam = BcCamera::from_mock_connection_with_credentials(mock, creds).await;
		let err = cam
			.login_with_maxenc(MaxEncryption::Aes)
			.await
			.expect_err("must refuse AES + empty");
		assert!(matches!(err, Error::AuthFailed));
	}

	#[tokio::test]
	async fn login_bcencrypt_with_empty_password_fails_before_wire_io() {
		use crate::bc_protocol::Credentials;
		let mock = MockConnection::new().build().await;
		let creds = Credentials::new("admin", Some(""));
		let cam = BcCamera::from_mock_connection_with_credentials(mock, creds).await;
		let err = cam
			.login_with_maxenc(MaxEncryption::BcEncrypt)
			.await
			.expect_err("must refuse BcEncrypt + empty");
		assert!(matches!(err, Error::AuthFailed));
	}

	#[tokio::test]
	async fn login_none_with_empty_password_proceeds_to_wire() {
		// Anonymous-login (None encryption) is the only path where
		// empty password is meaningful — must not short-circuit.
		use crate::bc_protocol::Credentials;
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let creds = Credentials::new("admin", Some(""));
		let cam = BcCamera::from_mock_connection_with_credentials(mock, creds).await;
		let err = cam
			.login_with_maxenc(MaxEncryption::None)
			.await
			.expect_err("legacy reply was empty → UnintelligibleReply");
		// Reaches the wire, then dies on the missing Encryption xml —
		// proves the fail-fast did NOT trigger for None encryption.
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn login_modern_reply_missing_device_info_returns_unintelligible() {
		// Modern reply 200 but body is neither DeviceInfo nor empty
		// (payload is Some but of a non-DeviceInfo shape) → matches
		// the wildcard UnintelligibleReply branch.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| reply_200_xml(req, encryption_reply("abc123")))
			.expect_msg(MSG_ID_LOGIN)
			.reply_with(|req| {
				// A 200 reply with an unrelated xml payload (encryption)
				// — not DeviceInfo and not empty → wildcard path.
				reply_200_xml(req, encryption_reply("xyz"))
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.login_with_maxenc(MaxEncryption::None)
			.await
			.expect_err("err");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}
}
