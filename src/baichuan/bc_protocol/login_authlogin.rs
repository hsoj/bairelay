//! Camera-local `getAccesskey` + `authLogin` login for newer Reolink
//! battery firmware (GitHub issue #3).
//!
//! New-firmware battery cameras (e.g. Argus PT Ultra fw ~5808) reject the
//! legacy plain-MD5 modern login (response_code 406). Their `<Encryption>`
//! reply advertises an `<authTypeList>` of
//! `password / sigV1 / sigV3 / authLogin / getAccesskey`. `sigV3` needs a
//! cloud-issued signature bundle (token/tokenKey/certChain from Reolink's
//! `getAccesskey` cloud API — see `login_sigv3`), which a
//! camera-and-password-only client cannot synthesise. `authLogin` /
//! `getAccesskey` are instead a **camera-local** challenge-response that
//! needs no cloud account.
//!
//! Flow (recovered by reverse-engineering the official app's
//! `BaichuanDevice.cpp` authCodeLogin state machine):
//!
//! 1. `LoginUpgrade` → `Encryption{nonce, authTypeList}` (shared with the
//!    legacy login; the caller already did this).
//! 2. → `LoginUser{authType=getAccesskey, AuthInfo{authCode}}` where
//!    `authCode = md5(password + nonce)`.
//!    ← challenge: two base64 tokens, each `AES-128-CFB` encrypted under
//!    `key = md5("{nonce}-{password}")[..16]`, `iv = "0123456789abcdef"` —
//!    i.e. exactly [`Credentials::make_aeskey`] + [`EncryptionProtocol::aes`].
//! 3. decrypt both tokens → `A`, `B`; →
//!    `LoginUser{authType=authLogin, userName=md5(A+nonce),
//!    password=md5(B+nonce), userVer=1}`.
//!    ← `DeviceInfo`.
//!
//! Every primitive already existed in `baichuan`; this module
//! is the small glue + the (testable) challenge parsing/decrypt. The wire
//! orchestration lives in `login.rs`.

use super::credentials::Credentials;
use super::{md5_string, Error, Truncate};
use crate::baichuan::bc::crypto::EncryptionProtocol;
use base64::Engine as _;

const B64: base64::engine::general_purpose::GeneralPurpose =
	base64::engine::general_purpose::STANDARD;

/// Width of each base64 token slot in the camera's binary challenge
/// payload (two slots → a 128-byte buffer). The app reads slot 0 at
/// offset 0 and slot 1 at offset `0x40`.
pub(crate) const CHALLENGE_SLOT: usize = 0x40;

/// `authCode` for the `getAccesskey` step: `md5(password + nonce)`
/// truncated to 31 hex chars — the same proof the legacy `<password>`
/// field carries, so a camera that knows the password can derive the same
/// value and encrypt the challenge for us.
pub(crate) fn auth_code(password: &str, nonce: &str) -> String {
	md5_string(&format!("{}{}", password, nonce), Truncate)
}

/// Split the camera's binary challenge into its two NUL-terminated base64
/// token strings (slot 0 at `0`, slot 1 at `CHALLENGE_SLOT`). Returns
/// `None` if the buffer is too short or either slot is empty — the caller
/// then logs the raw reply for diagnosis instead of proceeding blind.
pub(crate) fn parse_challenge(payload: &[u8]) -> Option<(String, String)> {
	let slot = |off: usize| -> Option<String> {
		let end = (off + CHALLENGE_SLOT).min(payload.len());
		let raw = payload.get(off..end)?;
		let s: String = raw
			.iter()
			.take_while(|&&b| b != 0)
			.map(|&b| b as char)
			.collect();
		let s = s.trim().to_string();
		if s.is_empty() {
			None
		} else {
			Some(s)
		}
	};
	Some((slot(0)?, slot(CHALLENGE_SLOT)?))
}

/// Decrypt one base64 challenge token: standard-alphabet base64-decode,
/// then AES-128-CFB decrypt with the nonce+password session key (the same
/// `make_aeskey` + `0123456789abcdef` IV the rest of the protocol uses).
/// Trailing NULs are trimmed off the recovered token.
pub(crate) fn decrypt_token(b64: &str, creds: &Credentials, nonce: &str) -> Result<String, Error> {
	let ct = B64
		.decode(b64.trim())
		.map_err(|_| Error::Other("authLogin: challenge token not valid base64"))?;
	let key = creds.make_aeskey(nonce);
	let pt = EncryptionProtocol::aes(key).decrypt(0, &ct);
	let s = String::from_utf8_lossy(&pt);
	Ok(s.trim_end_matches('\0').trim().to_string())
}

/// Final `authLogin` `userName` / `password` field: `md5(token + nonce)`
/// truncated to 31 hex chars, computed over each decrypted challenge
/// token.
pub(crate) fn authlogin_field(decrypted_token: &str, nonce: &str) -> String {
	md5_string(&format!("{}{}", decrypted_token, nonce), Truncate)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn auth_code_matches_legacy_password_formula() {
		// authCode is md5(password+nonce) truncated to 31 — identical to
		// the value login.rs builds for the modern `<password>` field.
		let nonce = "abc123nonce";
		let got = auth_code("hunter2", nonce);
		let want = md5_string(&format!("{}{}", "hunter2", nonce), Truncate);
		assert_eq!(got, want);
		assert_eq!(got.len(), 31);
	}

	#[test]
	fn parse_challenge_extracts_two_nul_terminated_slots() {
		let mut buf = vec![0u8; 0x80];
		buf[..5].copy_from_slice(b"AAAA=");
		buf[0x40..0x45].copy_from_slice(b"BBBB=");
		let (a, b) = parse_challenge(&buf).expect("two tokens");
		assert_eq!(a, "AAAA=");
		assert_eq!(b, "BBBB=");
	}

	#[test]
	fn parse_challenge_rejects_short_or_empty() {
		assert!(parse_challenge(&[]).is_none());
		assert!(parse_challenge(&[0u8; 0x80]).is_none()); // both slots empty
		let mut only_first = vec![0u8; 0x80];
		only_first[..3].copy_from_slice(b"AAA");
		assert!(parse_challenge(&only_first).is_none()); // slot 1 empty
	}

	#[test]
	fn decrypt_token_roundtrips_against_make_aeskey() {
		// Encrypt a token with the same key/IV the camera would use, then
		// confirm decrypt_token recovers it.
		let creds = Credentials::new("admin", Some("s3cret"));
		let nonce = "noncenoncenonce";
		let key = creds.make_aeskey(nonce);
		let plain = b"TOKEN-ABCDEF";
		let ct = EncryptionProtocol::aes(key).encrypt(0, plain);
		let b64 = B64.encode(ct);
		let got = decrypt_token(&b64, &creds, nonce).expect("decrypts");
		assert_eq!(got, "TOKEN-ABCDEF");
	}

	#[test]
	fn decrypt_token_rejects_bad_base64() {
		let creds = Credentials::new("admin", Some("s3cret"));
		let err = decrypt_token("not valid base64!!!", &creds, "n").unwrap_err();
		assert!(matches!(err, Error::Other(_)));
	}

	#[test]
	fn authlogin_field_is_md5_token_nonce_truncated() {
		let got = authlogin_field("TOKEN-A", "noncexyz");
		assert_eq!(got, md5_string("TOKEN-Anoncexyz", Truncate));
		assert_eq!(got.len(), 31);
	}
}
