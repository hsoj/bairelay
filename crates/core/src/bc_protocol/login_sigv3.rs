//! sigV3 / ECDHE login crypto for newer Reolink firmware.
//!
//! Newer battery-camera firmware rejects the legacy plain-MD5 modern
//! login (response_code 406) and instead negotiates an X25519 ECDHE
//! handshake ("sigV3"). The camera advertises its ephemeral public key
//! in the `<Encryption><ECDHE>` block of its (unauthenticated) login
//! reply; the client generates its own X25519 key and sends, on top of
//! the legacy login fields, its public key plus an AES-encrypted proof
//! (`cipherContent`).
//!
//! Two separate things are proven. The PASSWORD is proven via the
//! unchanged legacy `<password>` = `md5(password+nonce)` field the login
//! still carries — the firmware did not change the password hash, it just
//! additionally requires the ECDHE layer. The ECDHE SESSION BINDING is the
//! `cipherContent`: AES-encrypted under a key derived from `PBKDF2(nonce,
//! salt = X25519 shared secret)`. Both sides compute that from public
//! values (nonce + ECDH), so it does NOT prove the password; it proves we
//! hold the private key for the `publicKey` we sent (anti-replay/MITM).
//!
//! This is fully camera-local: the camera's ECDHE material rides its
//! unauthenticated Encryption reply, and nothing here needs a Reolink
//! cloud account. Construction recovered by reverse-engineering the
//! official app (`BaichuanDevice::signatureLoginV3`).

use base64::Engine as _;
use cfb_mode::cipher::generic_array::GenericArray;
use cfb_mode::cipher::{AsyncStreamCipher, KeyIvInit};
use rand::RngCore;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use super::Error;

/// Parse `(camera_public_b64, iterations)` from a handshake `pl` line if it
/// advertises sigV3 (`P2=v3`). Fields are `Pn=value` pairs split on `,`/`;`.
pub(crate) fn parse_pl(pl: &str) -> Option<(String, u32)> {
	let mut is_v3 = false;
	let mut pubkey: Option<String> = None;
	let mut iters: Option<u32> = None;
	for field in pl.split([',', ';']) {
		if let Some((k, v)) = field.split_once('=') {
			match k.trim() {
				"P2" => is_v3 = v.trim() == "v3",
				"P4" => pubkey = Some(v.trim().to_string()),
				"P6" => iters = v.trim().parse().ok(),
				_ => {}
			}
		}
	}
	if is_v3 {
		Some((pubkey?, iters.unwrap_or(1000)))
	} else {
		None
	}
}

// The app encrypts cipherContent with AES-128-CFB-128 (OpenSSL
// `AES_cfb128_encrypt`, num=0) — the same stream cipher the rest of the
// Baichuan protocol uses, NOT CBC. Stream cipher: ciphertext length ==
// plaintext length, no padding.
type Aes128CfbEnc = cfb_mode::Encryptor<aes::Aes128>;
const B64: base64::engine::general_purpose::GeneralPurpose =
	base64::engine::general_purpose::STANDARD;

/// The extra `<LoginUser>` children the sigV3 login carries on top of the
/// legacy `userName` / `password` MD5 fields.
pub(crate) struct Sigv3Extras {
	/// base64 of our ephemeral X25519 public key.
	pub public_key: String,
	/// base64 of `AES-128-CFB-128(cipherContent-JSON)`.
	pub cipher_content: String,
	/// Post-login session AES-128-CFB key — `PBKDF2(nonce, shared, iters)[0..16]`.
	/// The camera switches the session to FullAes (control + media) with this
	/// key + IV right after a successful sigV3 login (`setXmlEncryptVersion(2)`).
	pub session_key: [u8; 16],
	/// Post-login session AES-128-CFB IV — the derived bytes `[16..32]` (NOT the
	/// fixed BCEncrypt IV).
	pub session_iv: [u8; 16],
}

/// Build the sigV3 login extras from the camera's ECDHE offer.
///
/// Note the key is derived from the `nonce` (not the password — see the
/// module docs); the password is proven by the separate legacy
/// `md5(password+nonce)` `<password>` field built by the caller.
///
/// - `nonce` — the camera's login nonce (`<Encryption><nonce>`).
/// - `camera_public_b64` — `<ECDHE><publicKey>` from the camera's reply.
/// - `iterations` — `<ECDHE><iterations>` (observed: 1000).
/// - `unix_time` — current UTC unix seconds, stamped into the proof.
pub(crate) fn build_sigv3_extras(
	nonce: &str,
	camera_public_b64: &str,
	iterations: u32,
	unix_time: i64,
	token_p: &str,
	token_s: &str,
) -> Result<Sigv3Extras, Error> {
	// `iterations` is attacker-controllable (camera-supplied, or a serde
	// default of 0 when the field is absent). Reject 0 (broken KDF) and an
	// absurd ceiling (PBKDF2 CPU-DoS). The observed real value is 1000.
	if !(1..=1_000_000).contains(&iterations) {
		return Err(Error::Other(
			"sigV3: camera ECDHE iterations out of sane range (expected 1..=1_000_000)",
		));
	}

	// Camera's ephemeral X25519 public key (32 bytes).
	let cam_pub_bytes = B64
		.decode(camera_public_b64.trim())
		.map_err(|_| Error::Other("sigV3: camera publicKey not valid base64"))?;
	let cam_pub: [u8; 32] = cam_pub_bytes
		.as_slice()
		.try_into()
		.map_err(|_| Error::Other("sigV3: camera publicKey not 32 bytes"))?;

	// Our ephemeral X25519 keypair.
	let mut sk_bytes = [0u8; 32];
	rand::rngs::OsRng.fill_bytes(&mut sk_bytes);
	let our_secret = StaticSecret::from(sk_bytes);
	let our_public = PublicKey::from(&our_secret);

	// Raw X25519 shared secret (RFC 7748; matches the app's OpenSSL
	// `EVP_PKEY_derive` over NID_X25519, no post-hash).
	let shared = our_secret.diffie_hellman(&PublicKey::from(cam_pub));

	// Derived AES material: PBKDF2-HMAC-SHA256(nonce, salt=shared,
	// iters, dkLen=32) -> AES-128 key (bytes[0..16]) + IV (bytes[16..32]).
	// The app zero-pads the nonce into a fixed 32-byte buffer (first 31
	// bytes copied) and passes length 32 — replicate that exactly.
	let mut kdf_buf = [0u8; 32];
	let nb = nonce.as_bytes();
	let n = nb.len().min(31);
	kdf_buf[..n].copy_from_slice(&nb[..n]);
	let mut derived = [0u8; 32];
	pbkdf2::pbkdf2_hmac::<Sha256>(&kdf_buf, shared.as_bytes(), iterations, &mut derived);
	let (key, iv) = derived.split_at(16);

	// cipherContent plaintext JSON. Field shape recovered from the app:
	// `nonce` echoes the camera's login nonce; `token` is left empty for
	// the camera-local path (no cloud accessKey — those fields come from
	// the signature-login cfg). If the camera still rejects, `token` /
	// tokenKey / certChain are the next knobs.
	let json = format!(
		r#"{{"nonce":"{}","clientTime":{},"token":{{"p":"{}","s":"{}"}}}}"#,
		json_escape(nonce),
		unix_time,
		json_escape(token_p),
		json_escape(token_s)
	);

	// AES-128-CFB-128, in-place (stream cipher: ct len == pt len).
	let mut ct = json.clone().into_bytes();
	Aes128CfbEnc::new(GenericArray::from_slice(key), GenericArray::from_slice(iv)).encrypt(&mut ct);

	// `derived` is a fixed 32-byte buffer so these halves are always 16 bytes;
	// map the (unreachable) error rather than `expect`, keeping the crate
	// panic-free on every camera-fed path.
	let session_key: [u8; 16] = key
		.try_into()
		.map_err(|_| Error::Other("sigV3: derived key half not 16 bytes"))?;
	let session_iv: [u8; 16] = iv
		.try_into()
		.map_err(|_| Error::Other("sigV3: derived iv half not 16 bytes"))?;
	let extras = Sigv3Extras {
		public_key: B64.encode(our_public.as_bytes()),
		cipher_content: B64.encode(&ct),
		session_key,
		session_iv,
	};

	// Heavy diagnostic logging for the iterate-with-reporter loop. Key
	// material (shared secret, derived key) is only ever logged as a
	// 4-byte fingerprint. The cipherContent plaintext (nonce + clientTime,
	// no secret) is at trace.
	log::info!(
		"sigV3: built login extras (iterations={iterations}, cipherContent_len={})",
		ct.len()
	);
	log::debug!(
		"sigV3: our_public(b64)={} cam_public(b64)={} shared_fp={:02x?} derived_fp={:02x?}",
		extras.public_key,
		camera_public_b64.trim(),
		fp(shared.as_bytes()),
		fp(&derived),
	);
	log::trace!("sigV3: cipherContent plaintext = {json}");

	Ok(extras)
}

/// First 4 bytes as a log fingerprint — never log full key material.
fn fp(b: &[u8]) -> [u8; 4] {
	let mut o = [0u8; 4];
	o.copy_from_slice(&b[..4]);
	o
}

/// Minimal JSON string escaping for the values embedded in the proof.
fn json_escape(s: &str) -> String {
	s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn unhex(s: &str) -> Vec<u8> {
		(0..s.len())
			.step_by(2)
			.map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
			.collect()
	}

	#[test]
	fn x25519_matches_rfc7748_vector() {
		// RFC 7748 §6.1 (Alice's side): scalar + camera-u -> shared.
		let scalar = unhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
		let u = unhex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
		let expect = unhex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
		let sk = StaticSecret::from(<[u8; 32]>::try_from(scalar.as_slice()).unwrap());
		let pk = PublicKey::from(<[u8; 32]>::try_from(u.as_slice()).unwrap());
		assert_eq!(
			sk.diffie_hellman(&pk).as_bytes().as_slice(),
			expect.as_slice()
		);
	}

	#[test]
	fn pbkdf2_hmac_sha256_matches_known_vector() {
		// password="password", salt="salt", iter=1, dkLen=32.
		let mut out = [0u8; 32];
		pbkdf2::pbkdf2_hmac::<Sha256>(b"password", b"salt", 1, &mut out);
		assert_eq!(
			out.to_vec(),
			unhex("120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b")
		);
	}

	#[test]
	fn aes128_cfb128_roundtrips_and_preserves_length() {
		// AES-128-CFB-128 (matches the app's AES_cfb128_encrypt): stream
		// cipher, so ciphertext length == plaintext length (no padding),
		// and decrypt with the same key/iv recovers the plaintext.
		use cfb_mode::cipher::AsyncStreamCipher;
		let key = unhex("000102030405060708090a0b0c0d0e0f");
		let iv = unhex("101112131415161718191a1b1c1d1e1f");
		let pt = b"hello sigv3 proof"; // 17 bytes (not block-aligned)
		let mut buf = pt.to_vec();
		Aes128CfbEnc::new(
			GenericArray::from_slice(&key),
			GenericArray::from_slice(&iv),
		)
		.encrypt(&mut buf);
		assert_eq!(buf.len(), pt.len(), "CFB must not pad");
		assert_ne!(&buf, pt, "must actually encrypt");
		cfb_mode::Decryptor::<aes::Aes128>::new(
			GenericArray::from_slice(&key),
			GenericArray::from_slice(&iv),
		)
		.decrypt(&mut buf);
		assert_eq!(&buf, pt, "CFB decrypt must recover plaintext");
	}

	#[test]
	fn build_extras_is_deterministic_in_shape() {
		// Camera pub = RFC u-coordinate (valid 32-byte point).
		let cam_pub_b64 = B64.encode(unhex(
			"de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f",
		));
		let nonce = "6a2da8d0-IuDIFGJQenDTaXFxRm7w";
		let e = build_sigv3_extras(nonce, &cam_pub_b64, 1000, 1_700_000_000, "", "").unwrap();
		// our pubkey is 32 bytes -> 44 base64 chars.
		assert_eq!(B64.decode(&e.public_key).unwrap().len(), 32);
		// cipherContent decodes; CFB is a stream cipher so its length
		// equals the JSON plaintext length (not block-aligned).
		let ct = B64.decode(&e.cipher_content).unwrap();
		let json = format!(
			r#"{{"nonce":"{nonce}","clientTime":{},"token":{{"p":"","s":""}}}}"#,
			1_700_000_000
		);
		assert_eq!(ct.len(), json.len());
	}

	#[test]
	fn rejects_bad_camera_pubkey() {
		assert!(build_sigv3_extras("pw", "not base64!!", 1000, 0, "", "").is_err());
		assert!(build_sigv3_extras("pw", &B64.encode([0u8; 16]), 1000, 0, "", "").is_err());
	}

	#[test]
	fn rejects_out_of_range_iterations() {
		let cam = B64.encode(unhex(
			"de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f",
		));
		// 0 (missing/garbage field) and an absurd ceiling are rejected;
		// the real value (1000) is accepted.
		assert!(build_sigv3_extras("pw", &cam, 0, 0, "", "").is_err());
		assert!(build_sigv3_extras("pw", &cam, 2_000_000, 0, "", "").is_err());
		assert!(build_sigv3_extras("pw", &cam, 1000, 0, "", "").is_ok());
	}

	#[test]
	fn parse_pl_extracts_pubkey_and_iterations() {
		let pl = "V=1;C=2,N=6,P1=59,P2=v3,P3=X25519,\
		          P4=FkTDv8H1jQKkU/nZkWPfxT8A7JArl7OqWwNQ4jerHCw=,P5=sig,P6=1000;";
		let (pk, iters) = parse_pl(pl).expect("valid v3 pl");
		assert_eq!(pk, "FkTDv8H1jQKkU/nZkWPfxT8A7JArl7OqWwNQ4jerHCw=");
		assert_eq!(iters, 1000);
	}

	#[test]
	fn parse_pl_defaults_iterations_when_absent() {
		// P6 missing -> default 1000.
		let (_, iters) = parse_pl("P2=v3,P4=abc").expect("v3 with pubkey");
		assert_eq!(iters, 1000);
	}

	#[test]
	fn parse_pl_rejects_hostile_or_non_v3() {
		// Not v3 -> None even with a P4.
		assert!(parse_pl("P2=v1,P4=abc,P6=1000").is_none());
		// v3 but no P4 -> None.
		assert!(parse_pl("P2=v3,P6=1000").is_none());
		// Empty / garbage / no key=value pairs -> None.
		assert!(parse_pl("").is_none());
		assert!(parse_pl("garbage;;;,,,").is_none());
		assert!(parse_pl("P2=v3").is_none());
		// Garbage P6 falls back to the default rather than panicking.
		let (pk, iters) = parse_pl("P2=v3,P4=k,P6=notanumber").expect("v3 with pubkey");
		assert_eq!((pk.as_str(), iters), ("k", 1000));
	}
}
