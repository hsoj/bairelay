//! RTSP authentication (Basic + Digest).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use thiserror::Error;

/// Errors produced while validating an `Authorization` header.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
	/// The request did not include an `Authorization` header.
	#[error("missing Authorization header")]
	Missing,
	/// The header could not be parsed (bad base64, bad structure, etc.).
	#[error("malformed Authorization header")]
	Malformed,
	/// The scheme portion of the header is not one we support.
	#[error("unknown auth scheme")]
	UnknownScheme,
	/// The credentials parsed cleanly but did not match any known user.
	#[error("bad credentials")]
	BadCredentials,
	/// The Digest nonce has expired; the client should retry with a fresh one.
	#[error("stale nonce")]
	StaleNonce,
}

/// One user in the credentials store.
#[derive(Debug, Clone)]
pub struct UserCred {
	/// Username the client must present.
	pub name: String,
	/// Plaintext password associated with [`UserCred::name`].
	pub password: String,
}

/// Verify a `Basic` Authorization header value against a credentials list.
///
/// Expects value formatted as `Basic <base64>`. Returns the username on
/// success.
pub fn verify_basic<'a>(authz: &str, users: &'a [UserCred]) -> Result<&'a str, AuthError> {
	let rest = authz
		.strip_prefix_ignore_case("Basic ")
		.ok_or(AuthError::UnknownScheme)?;
	let decoded = BASE64
		.decode(rest.trim())
		.map_err(|_| AuthError::Malformed)?;
	let s = std::str::from_utf8(&decoded).map_err(|_| AuthError::Malformed)?;
	let (user, pass) = s.split_once(':').ok_or(AuthError::Malformed)?;
	for u in users {
		if u.name == user && u.password == pass {
			return Ok(&u.name);
		}
	}
	Err(AuthError::BadCredentials)
}

trait StrExt {
	fn strip_prefix_ignore_case<'a>(&'a self, prefix: &str) -> Option<&'a str>;
}

impl StrExt for str {
	fn strip_prefix_ignore_case<'a>(&'a self, prefix: &str) -> Option<&'a str> {
		if self.len() >= prefix.len() && self[..prefix.len()].eq_ignore_ascii_case(prefix) {
			Some(&self[prefix.len()..])
		} else {
			None
		}
	}
}

/// Build a Basic challenge header value for a `WWW-Authenticate:` response.
pub fn build_basic_challenge(realm: &str) -> String {
	format!("Basic realm=\"{realm}\"")
}

/// A server-managed nonce with a creation time for staleness checks.
#[derive(Debug, Clone)]
pub struct Nonce {
	/// Hex-encoded random value presented to the client.
	pub value: String,
	/// Time at which this nonce was minted.
	pub created: std::time::Instant,
}

impl Nonce {
	/// Mint a fresh nonce from 16 random bytes.
	pub fn random() -> Self {
		use rand::Rng;
		let bytes: [u8; 16] = rand::thread_rng().gen();
		let value = bytes.iter().map(|b| format!("{b:02x}")).collect();
		Self {
			value,
			created: std::time::Instant::now(),
		}
	}

	/// Return true if this nonce is older than `ttl`.
	pub fn is_stale(&self, ttl: std::time::Duration) -> bool {
		self.created.elapsed() >= ttl
	}
}

/// Compute the MD5 Digest response per RFC 7616 qop=auth.
///
/// `HA1 = MD5(username:realm:password)`
/// `HA2 = MD5(method:uri)`
/// `response = MD5(HA1:nonce:nc:cnonce:qop:HA2)`
#[allow(clippy::too_many_arguments)]
pub fn digest_response(
	username: &str,
	realm: &str,
	password: &str,
	method: &str,
	uri: &str,
	nonce: &str,
	nc: &str,
	cnonce: &str,
	qop: &str,
) -> String {
	let ha1 = md5_hex(format!("{username}:{realm}:{password}").as_bytes());
	let ha2 = md5_hex(format!("{method}:{uri}").as_bytes());
	md5_hex(format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}").as_bytes())
}

/// Hex-encode the MD5 digest of `input`.
fn md5_hex(input: &[u8]) -> String {
	let digest = md5::compute(input);
	format!("{digest:x}")
}

/// Build a Digest challenge header value for a `WWW-Authenticate:` response.
pub fn build_digest_challenge(realm: &str, nonce: &Nonce, stale: bool) -> String {
	let stale_str = if stale { ",stale=true" } else { "" };
	format!(
		"Digest realm=\"{realm}\", qop=\"auth\", nonce=\"{}\", algorithm=MD5{}",
		nonce.value, stale_str,
	)
}

/// Parse a Digest Authorization header into a dictionary of name/value pairs.
pub fn parse_digest_params(
	value: &str,
) -> Result<std::collections::HashMap<String, String>, AuthError> {
	let rest = value
		.strip_prefix_ignore_case("Digest ")
		.ok_or(AuthError::UnknownScheme)?;
	let mut map = std::collections::HashMap::new();
	for part in split_digest_params(rest) {
		let (k, v) = part.split_once('=').ok_or(AuthError::Malformed)?;
		let v = v.trim().trim_matches('"');
		map.insert(k.trim().to_ascii_lowercase(), v.to_string());
	}
	Ok(map)
}

/// Split a digest header's key=value list, respecting quoted strings.
fn split_digest_params(s: &str) -> Vec<String> {
	let mut out = Vec::new();
	let mut cur = String::new();
	let mut in_quotes = false;
	for ch in s.chars() {
		match ch {
			'"' => {
				in_quotes = !in_quotes;
				cur.push(ch);
			}
			',' if !in_quotes => {
				out.push(std::mem::take(&mut cur));
			}
			_ => cur.push(ch),
		}
	}
	if !cur.trim().is_empty() {
		out.push(cur);
	}
	out
}

/// Extract the path-and-query component of an RTSP URI. Accepts both
/// the absolute form (`rtsp://host:port/cam/sub?x=1`) and the path-only
/// form (`/cam/sub?x=1`). The query string is preserved so a client
/// that signs `/cam?role=admin` cannot replay against `/cam?role=guest`.
/// Returns the entire input on failure to find a path — defensive
/// fallback so a malformed URI still produces a stable comparison key.
pub(crate) fn uri_path(uri: &str) -> &str {
	if let Some(scheme_end) = uri.find("://") {
		let after = &uri[scheme_end + 3..];
		match after.find('/') {
			Some(path_start) => &after[path_start..],
			None => "/",
		}
	} else {
		uri
	}
}

/// Compare two RTSP URIs for digest-binding equality. Real-world
/// clients are split between absolute and path-only forms; the path
/// is the load-bearing component for cross-resource replay defence.
fn digest_uri_paths_match(a: &str, b: &str) -> bool {
	uri_path(a) == uri_path(b)
}

/// Verify a Digest Authorization header.
///
/// `request_uri` is the URI from the request line; the function rejects
/// the digest if the header's embedded `uri=` value differs, defending
/// against the "replay a digest computed for /cam1 against /cam2" attack.
/// `nonce_ok` checks both nonce equality and staleness — caller composes
/// the active-value match with `Nonce::is_stale(ttl)`.
///
/// Returns the username on success.
pub fn verify_digest<'a, F>(
	authz: &str,
	method: &str,
	request_uri: &str,
	users: &'a [UserCred],
	realm: &str,
	nonce_ok: F,
) -> Result<&'a str, AuthError>
where
	F: FnOnce(&str) -> bool,
{
	let params = parse_digest_params(authz)?;
	let username = params.get("username").ok_or(AuthError::Malformed)?;
	let nonce = params.get("nonce").ok_or(AuthError::Malformed)?;
	let uri = params.get("uri").ok_or(AuthError::Malformed)?;
	let response = params.get("response").ok_or(AuthError::Malformed)?;
	let qop = params.get("qop").cloned().unwrap_or_default();
	let nc = params.get("nc").cloned().unwrap_or_default();
	let cnonce = params.get("cnonce").cloned().unwrap_or_default();

	if !nonce_ok(nonce) {
		return Err(AuthError::StaleNonce);
	}

	// Bind the digest to the actual request URI — RFC 7616 §3.4 ties HA2
	// to method:uri, so a client that signed for /cam1 must not be able
	// to send the same Authorization header against /cam2. We compare the
	// **path component** (everything after the authority, ignoring query)
	// because real RTSP clients are split: VLC / ffmpeg sign the absolute
	// form (`rtsp://host:port/cam`) while many embedded / Android clients
	// sign the path-only form (`/cam`). A byte-exact comparison would 403
	// half the deployment matrix. The path is the load-bearing segment of
	// the cross-resource replay defence — `/cam1` vs `/cam2` differs in
	// the path regardless of which form either side chose.
	if !digest_uri_paths_match(uri, request_uri) {
		return Err(AuthError::BadCredentials);
	}

	for user in users {
		if &user.name == username {
			let expected = digest_response(
				&user.name,
				realm,
				&user.password,
				method,
				uri,
				nonce,
				&nc,
				&cnonce,
				&qop,
			);
			if expected.eq_ignore_ascii_case(response) {
				return Ok(&user.name);
			}
		}
	}
	Err(AuthError::BadCredentials)
}

#[cfg(test)]
mod basic_tests {
	use super::*;

	fn users() -> Vec<UserCred> {
		vec![
			UserCred {
				name: "alice".into(),
				password: "wonderland".into(),
			},
			UserCred {
				name: "bob".into(),
				password: "builder".into(),
			},
		]
	}

	#[test]
	fn accepts_valid_creds() {
		// base64("alice:wonderland") = YWxpY2U6d29uZGVybGFuZA==
		assert_eq!(
			verify_basic("Basic YWxpY2U6d29uZGVybGFuZA==", &users()).unwrap(),
			"alice"
		);
	}

	#[test]
	fn rejects_wrong_password() {
		// base64("alice:wrong")
		assert!(matches!(
			verify_basic("Basic YWxpY2U6d3Jvbmc=", &users()),
			Err(AuthError::BadCredentials)
		));
	}

	#[test]
	fn rejects_unknown_scheme() {
		assert!(matches!(
			verify_basic("Bearer xxx", &users()),
			Err(AuthError::UnknownScheme)
		));
	}

	#[test]
	fn rejects_malformed_base64() {
		assert!(matches!(
			verify_basic("Basic !!!notbase64!!!", &users()),
			Err(AuthError::Malformed)
		));
	}

	#[test]
	fn challenge_header_format() {
		assert_eq!(
			build_basic_challenge("bairelay"),
			"Basic realm=\"bairelay\""
		);
	}
}

#[cfg(test)]
mod digest_tests {
	use super::*;

	#[test]
	fn digest_response_matches_rfc7616_worked_example() {
		// RFC 7616 Appendix 3.5.1 MD5 example
		let resp = digest_response(
			"Mufasa",
			"http-auth@example.org",
			"Circle of Life",
			"GET",
			"/dir/index.html",
			"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v",
			"00000001",
			"f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
			"auth",
		);
		assert_eq!(resp, "8ca523f5e9506fed4657c9700eebdbec");
	}

	#[test]
	fn nonce_staleness() {
		let n = Nonce::random();
		assert!(!n.is_stale(std::time::Duration::from_secs(60)));
	}

	#[test]
	fn challenge_format() {
		let n = Nonce {
			value: "abcd".into(),
			created: std::time::Instant::now(),
		};
		assert!(build_digest_challenge("bairelay", &n, false).contains("nonce=\"abcd\""));
		assert!(!build_digest_challenge("bairelay", &n, false).contains("stale"));
		assert!(build_digest_challenge("bairelay", &n, true).contains("stale=true"));
	}

	#[test]
	fn parse_params_handles_quoted_commas() {
		let header =
			r#"Digest username="alice", realm="r1, r2", nonce="n1", uri="/foo", response="abcd""#;
		let params = parse_digest_params(header).unwrap();
		assert_eq!(params.get("realm").unwrap(), "r1, r2");
		assert_eq!(params.get("username").unwrap(), "alice");
	}

	#[test]
	fn verify_accepts_correct_response() {
		let users = vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}];
		let realm = "bairelay";
		let nonce = "fixednonce";
		let method = "DESCRIBE";
		let uri = "rtsp://host/cam1";
		let nc = "00000001";
		let cnonce = "mycnonce";
		let qop = "auth";
		let resp = digest_response(
			"alice",
			realm,
			"wonderland",
			method,
			uri,
			nonce,
			nc,
			cnonce,
			qop,
		);
		let authz = format!(
			r#"Digest username="alice", realm="{realm}", nonce="{nonce}", uri="{uri}", response="{resp}", qop={qop}, nc={nc}, cnonce="{cnonce}""#
		);
		let got = verify_digest(&authz, method, uri, &users, realm, |n| n == nonce).unwrap();
		assert_eq!(got, "alice");
	}

	#[test]
	fn verify_rejects_stale_nonce() {
		let users = vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}];
		let authz =
			r#"Digest username="alice", realm="bairelay", nonce="stale", uri="/", response="xxx""#;
		assert_eq!(
			verify_digest(authz, "OPTIONS", "/", &users, "bairelay", |_| false).err(),
			Some(AuthError::StaleNonce)
		);
	}

	#[test]
	fn verify_rejects_wrong_response() {
		let users = vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}];
		let authz =
			r#"Digest username="alice", realm="bairelay", nonce="n", uri="/", response="abcd""#;
		assert_eq!(
			verify_digest(authz, "OPTIONS", "/", &users, "bairelay", |_| true).err(),
			Some(AuthError::BadCredentials)
		);
	}

	#[test]
	fn verify_rejects_uri_mismatch() {
		// RFC 7616 §3.4: HA2 binds method:uri. If the URI in the
		// `Authorization` header doesn't match the request line, an
		// attacker could replay a digest computed for /cam1 against
		// /cam2. Reject before the cryptographic comparison.
		let users = vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}];
		let realm = "bairelay";
		let nonce = "fixednonce";
		let method = "DESCRIBE";
		let header_uri = "rtsp://host/cam1";
		let request_uri = "rtsp://host/cam2"; // different
		let resp = digest_response(
			"alice",
			realm,
			"wonderland",
			method,
			header_uri,
			nonce,
			"00000001",
			"cn",
			"auth",
		);
		let authz = format!(
			r#"Digest username="alice", realm="{realm}", nonce="{nonce}", uri="{header_uri}", response="{resp}", qop=auth, nc=00000001, cnonce="cn""#
		);
		assert_eq!(
			verify_digest(&authz, method, request_uri, &users, realm, |n| n == nonce).err(),
			Some(AuthError::BadCredentials)
		);
	}

	#[test]
	fn uri_path_extracts_from_absolute_form() {
		assert_eq!(super::uri_path("rtsp://host:8554/cam1"), "/cam1");
		assert_eq!(super::uri_path("rtsps://host/cam1/sub"), "/cam1/sub");
		assert_eq!(super::uri_path("rtsp://host"), "/");
	}

	#[test]
	fn uri_path_passthrough_for_path_only_form() {
		assert_eq!(super::uri_path("/cam1"), "/cam1");
		assert_eq!(super::uri_path("/cam1/sub?x=1"), "/cam1/sub?x=1");
	}

	#[test]
	fn verify_accepts_path_only_digest_against_absolute_request_uri() {
		// The interop case: client signed `/cam1`, server saw
		// `rtsp://host:8554/cam1` on the request line. Both have the
		// same path component → match.
		let users = vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}];
		let realm = "bairelay";
		let nonce = "fixednonce";
		let method = "DESCRIBE";
		let header_uri = "/cam1";
		let request_uri = "rtsp://host:8554/cam1";
		let resp = digest_response(
			"alice",
			realm,
			"wonderland",
			method,
			header_uri,
			nonce,
			"00000001",
			"cn",
			"auth",
		);
		let authz = format!(
			r#"Digest username="alice", realm="{realm}", nonce="{nonce}", uri="{header_uri}", response="{resp}", qop=auth, nc=00000001, cnonce="cn""#
		);
		let got = verify_digest(&authz, method, request_uri, &users, realm, |n| n == nonce)
			.expect("path-only digest must accept against absolute request URI");
		assert_eq!(got, "alice");
	}

	#[test]
	fn verify_accepts_absolute_digest_against_path_only_request_uri() {
		// Symmetric case (less common, but RFC-permissible).
		let users = vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}];
		let realm = "bairelay";
		let nonce = "fixednonce";
		let method = "PLAY";
		let header_uri = "rtsp://host:8554/cam1";
		let request_uri = "/cam1";
		let resp = digest_response(
			"alice",
			realm,
			"wonderland",
			method,
			header_uri,
			nonce,
			"00000001",
			"cn",
			"auth",
		);
		let authz = format!(
			r#"Digest username="alice", realm="{realm}", nonce="{nonce}", uri="{header_uri}", response="{resp}", qop=auth, nc=00000001, cnonce="cn""#
		);
		let got = verify_digest(&authz, method, request_uri, &users, realm, |n| n == nonce)
			.expect("absolute digest must accept against path-only request URI");
		assert_eq!(got, "alice");
	}

	#[test]
	fn verify_rejects_path_mismatch_across_forms() {
		// The replay defence still fires when the paths differ even if
		// one side is absolute and the other path-only.
		let users = vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}];
		let realm = "bairelay";
		let nonce = "fixednonce";
		let method = "DESCRIBE";
		let header_uri = "rtsp://host:8554/cam1";
		let request_uri = "/cam2"; // different path
		let resp = digest_response(
			"alice",
			realm,
			"wonderland",
			method,
			header_uri,
			nonce,
			"00000001",
			"cn",
			"auth",
		);
		let authz = format!(
			r#"Digest username="alice", realm="{realm}", nonce="{nonce}", uri="{header_uri}", response="{resp}", qop=auth, nc=00000001, cnonce="cn""#
		);
		assert_eq!(
			verify_digest(&authz, method, request_uri, &users, realm, |n| n == nonce).err(),
			Some(AuthError::BadCredentials)
		);
	}

	#[test]
	fn verify_accepts_when_uri_matches() {
		let users = vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}];
		let realm = "bairelay";
		let nonce = "fixednonce";
		let method = "PLAY";
		let uri = "rtsp://host/cam1";
		let resp = digest_response(
			"alice",
			realm,
			"wonderland",
			method,
			uri,
			nonce,
			"00000001",
			"cn",
			"auth",
		);
		let authz = format!(
			r#"Digest username="alice", realm="{realm}", nonce="{nonce}", uri="{uri}", response="{resp}", qop=auth, nc=00000001, cnonce="cn""#
		);
		// header uri matches request uri → accept.
		let got = verify_digest(&authz, method, uri, &users, realm, |n| n == nonce).unwrap();
		assert_eq!(got, "alice");
	}

	// Auth-header pathologies — confirm hostile/empty inputs reject
	// safely without panic.

	fn alice() -> Vec<UserCred> {
		vec![UserCred {
			name: "alice".into(),
			password: "wonderland".into(),
		}]
	}

	#[test]
	fn verify_basic_rejects_empty_header() {
		assert!(matches!(
			verify_basic("", &alice()),
			Err(AuthError::UnknownScheme)
		));
	}

	#[test]
	fn verify_basic_rejects_scheme_only() {
		// "Basic " with no base64 → empty payload → split_once finds no `:` → Malformed
		assert!(matches!(
			verify_basic("Basic ", &alice()),
			Err(AuthError::Malformed)
		));
	}

	#[test]
	fn verify_basic_rejects_scheme_only_no_space() {
		// "Basic" without trailing space — strip_prefix_ignore_case("Basic ") fails
		assert!(matches!(
			verify_basic("Basic", &alice()),
			Err(AuthError::UnknownScheme)
		));
	}

	#[test]
	fn verify_digest_rejects_empty_header() {
		assert!(matches!(
			verify_digest("", "OPTIONS", "/", &alice(), "bairelay", |_| true),
			Err(AuthError::UnknownScheme)
		));
	}

	#[test]
	fn verify_digest_rejects_scheme_only() {
		// "Digest " with no params → split_once on `=` fails on empty parts
		// → Malformed (or UnknownScheme, depending on the parser path).
		let err = verify_digest("Digest ", "OPTIONS", "/", &alice(), "bairelay", |_| true)
			.expect_err("scheme-only digest must reject");
		assert!(
			matches!(err, AuthError::Malformed | AuthError::UnknownScheme),
			"got {err:?}"
		);
	}

	#[test]
	fn verify_digest_rejects_concatenated_schemes() {
		// "Basic <b64> Digest username=..." — strip_prefix matches Basic,
		// but verify_basic is called in connection.rs only when the
		// header starts with `basic ` (case-insensitive). At the auth.rs
		// boundary, verify_digest sees a string starting with `Basic`
		// and reports UnknownScheme. Either way: no panic, reject.
		let mixed = r#"Basic YWxpY2U6d29uZGVybGFuZA== Digest username="alice""#;
		assert!(matches!(
			verify_digest(mixed, "OPTIONS", "/", &alice(), "bairelay", |_| true),
			Err(AuthError::UnknownScheme)
		));
	}

	#[test]
	fn verify_digest_rejects_huge_username_without_panic() {
		// 16 KiB username inside the digest header. Must parse to a
		// well-formed map (or reject at the param parser) — never panic.
		let big = "a".repeat(16 * 1024);
		let authz = format!(
			r#"Digest username="{big}", realm="bairelay", nonce="n", uri="/", response="x", qop=auth, nc=00000001, cnonce="c""#
		);
		// Either Malformed or BadCredentials is acceptable. The
		// safety property is "no panic, no infinite loop, returns Err".
		let err = verify_digest(&authz, "OPTIONS", "/", &alice(), "bairelay", |_| true)
			.expect_err("must reject");
		assert!(
			matches!(err, AuthError::BadCredentials | AuthError::Malformed),
			"got {err:?}"
		);
	}

	#[test]
	fn parse_digest_params_rejects_no_equals_pair() {
		// `Digest foo` — no `=` in the value list → Malformed.
		let r = parse_digest_params("Digest foo");
		assert_eq!(r.err(), Some(AuthError::Malformed));
	}

	#[test]
	fn parse_digest_params_handles_empty_quoted_value() {
		// `Digest username=""` — empty quoted value parses cleanly.
		let r = parse_digest_params(r#"Digest username="""#).unwrap();
		assert_eq!(r.get("username").map(String::as_str), Some(""));
	}

	#[test]
	fn nonce_is_stale_after_ttl() {
		// Mint a nonce with a creation time well in the past. The
		// closure-driven staleness path in `verify_digest` translates
		// `nonce_ok = false` into `StaleNonce`; the wiring in the
		// connection layer composes that with `Nonce::is_stale(ttl)`.
		let stale = Nonce {
			value: "old".into(),
			created: std::time::Instant::now()
				.checked_sub(std::time::Duration::from_secs(3600))
				.expect("checked_sub of 1 hour from `now` always succeeds at runtime"),
		};
		assert!(stale.is_stale(std::time::Duration::from_secs(300)));
	}
}
