//! Per-TCP-connection RTSP server task.
//!
//! Reads RTSP requests from the peer, dispatches to handlers that
//! mutate `ConnectionState`, and writes responses back. TCP-interleaved
//! RTP/RTCP is written on the same stream via the session send loop,
//! which shares the write half via an `Arc<Mutex<_>>` so the
//! request-handling side and each track's send path can both frame
//! packets onto it without tearing the connection.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::provider::StreamProvider;
use crate::rtsp::auth::{Nonce, UserCred};
use crate::server::registry::SessionRegistry;
use crate::server::udp_pool::UdpPortPool;

/// Shared per-connection state.
///
/// Fields are populated here but most are consumed by handlers wired in
/// Tasks 10–14; the per-field `#[allow(dead_code)]` attributes suppress the
/// transient warnings until their task lands.
pub(crate) struct ConnectionState {
	pub provider: Arc<dyn StreamProvider>,
	pub users: Vec<UserCred>,
	pub realm: String,
	pub current_nonce: Mutex<Nonce>,
	pub sessions: Arc<SessionRegistry>,
	pub udp_pool: Arc<UdpPortPool>,
	pub server_bind_ip: std::net::IpAddr,
	/// IP the TCP connection was accepted from. Used as the destination for
	/// UDP RTP/RTCP when a SETUP picks the UDP transport. Falls back to
	/// loopback if `peer_addr()` fails on the stream (unusual).
	pub peer_ip: std::net::IpAddr,
	/// Local IP this connection's TCP socket was accepted on. Used as the
	/// UDP bind address when `server_bind_ip` is unspecified (wildcard),
	/// so that RTP packets have a source IP consistent with the RTSP
	/// TCP connection — strict firewalls on multi-homed hosts otherwise
	/// drop mismatched-source RTP.
	pub local_ip: std::net::IpAddr,
	/// `true` when this connection is wrapped in TLS (`rtsps://`). Drives
	/// the scheme-mismatch defence in the request dispatcher and lets log
	/// messages identify the transport.
	pub is_tls: bool,
	pub writer: Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>>,
}

/// Maximum bytes we'll read into the RTSP request buffer.
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Lifetime of a Digest nonce before the server forces a fresh one.
/// Five minutes balances replay-window vs. challenge-storm: short enough
/// that a captured `Authorization` is useless after a coffee break,
/// long enough that a normal RTSP session never trips re-challenge.
const NONCE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Slow-loris cap: a fresh TCP connection (no sessions yet) must
/// receive a parseable RTSP request within this window — and another
/// one within the same window after each request — or the handler
/// closes the socket. Rolling, not one-shot: a client that sends a
/// single OPTIONS and idles still gets reaped 30 s later. Once at
/// least one session exists, the deadline is suppressed (the session
/// keepalive watchdog handles idle reaping with its own 30 s sweep).
const INITIAL_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Handle a single connection until EOF, error, or cancellation.
///
/// Generic over any `AsyncRead + AsyncWrite` stream so the same handler
/// serves plain `TcpStream` and `TlsStream<TcpStream>`. The listener
/// captures `peer_ip` / `local_ip` from the underlying TCP socket *before*
/// any TLS wrap and passes them in — TLS-wrapped types do not expose
/// `peer_addr` / `local_addr` directly.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all, fields(peer = %peer_ip, tls = is_tls))]
pub async fn handle_connection<S>(
	stream: S,
	provider: Arc<dyn StreamProvider>,
	users: Vec<UserCred>,
	realm: String,
	udp_pool: Arc<UdpPortPool>,
	server_bind_ip: std::net::IpAddr,
	peer_ip: std::net::IpAddr,
	local_ip: std::net::IpAddr,
	is_tls: bool,
	cancel: CancellationToken,
) where
	S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
	let (mut read_half, write_half) = tokio::io::split(stream);
	let writer: Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
		Arc::new(Mutex::new(Box::new(write_half)));

	let state = Arc::new(ConnectionState {
		provider,
		users,
		realm,
		current_nonce: Mutex::new(Nonce::random()),
		sessions: Arc::new(SessionRegistry::new()),
		udp_pool,
		server_bind_ip,
		peer_ip,
		local_ip,
		is_tls,
		writer: Arc::clone(&writer),
	});

	let mut buf = Vec::with_capacity(4096);
	// Fires every 5 seconds to reap sessions idle for >30 seconds. The
	// first tick fires immediately, which harmlessly sweeps an empty
	// registry before the client has even issued SETUP. Use Delay so
	// a momentarily stalled handler doesn't cause a burst of catch-up
	// ticks when control returns.
	let mut keepalive_ticker = tokio::time::interval(std::time::Duration::from_secs(5));
	keepalive_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
	// Slow-loris guard: a fresh connection that goes silent — either
	// before its first request, or for an extended period between
	// requests with no session active — must not pin an accept slot.
	// Tracks the last activity (handler-entry time, then refreshed on
	// each successful dispatch). The arm fires only when no sessions
	// exist; once SETUP creates a session, the keepalive watchdog
	// handles idle reaping.
	let mut last_activity = tokio::time::Instant::now();
	'conn: loop {
		let pre_session = state.sessions.is_empty();
		let deadline = last_activity + INITIAL_REQUEST_TIMEOUT;
		tokio::select! {
			_ = tokio::time::sleep_until(deadline), if pre_session => {
				tracing::warn!(
					timeout_secs = INITIAL_REQUEST_TIMEOUT.as_secs(),
					"slow-loris: closing connection idle past pre-session deadline"
				);
				break;
			}
			_ = keepalive_ticker.tick() => {
				let removed = state.sessions.sweep_expired(std::time::Duration::from_secs(30));
				for id in removed {
					tracing::debug!(session = %id, "session timed out");
				}
			}
			_ = cancel.cancelled() => {
				tracing::debug!("connection cancelled by server shutdown");
				break;
			}
			result = read_once_or_more(&mut read_half, &mut buf) => {
				match result {
					Ok(0) => {
						tracing::debug!("connection closed by peer");
						break;
					}
					Ok(_) => {
						// Drain *every* complete request the read yielded, not
						// just the first. Pipelining is legal (RFC 7826 §9.2)
						// and a single read also coalesces whatever the peer
						// had in flight. Handling one per read parks the rest
						// in `buf` until the next inbound byte arrives — a
						// client that pipelines and then blocks for all its
						// responses deadlocks until the slow-loris arm fires,
						// or forever once a session exists and that arm is
						// disabled.
						loop {
							// Re-checked per request: a 64 KiB buffer holds
							// thousands of minimal requests, and dispatch is
							// not cancel-aware. Without this, shutdown waits
							// out the whole drain.
							if cancel.is_cancelled() {
								tracing::debug!("connection cancelled by server shutdown");
								break 'conn;
							}
							match try_consume_request(&mut buf) {
								Ok(Some(req_bytes)) => {
									if let Err(e) = crate::server::connection::dispatch_request(
										&state, &req_bytes,
									).await {
										tracing::warn!(error = %e, "request handler error");
										break 'conn;
									}
									// Refresh the rolling deadline: the
									// connection has shown activity, so
									// give it another full window before
									// the slow-loris arm can fire.
									last_activity = tokio::time::Instant::now();
								}
								Ok(None) => {
									// Need more bytes. Any trailing partial
									// request stays buffered for the next read.
									if buf.len() > MAX_REQUEST_BYTES {
										tracing::warn!("request buffer exceeded limit, closing");
										break 'conn;
									}
									break;
								}
								Err(e) => {
									tracing::warn!(error = %e, "malformed RTSP request, closing");
									// Send 400 Bad Request once then close.
									let resp = crate::rtsp::message::build_response(
										rtsp_types::StatusCode::BadRequest,
										0,
										&[],
										None,
									);
									let mut w = writer.lock().await;
									let _ = w.write_all(&resp).await;
									let _ = w.flush().await;
									break 'conn;
								}
							}
						}
					}
					Err(e) => {
						tracing::debug!(error = %e, "read error");
						break;
					}
				}
			}
		}
	}

	// Connection closing — tear down any live sessions to release wake locks.
	state.sessions.clear();
}

async fn read_once_or_more<R>(read_half: &mut R, buf: &mut Vec<u8>) -> io::Result<usize>
where
	R: AsyncRead + Unpin,
{
	let mut tmp = [0u8; 4096];
	let n = read_half.read(&mut tmp).await?;
	if n == 0 {
		return Ok(0);
	}
	buf.extend_from_slice(&tmp[..n]);
	Ok(n)
}

/// Attempt to slice out exactly one RTSP request from `buf`. Returns:
/// - `Ok(Some(bytes))` with the request bytes removed from `buf`,
/// - `Ok(None)` if the buffer doesn't yet contain a complete request,
/// - `Err(...)` on unrecoverable parse error.
fn try_consume_request(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
	// Minimum viable request ends at the blank CRLF after headers.
	// Find \r\n\r\n; fall back to rtsp-types which may support partial.
	if let Some(pos) = find_double_crlf(buf) {
		let end = pos + 4;
		// Look for Content-Length header to include body in the slice.
		let headers = &buf[..end];
		let body_len = parse_content_length(headers).unwrap_or(0);
		// Reject oversize / overflowing Content-Length explicitly.
		// Without this, `end + body_len` overflows in debug builds and
		// wraps in release, which the buffer-cap check at the call site
		// would only catch on a re-entry — and only if the wrap landed
		// above `MAX_REQUEST_BYTES`. Cap defensively here.
		if body_len > MAX_REQUEST_BYTES {
			return Err(format!(
				"Content-Length {body_len} exceeds maximum {MAX_REQUEST_BYTES}"
			));
		}
		let total = end
			.checked_add(body_len)
			.ok_or_else(|| "request size overflows usize".to_string())?;
		if total > MAX_REQUEST_BYTES {
			return Err(format!(
				"request size {total} exceeds maximum {MAX_REQUEST_BYTES}"
			));
		}
		if buf.len() < total {
			return Ok(None);
		}
		let req_bytes: Vec<u8> = buf.drain(..total).collect();
		return Ok(Some(req_bytes));
	}
	Ok(None)
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
	buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
	let s = std::str::from_utf8(headers).ok()?;
	for line in s.split("\r\n") {
		if let Some((name, value)) = line.split_once(':') {
			if name.eq_ignore_ascii_case("Content-Length") {
				return value.trim().parse().ok();
			}
		}
	}
	None
}

/// Dispatch a parsed request to the right handler.
///
/// Parses `req_bytes` as a single RTSP request, then matches on
/// [`RtspMethod`] to invoke the per-method handler. On a parse failure
/// we emit a `400 Bad Request` with `CSeq: 0` and return an error so
/// the connection loop closes the socket.
#[tracing::instrument(skip_all)]
pub(crate) async fn dispatch_request(
	state: &Arc<ConnectionState>,
	req_bytes: &[u8],
) -> io::Result<()> {
	use crate::rtsp::message::{build_response, parse_request, RtspMethod};

	let parsed = match parse_request(req_bytes) {
		Ok(p) => p,
		Err(e) => {
			let resp = build_response(rtsp_types::StatusCode::BadRequest, 0, &[], None);
			write_response(state, &resp).await?;
			return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
		}
	};

	// Reject requests whose URI scheme contradicts the actual transport.
	// A `rtsps://` URI on a plain TCP listener (or `rtsp://` on a TLS
	// listener) signals a confused or hostile client routing across the
	// two parallel listeners; 400 it before any handler fires.
	if !scheme_matches_transport(&parsed.uri, state.is_tls) {
		tracing::warn!(uri = %parsed.uri, is_tls = state.is_tls, "scheme/transport mismatch");
		let resp = build_response(rtsp_types::StatusCode::BadRequest, parsed.cseq, &[], None);
		return write_response(state, &resp).await;
	}

	// Refresh session idle timer so the keepalive sweep doesn't reap an
	// active client. GET_PARAMETER is the classic ffmpeg/VLC heartbeat, but
	// any request carrying a Session header counts as activity.
	if let Some(sid) = parsed.session.as_deref() {
		state.sessions.touch(sid);
	}

	let cseq = parsed.cseq;
	match parsed.method {
		RtspMethod::Options => handle_options(state, cseq).await,
		RtspMethod::GetParameter => handle_getparameter(state, cseq, &parsed).await,
		RtspMethod::Pause => handle_pause(state, cseq, &parsed).await,
		RtspMethod::Describe => handle_describe(state, cseq, &parsed).await,
		RtspMethod::Setup => handle_setup(state, cseq, &parsed).await,
		RtspMethod::Play => handle_play(state, cseq, &parsed).await,
		RtspMethod::Teardown => handle_teardown(state, cseq, &parsed).await,
	}
}

async fn write_response(state: &Arc<ConnectionState>, resp: &[u8]) -> io::Result<()> {
	let mut w = state.writer.lock().await;
	w.write_all(resp).await?;
	w.flush().await?;
	Ok(())
}

async fn handle_options(state: &Arc<ConnectionState>, cseq: u32) -> io::Result<()> {
	let resp = crate::rtsp::message::build_response(
		rtsp_types::StatusCode::Ok,
		cseq,
		&[(
			"Public",
			"OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN, GET_PARAMETER, PAUSE".to_string(),
		)],
		None,
	);
	write_response(state, &resp).await
}

async fn handle_getparameter(
	state: &Arc<ConnectionState>,
	cseq: u32,
	parsed: &crate::rtsp::message::ParsedRequest,
) -> io::Result<()> {
	// Used as session keepalive; no body required.
	let mut extra = vec![];
	if let Some(sid) = &parsed.session {
		extra.push(("Session", format!("{sid};timeout=30")));
	}
	let resp = crate::rtsp::message::build_response(rtsp_types::StatusCode::Ok, cseq, &extra, None);
	write_response(state, &resp).await
}

async fn handle_pause(
	state: &Arc<ConnectionState>,
	cseq: u32,
	parsed: &crate::rtsp::message::ParsedRequest,
) -> io::Result<()> {
	// Live stream: PAUSE is a no-op on the send loop; return 200 OK.
	let mut extra = vec![];
	if let Some(sid) = &parsed.session {
		extra.push(("Session", sid.clone()));
	}
	let resp = crate::rtsp::message::build_response(rtsp_types::StatusCode::Ok, cseq, &extra, None);
	write_response(state, &resp).await
}

async fn handle_describe(
	state: &Arc<ConnectionState>,
	cseq: u32,
	parsed: &crate::rtsp::message::ParsedRequest,
) -> io::Result<()> {
	// Parse URL → (camera, stream kind).
	let path = extract_path(&parsed.uri);
	let Some(resolved) = crate::url::resolve(&path) else {
		let resp =
			crate::rtsp::message::build_response(rtsp_types::StatusCode::NotFound, cseq, &[], None);
		return write_response(state, &resp).await;
	};

	// Auth gate. Empty users list = no auth required.
	// When auth is required, capture the authenticated username so it
	// can be forwarded to the provider for per-camera ACL enforcement.
	let authenticated_user: Option<String> = if state.users.is_empty() {
		None
	} else {
		match authenticate(state, parsed, "DESCRIBE").await {
			AuthOutcome::Ok(user) => Some(user),
			AuthOutcome::Challenge(headers) => {
				let resp = crate::rtsp::message::build_response(
					rtsp_types::StatusCode::Unauthorized,
					cseq,
					&headers,
					None,
				);
				return write_response(state, &resp).await;
			}
			AuthOutcome::Forbidden => {
				let resp = crate::rtsp::message::build_response(
					rtsp_types::StatusCode::Forbidden,
					cseq,
					&[],
					None,
				);
				return write_response(state, &resp).await;
			}
		}
	};

	// Subscribe speculatively to obtain SDP params — then immediately drop the
	// handle to release the wake lock. The real subscription happens in SETUP.
	let sub = match subscribe_with_extern_fallback(
		state,
		&resolved.camera,
		resolved.stream,
		authenticated_user.as_deref(),
	)
	.await
	{
		Ok(s) => s,
		Err(crate::provider::StreamError::UnknownCamera) => {
			let resp = crate::rtsp::message::build_response(
				rtsp_types::StatusCode::NotFound,
				cseq,
				&[],
				None,
			);
			return write_response(state, &resp).await;
		}
		Err(e) => {
			tracing::warn!(error = %e, "provider error during DESCRIBE");
			let resp = crate::rtsp::message::build_response(
				rtsp_types::StatusCode::ServiceUnavailable,
				cseq,
				&[],
				None,
			);
			return write_response(state, &resp).await;
		}
	};

	// Rewrite SDP server_ip/session_id at DESCRIBE time: the upstream
	// StreamSource has no view of the connection's local IP or the
	// per-session identifier a client should see. Clients would otherwise
	// see the literal "0.0.0.0" in `o=`/`c=` and the placeholder "0" in
	// `o=` — both harmful for multicast ACLs, NAT detection, and
	// session-matching heuristics.
	let mut sdp_params = sub.sdp_params.clone();
	sdp_params.server_ip = advertised_server_ip(state).to_string();
	sdp_params.session_id = generate_session_id_for_sdp();
	let sdp = crate::sdp::build(&sdp_params);
	// Drop sub now; we don't want to hold a wake lock for just DESCRIBE.
	drop(sub);

	let resp = crate::rtsp::message::build_response(
		rtsp_types::StatusCode::Ok,
		cseq,
		&[
			("Content-Type", "application/sdp".to_string()),
			(
				"Content-Base",
				// Content-Base must echo the presentation URI so that the
				// relative track-control URIs in the SDP (`trackID=N`)
				// resolve to `<DESCRIBE URI>/trackID=N`. Emitting a bare
				// `rtsp://host:port/` instead drops the camera segment
				// and ffmpeg/mpv/gstreamer compute SETUP URIs that
				// resolve to 404.
				if parsed.uri.ends_with('/') {
					parsed.uri.clone()
				} else {
					format!("{}/", parsed.uri)
				},
			),
		],
		Some(sdp.as_bytes()),
	);
	write_response(state, &resp).await
}

/// The server IP to advertise to clients.
///
/// If the listener was bound to a specific IP, we trust that. If it was
/// bound to wildcard (0.0.0.0 or ::) we substitute the TCP connection's
/// local IP — which the kernel resolved for this specific client — so
/// clients on different networks see sensible values.
fn advertised_server_ip(state: &Arc<ConnectionState>) -> std::net::IpAddr {
	if state.server_bind_ip.is_unspecified() {
		state.local_ip
	} else {
		state.server_bind_ip
	}
}

/// Generate a fresh numeric SDP session ID for the `o=` line.
///
/// Uses Unix seconds by convention — RFC 4566 just requires "uniqueness
/// within the originator context" and any monotonically increasing
/// integer satisfies that for a long-lived server. Falls back to 1 if
/// the system clock is pre-epoch (won't happen in practice).
fn generate_session_id_for_sdp() -> String {
	use std::time::{SystemTime, UNIX_EPOCH};
	let secs = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_secs())
		.unwrap_or(1);
	format!("{secs}")
}

fn extract_path(uri: &str) -> String {
	// URI might be absolute (rtsp://host:port/camera/sub) or relative.
	if let Some(scheme_end) = uri.find("://") {
		let after = &uri[scheme_end + 3..];
		if let Some(path_start) = after.find('/') {
			return after[path_start..].to_string();
		}
		return "/".to_string();
	}
	uri.to_string()
}

/// Returns `true` if the URI's scheme is consistent with `is_tls`. URIs
/// without a scheme (relative form, used for some session-bound requests)
/// always pass — clients are allowed to send relative URIs once they have
/// issued an absolute one.
pub(crate) fn scheme_matches_transport(uri: &str, is_tls: bool) -> bool {
	if uri.starts_with("rtsp://") {
		!is_tls
	} else if uri.starts_with("rtsps://") {
		is_tls
	} else {
		// Relative URI / unknown scheme — accept (downstream handlers can
		// still 4xx anything semantically wrong).
		true
	}
}

enum AuthOutcome {
	/// Authenticated username. Forwarded to the provider via
	/// `subscribe(..., authenticated_user)` so per-camera ACLs
	/// (`permitted_users`) can be enforced.
	Ok(String),
	Challenge(Vec<(&'static str, String)>), // 401 with WWW-Authenticate
	Forbidden,                              // 403
}

async fn authenticate(
	state: &Arc<ConnectionState>,
	parsed: &crate::rtsp::message::ParsedRequest,
	method: &str,
) -> AuthOutcome {
	use crate::rtsp::auth::{
		build_basic_challenge, build_digest_challenge, verify_basic, verify_digest, AuthError,
	};
	let nonce = state.current_nonce.lock().await.clone();
	let Some(authz) = parsed.authorization.as_deref() else {
		// No credentials provided → challenge.
		return AuthOutcome::Challenge(vec![
			(
				"WWW-Authenticate",
				build_digest_challenge(&state.realm, &nonce, false),
			),
			("WWW-Authenticate", build_basic_challenge(&state.realm)),
		]);
	};
	if authz.to_ascii_lowercase().starts_with("basic ") {
		match verify_basic(authz, &state.users) {
			Ok(user) => return AuthOutcome::Ok(user.to_string()),
			Err(AuthError::BadCredentials) => return AuthOutcome::Forbidden,
			Err(_) => {
				return AuthOutcome::Challenge(vec![(
					"WWW-Authenticate",
					build_basic_challenge(&state.realm),
				)]);
			}
		}
	}
	match verify_digest(
		authz,
		method,
		&parsed.uri,
		&state.users,
		&state.realm,
		|n| n == nonce.value && !nonce.is_stale(NONCE_TTL),
	) {
		Ok(user) => AuthOutcome::Ok(user.to_string()),
		Err(AuthError::StaleNonce) => {
			let fresh = crate::rtsp::auth::Nonce::random();
			*state.current_nonce.lock().await = fresh.clone();
			AuthOutcome::Challenge(vec![(
				"WWW-Authenticate",
				build_digest_challenge(&state.realm, &fresh, true),
			)])
		}
		Err(AuthError::BadCredentials) => AuthOutcome::Forbidden,
		Err(_) => AuthOutcome::Challenge(vec![(
			"WWW-Authenticate",
			build_digest_challenge(&state.realm, &nonce, false),
		)]),
	}
}

async fn handle_setup(
	state: &Arc<ConnectionState>,
	cseq: u32,
	parsed: &crate::rtsp::message::ParsedRequest,
) -> io::Result<()> {
	use crate::rtsp::session::new_session_id;
	use crate::rtsp::transport as transport_mod;
	use crate::server::registry::{TrackEntry, TrackKind};

	let path = extract_path(&parsed.uri);
	// SETUP URIs may include a `trackID=N` suffix. Strip it for base-path
	// resolution AND capture the numeric track index: 0 → Video, !=0 →
	// Audio, matching the order `sdp::build` assigns (video first).
	// Absent or non-numeric track suffix falls back to track 0 (video).
	let (base_path, track_id_num) = path
		.rsplit_once('/')
		.and_then(|(base, last)| {
			last.strip_prefix("trackID=")
				.and_then(|n| n.parse::<u32>().ok())
				.map(|n| (base.to_string(), n))
		})
		.unwrap_or_else(|| (path.clone(), 0));
	let Some(resolved) = crate::url::resolve(&base_path) else {
		let resp =
			crate::rtsp::message::build_response(rtsp_types::StatusCode::NotFound, cseq, &[], None);
		return write_response(state, &resp).await;
	};

	// Authenticate. When auth is required, capture the authenticated
	// username so it can be forwarded to the provider for per-camera ACL
	// enforcement (provider decides whether user is permitted).
	let authenticated_user: Option<String> = if state.users.is_empty() {
		None
	} else {
		match authenticate(state, parsed, "SETUP").await {
			AuthOutcome::Ok(user) => Some(user),
			AuthOutcome::Challenge(headers) => {
				let resp = crate::rtsp::message::build_response(
					rtsp_types::StatusCode::Unauthorized,
					cseq,
					&headers,
					None,
				);
				return write_response(state, &resp).await;
			}
			AuthOutcome::Forbidden => {
				let resp = crate::rtsp::message::build_response(
					rtsp_types::StatusCode::Forbidden,
					cseq,
					&[],
					None,
				);
				return write_response(state, &resp).await;
			}
		}
	};

	// Parse Transport header.
	let Some(transport_header) = parsed.transport.as_deref() else {
		let resp = crate::rtsp::message::build_response(
			rtsp_types::StatusCode::BadRequest,
			cseq,
			&[],
			None,
		);
		return write_response(state, &resp).await;
	};
	let transport_spec = match transport_mod::parse(transport_header) {
		Ok(t) => t,
		Err(_) => {
			let resp = crate::rtsp::message::build_response(
				rtsp_types::StatusCode::UnsupportedTransport,
				cseq,
				&[],
				None,
			);
			return write_response(state, &resp).await;
		}
	};

	// Per-track SSRC. Echoed in the Transport response (RFC 2326 §12.39)
	// and threaded into the session task so RTP packets carry the same
	// value the client was told about. A random SSRC per track keeps
	// video and audio distinguishable on the wire.
	let track_ssrc: u32 = rand::random();

	// Build transport & Transport response header.
	//
	// Built up-front (before the session-existence check) so the
	// append path for a second SETUP against an existing session can
	// construct a TrackEntry without calling
	// `subscribe_with_extern_fallback` a second time. The
	// transport_impl build depends only on `state` + `transport_spec` +
	// `track_ssrc`; it does not need a subscription.
	//
	// Note: the UDP port lease is folded into UdpUnicastTransport
	// itself, so no separate Option<UdpPortLease> is threaded here.
	let (transport_impl, transport_response) =
		match build_transport(state, transport_spec, track_ssrc).await {
			Ok(v) => v,
			Err(status) => {
				let resp = crate::rtsp::message::build_response(status, cseq, &[], None);
				return write_response(state, &resp).await;
			}
		};

	let session_id = parsed.session.clone().unwrap_or_else(new_session_id);
	let track_kind = if track_id_num == 0 {
		TrackKind::Video
	} else {
		TrackKind::Audio
	};

	if state.sessions.contains(&session_id) {
		// Append path: second (or later) SETUP on an existing session ID.
		// Per RFC 7826 §13.3 a client may issue multiple SETUPs against
		// the same session to negotiate additional tracks (video+audio).
		// We append a TrackEntry to the session's track list and return
		// 200 OK with the negotiated Transport header.
		//
		// We deliberately do NOT call `subscribe_with_extern_fallback`
		// here: the existing session's SubscriptionHandle already holds
		// the camera's wake lock, and re-subscribing would acquire a
		// second guard that'd have to be dropped immediately (wasting
		// work and risking wake-lock counter leaks if we got it wrong).
		//
		// State-awareness caveat: RFC 7826 §13.3 technically forbids
		// SETUP while the session is already in Playing state. The
		// current SessionEntry doesn't track Playing-vs-Ready state, so
		// we accept the late append unconditionally. Real clients (VLC,
		// ffmpeg, HA's stream) issue all SETUPs before PLAY, which this
		// path handles cleanly. If tightening is ever needed, add a
		// deliverable for it to `docs/architecture.md` Phased Build Order.
		// Derive clock rate per RFC 3550 for RTCP SR extrapolation.
		// Video always uses 90000; audio uses the sample rate the session
		// already advertised in its SDP (pulled from the existing
		// subscription). If audio SDP wasn't populated (e.g. camera emits
		// video only), fall back to 16000 — a neutral default for Argus
		// AAC-LC. The fallback path shouldn't fire in practice: a client
		// wouldn't SETUP audio without seeing an `m=audio` line in the
		// DESCRIBE response.
		let track_clock_rate = match track_kind {
			TrackKind::Video => 90_000u32,
			TrackKind::Audio => state
				.sessions
				.audio_sample_rate(&session_id)
				.unwrap_or(16_000),
		};
		let track = TrackEntry {
			kind: track_kind,
			transport: Arc::clone(&transport_impl),
			ssrc: track_ssrc,
			clock_rate: track_clock_rate,
		};
		state.sessions.append_track(&session_id, track);
		let resp = crate::rtsp::message::build_response(
			rtsp_types::StatusCode::Ok,
			cseq,
			&[
				("Transport", transport_response),
				("Session", format!("{session_id};timeout=30")),
			],
			None,
		);
		return write_response(state, &resp).await;
	}

	// First SETUP for this session ID — subscribe to the camera (acquires
	// wake lock, waits for SDP to be ready) and spawn the session send
	// loop.
	let subscription = match subscribe_with_extern_fallback(
		state,
		&resolved.camera,
		resolved.stream,
		authenticated_user.as_deref(),
	)
	.await
	{
		Ok(s) => s,
		Err(crate::provider::StreamError::UnknownCamera) => {
			let resp = crate::rtsp::message::build_response(
				rtsp_types::StatusCode::NotFound,
				cseq,
				&[],
				None,
			);
			return write_response(state, &resp).await;
		}
		Err(crate::provider::StreamError::AccessDenied) => {
			let resp = crate::rtsp::message::build_response(
				rtsp_types::StatusCode::Forbidden,
				cseq,
				&[],
				None,
			);
			return write_response(state, &resp).await;
		}
		Err(e) => {
			tracing::warn!(error = %e, "provider error during SETUP");
			let resp = crate::rtsp::message::build_response(
				rtsp_types::StatusCode::ServiceUnavailable,
				cseq,
				&[],
				None,
			);
			return write_response(state, &resp).await;
		}
	};

	// Register the session in the registry, then spawn the send loop with
	// handles pulled back out of the entry. `SessionEntry::new` constructs
	// the `first_video_rtp` slot internally; we pull the `tracks` Arc and
	// the first-video slot back out via registry getters. The send loop
	// reads the shared `Arc<Mutex<Vec<TrackEntry>>>` on every iteration
	// so a late SETUP that calls `append_track` becomes visible without
	// restarting the task.
	let cancel = CancellationToken::new();
	let frames_rx = subscription.frames.resubscribe();
	let last_frame = Arc::clone(&subscription.last_frame);
	let cancel_for_task = cancel.clone();
	let sessions_for_task = Arc::clone(&state.sessions);
	let session_id_for_task = session_id.clone();

	// Derive clock rate per RFC 3550 for RTCP SR extrapolation. See the
	// matching comment in the append path above.
	let first_track_clock_rate = match track_kind {
		TrackKind::Video => 90_000u32,
		TrackKind::Audio => subscription
			.sdp_params
			.audio
			.as_ref()
			.map(|a| a.sample_rate)
			.unwrap_or(16_000),
	};
	let first_track = TrackEntry {
		kind: track_kind,
		transport: Arc::clone(&transport_impl),
		ssrc: track_ssrc,
		clock_rate: first_track_clock_rate,
	};
	state.sessions.insert(
		session_id.clone(),
		crate::server::registry::SessionEntry::new(cancel, subscription, vec![first_track]),
	);
	let session_tracks_for_task = state
		.sessions
		.tracks_arc(&session_id)
		.expect("tracks arc missing immediately after insert");
	let tracks_changed_for_task = state
		.sessions
		.tracks_changed_arc(&session_id)
		.expect("tracks_changed notify missing immediately after insert");
	// Shared slot populated by the session task on its first video packet
	// and read by the PLAY handler to emit `RTP-Info:` (RFC 2326 §12.33).
	let first_video_rtp_for_task = state
		.sessions
		.first_video_rtp(&session_id)
		.expect("session just inserted");
	// PLAY gate — the session task parks on this until the client
	// issues PLAY, so no RTP flows between SETUP and PLAY.
	let (play_signal_for_task, play_fired_for_task) = state
		.sessions
		.play_gate_arc(&session_id)
		.expect("session just inserted");

	tokio::spawn(async move {
		crate::server::session_task::run(
			frames_rx,
			last_frame,
			session_tracks_for_task,
			sessions_for_task,
			session_id_for_task,
			first_video_rtp_for_task,
			cancel_for_task,
			tracks_changed_for_task,
			play_signal_for_task,
			play_fired_for_task,
		)
		.await;
	});

	// Respond 200 OK with Transport and Session headers.
	let resp = crate::rtsp::message::build_response(
		rtsp_types::StatusCode::Ok,
		cseq,
		&[
			("Transport", transport_response),
			("Session", format!("{session_id};timeout=30")),
		],
		None,
	);
	write_response(state, &resp).await
}

/// Construct the per-track transport impl and the Transport response
/// header string the SETUP handler echoes back. Extracted from
/// `handle_setup` so the caller's control flow stays focused on the
/// RTSP state machine.
///
/// Returns `Err(InternalServerError)` on UDP bind failure — the caller
/// folds that into a 500 response via `build_response`.
async fn build_transport(
	state: &Arc<ConnectionState>,
	transport_spec: crate::rtsp::transport::TransportSpec,
	track_ssrc: u32,
) -> Result<(Arc<dyn crate::server::transport::Transport>, String), rtsp_types::StatusCode> {
	use crate::rtsp::transport as transport_mod;
	match transport_spec {
		transport_mod::TransportSpec::TcpInterleaved {
			channel_rtp,
			channel_rtcp,
		} => {
			let t = crate::server::transport::TcpInterleavedTransport::new(
				Arc::clone(&state.writer),
				channel_rtp,
				channel_rtcp,
			);
			let response = transport_mod::build_tcp_response(channel_rtp, channel_rtcp, track_ssrc);
			Ok((Arc::new(t), response))
		}
		transport_mod::TransportSpec::UdpUnicast {
			client_rtp_port,
			client_rtcp_port,
		} => {
			// The client's IP comes from the TCP peer address captured when
			// the connection was accepted (see `handle_connection`).
			use std::net::SocketAddr;
			let client_rtp_addr = SocketAddr::new(state.peer_ip, client_rtp_port);
			let client_rtcp_addr = SocketAddr::new(state.peer_ip, client_rtcp_port);
			// If the server's listener is bound to wildcard, bind the UDP
			// sockets to the TCP connection's local IP so RTP source-IP
			// matches the RTSP 5-tuple on multi-homed hosts. Otherwise use
			// the explicitly configured bind IP.
			let udp_bind_ip = if state.server_bind_ip.is_unspecified() {
				state.local_ip
			} else {
				state.server_bind_ip
			};
			let t = crate::server::transport::UdpUnicastTransport::bind(
				udp_bind_ip,
				Arc::clone(&state.udp_pool),
				client_rtp_addr,
				client_rtcp_addr,
			)
			.await
			.map_err(|_| rtsp_types::StatusCode::InternalServerError)?;
			let response = transport_mod::build_udp_response(
				client_rtp_port,
				client_rtcp_port,
				t.server_rtp_port(),
				t.server_rtcp_port(),
				track_ssrc,
			);
			Ok((Arc::new(t), response))
		}
	}
}

async fn handle_play(
	state: &Arc<ConnectionState>,
	cseq: u32,
	parsed: &crate::rtsp::message::ParsedRequest,
) -> io::Result<()> {
	let Some(sid) = &parsed.session else {
		let resp = crate::rtsp::message::build_response(
			rtsp_types::StatusCode::BadRequest,
			cseq,
			&[],
			None,
		);
		return write_response(state, &resp).await;
	};
	if !state.sessions.contains(sid) {
		let resp = crate::rtsp::message::build_response(
			rtsp_types::StatusCode::SessionNotFound,
			cseq,
			&[],
			None,
		);
		return write_response(state, &resp).await;
	}
	// Release the session send loop — RFC 2326 §10.5 PLAY starts
	// media delivery. Must happen BEFORE we wait on first_video_rtp
	// below, because the task is parked on play_signal and can't
	// have sent its first video packet yet.
	state.sessions.mark_playing(sid);

	// Emit RTP-Info if the session has already sent at least one video
	// packet (RFC 2326 §12.33). The track URI is echoed from the
	// request-URI — real clients (VLC/ffmpeg) expect this exact value
	// when mapping RTP-Info entries to SDP tracks. If the first packet
	// hasn't been transmitted yet we skip the header; most clients
	// tolerate the absence. We wait briefly (up to ~200 ms) to let the
	// send loop record its first packet — after mark_playing the task
	// unparks and the cached-burst replay typically completes well
	// within that window.
	let mut headers: Vec<(&'static str, String)> =
		vec![("Session", sid.clone()), ("Range", "npt=now-".to_string())];
	if let Some(slot) = state.sessions.first_video_rtp(sid) {
		let mut found: Option<(u16, u32)> = None;
		for _ in 0..10 {
			if let Ok(guard) = slot.lock() {
				if let Some(pair) = *guard {
					found = Some(pair);
					break;
				}
			}
			tokio::time::sleep(std::time::Duration::from_millis(20)).await;
		}
		if let Some((seq, rtptime)) = found {
			headers.push((
				"RTP-Info",
				format!("url={};seq={};rtptime={}", parsed.uri, seq, rtptime),
			));
		} else {
			tracing::debug!(
				session = %sid,
				"no video packet sent yet at PLAY time; omitting RTP-Info"
			);
		}
	}
	let resp =
		crate::rtsp::message::build_response(rtsp_types::StatusCode::Ok, cseq, &headers, None);
	write_response(state, &resp).await
}

/// Subscribe to a camera stream, transparently falling back from `Extern`
/// to `Sub` when the provider reports the Extern stream is unavailable.
///
/// Reolink cameras advertise an `Extern` "balanced" stream that some
/// firmware revisions don't actually support. The spec (SPEC §2.3 stream
/// types) calls for the server to silently degrade to `Sub` rather than
/// surface a 503 to the RTSP client. Other [`crate::provider::StreamError`]
/// variants pass through unchanged.
///
/// `user` is the authenticated RTSP username (or `None` if the server is
/// running without auth). Forwarded to the provider so per-camera ACLs
/// (`permitted_users`) can be enforced.
async fn subscribe_with_extern_fallback(
	state: &Arc<ConnectionState>,
	camera: &str,
	kind: crate::url::StreamKind,
	user: Option<&str>,
) -> Result<crate::provider::SubscriptionHandle, crate::provider::StreamError> {
	match state.provider.subscribe(camera, kind, user).await {
		Err(crate::provider::StreamError::Unavailable(_))
			if kind == crate::url::StreamKind::Extern =>
		{
			tracing::debug!(camera, "Extern stream unavailable, falling back to Sub");
			state
				.provider
				.subscribe(camera, crate::url::StreamKind::Sub, user)
				.await
		}
		other => other,
	}
}

async fn handle_teardown(
	state: &Arc<ConnectionState>,
	cseq: u32,
	parsed: &crate::rtsp::message::ParsedRequest,
) -> io::Result<()> {
	let Some(sid) = &parsed.session else {
		let resp = crate::rtsp::message::build_response(
			rtsp_types::StatusCode::BadRequest,
			cseq,
			&[],
			None,
		);
		return write_response(state, &resp).await;
	};
	let removed = state.sessions.remove(sid);
	// Only echo the Session header when the session was actually torn
	// down. RFC 7826 §13.1 says the Session header in a response should
	// refer to an extant session — echoing an unknown session ID on a 454
	// response is mildly non-compliant (and can confuse strict clients).
	let (status, extra) = if removed.is_some() {
		(rtsp_types::StatusCode::Ok, vec![("Session", sid.clone())])
	} else {
		(rtsp_types::StatusCode::SessionNotFound, vec![])
	};
	let resp = crate::rtsp::message::build_response(status, cseq, &extra, None);
	write_response(state, &resp).await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn find_double_crlf_locates_header_end() {
		let s = b"OPTIONS / RTSP/1.0\r\nCSeq: 1\r\n\r\n";
		assert_eq!(find_double_crlf(s), Some(s.len() - 4));
	}

	#[test]
	fn try_consume_request_returns_none_on_partial() {
		let mut buf = b"OPTIONS / RTSP/1.0\r\nCSeq: 1\r\n".to_vec();
		assert_eq!(try_consume_request(&mut buf).unwrap(), None);
	}

	#[test]
	fn try_consume_request_slices_complete_request() {
		let req = b"OPTIONS / RTSP/1.0\r\nCSeq: 1\r\n\r\n";
		let mut buf = req.to_vec();
		buf.extend_from_slice(b"LEFTOVER");
		let consumed = try_consume_request(&mut buf).unwrap().unwrap();
		assert_eq!(&consumed, req);
		assert_eq!(buf, b"LEFTOVER");
	}

	#[test]
	fn parse_content_length_returns_value() {
		let headers = b"POST / RTSP/1.0\r\nContent-Length: 42\r\n\r\n";
		assert_eq!(parse_content_length(headers), Some(42));
	}

	#[test]
	fn parse_content_length_case_insensitive() {
		let headers = b"POST / RTSP/1.0\r\ncontent-length: 7\r\n\r\n";
		assert_eq!(parse_content_length(headers), Some(7));
	}

	// `Content-Length` arithmetic hardening. A hostile Content-Length
	// large enough to overflow `usize` panics in debug builds; large
	// enough to wrap-but-stay-positive in release misclassifies the
	// request boundary. Cap explicitly + use `checked_add`.

	#[test]
	fn try_consume_request_rejects_oversize_content_length() {
		// Content-Length larger than MAX_REQUEST_BYTES (64 KiB) — well
		// within usize range, but bigger than the buffer would ever
		// accommodate. Reject instead of waiting for bytes that can't fit.
		let cl = MAX_REQUEST_BYTES + 1;
		let headers = format!("POST / RTSP/1.0\r\nCSeq: 1\r\nContent-Length: {cl}\r\n\r\n");
		let mut buf = headers.into_bytes();
		let err = try_consume_request(&mut buf).expect_err("oversize must reject");
		assert!(
			err.contains("Content-Length") || err.contains("size"),
			"error: {err}"
		);
	}

	#[test]
	fn try_consume_request_rejects_overflow_content_length() {
		// usize::MAX is parseable but `end + body_len` overflows. Must
		// surface as a parse error, never panic in debug builds.
		let cl = usize::MAX;
		let headers = format!("POST / RTSP/1.0\r\nCSeq: 1\r\nContent-Length: {cl}\r\n\r\n");
		let mut buf = headers.into_bytes();
		let err = try_consume_request(&mut buf).expect_err("overflow must reject");
		assert!(
			err.contains("Content-Length") || err.contains("size") || err.contains("overflow"),
			"error: {err}"
		);
	}

	#[test]
	fn try_consume_request_accepts_zero_content_length() {
		// `Content-Length: 0` is the everyday case for OPTIONS/SETUP/PLAY;
		// must continue to slice cleanly.
		let req = b"OPTIONS / RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 0\r\n\r\n";
		let mut buf = req.to_vec();
		let consumed = try_consume_request(&mut buf).unwrap().unwrap();
		assert_eq!(&consumed, req);
	}

	#[test]
	fn try_consume_request_accepts_small_body() {
		// A small body within the cap must round-trip.
		let body = b"abc";
		let req = format!(
			"ANNOUNCE / RTSP/1.0\r\nCSeq: 1\r\nContent-Length: {}\r\n\r\n",
			body.len()
		);
		let mut buf = req.as_bytes().to_vec();
		buf.extend_from_slice(body);
		let consumed = try_consume_request(&mut buf).unwrap().unwrap();
		assert_eq!(consumed.len(), req.len() + body.len());
	}

	// ====== Handler coverage tests ======

	use crate::buffer::LastFrameBuffer;
	use crate::provider::{Frame, StreamError, SubscriptionHandle};
	use crate::rtsp::auth::Nonce;
	use crate::sdp::{SdpParams, VideoParams};
	use std::net::Ipv4Addr;

	#[derive(Default)]
	struct MockProv {
		/// When true, subscribe returns Unavailable on Extern.
		unavailable_extern: std::sync::atomic::AtomicBool,
		/// When true, subscribe returns AccessDenied.
		deny_access: std::sync::atomic::AtomicBool,
		/// When true, subscribe returns Internal error.
		internal_error: std::sync::atomic::AtomicBool,
		/// Names of known cameras.
		known: std::sync::Mutex<Vec<String>>,
	}

	impl MockProv {
		fn with_cameras(names: &[&str]) -> Arc<Self> {
			Arc::new(Self {
				known: std::sync::Mutex::new(names.iter().map(|s| s.to_string()).collect()),
				..Default::default()
			})
		}
	}

	#[async_trait::async_trait]
	impl crate::provider::StreamProvider for MockProv {
		async fn subscribe(
			&self,
			camera: &str,
			kind: crate::url::StreamKind,
			_user: Option<&str>,
		) -> Result<SubscriptionHandle, StreamError> {
			if self
				.internal_error
				.load(std::sync::atomic::Ordering::SeqCst)
			{
				return Err(StreamError::Internal("boom".into()));
			}
			if self.deny_access.load(std::sync::atomic::Ordering::SeqCst) {
				return Err(StreamError::AccessDenied);
			}
			let known = self.known.lock().unwrap();
			if !known.iter().any(|n| n == camera) {
				return Err(StreamError::UnknownCamera);
			}
			if kind == crate::url::StreamKind::Extern
				&& self
					.unavailable_extern
					.load(std::sync::atomic::Ordering::SeqCst)
			{
				return Err(StreamError::Unavailable("extern-na".into()));
			}
			let (_tx, rx) = tokio::sync::broadcast::channel::<Frame>(16);
			Ok(SubscriptionHandle {
				frames: rx,
				sdp_params: SdpParams {
					server_ip: "0.0.0.0".to_string(),
					session_id: "0".to_string(),
					session_name: camera.to_string(),
					video: Some(VideoParams {
						codec: crate::codec::VideoCodec::H264,
						payload_type: 96,
						sps: vec![0x67, 0x42, 0x00, 0x1f],
						pps: vec![0x68, 0xce],
						vps: None,
						profile_level_id: [0x42, 0x00, 0x1f],
					}),
					audio: None,
				},
				last_frame: Arc::new(LastFrameBuffer::new()),
				guard: Box::new(()),
			})
		}
	}

	fn make_state(
		provider: Arc<dyn crate::provider::StreamProvider>,
	) -> (Arc<ConnectionState>, tokio::io::DuplexStream) {
		make_state_with_users(provider, vec![])
	}

	fn make_state_with_users(
		provider: Arc<dyn crate::provider::StreamProvider>,
		users: Vec<UserCred>,
	) -> (Arc<ConnectionState>, tokio::io::DuplexStream) {
		let (client, server) = tokio::io::duplex(64 * 1024);
		let writer: Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
			Arc::new(Mutex::new(Box::new(server)));
		let state = Arc::new(ConnectionState {
			provider,
			users,
			realm: "test-realm".into(),
			current_nonce: Mutex::new(Nonce::random()),
			sessions: Arc::new(SessionRegistry::new()),
			udp_pool: Arc::new(UdpPortPool::new()),
			server_bind_ip: std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
			peer_ip: std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
			local_ip: std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
			is_tls: false,
			writer,
		});
		(state, client)
	}

	/// Read bytes from the duplex until `\r\n\r\n` is seen, then return
	/// headers as string and body bytes. Includes any response body.
	async fn read_response(client: &mut tokio::io::DuplexStream) -> (String, Vec<u8>) {
		use tokio::io::AsyncReadExt;
		let mut buf = Vec::new();
		let mut tmp = [0u8; 4096];
		loop {
			let n = client.read(&mut tmp).await.unwrap();
			if n == 0 {
				break;
			}
			buf.extend_from_slice(&tmp[..n]);
			if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
				let head = std::str::from_utf8(&buf[..pos + 4]).unwrap().to_string();
				// Parse Content-Length.
				let mut cl = 0usize;
				for line in head.split("\r\n") {
					if let Some((k, v)) = line.split_once(':') {
						if k.eq_ignore_ascii_case("Content-Length") {
							cl = v.trim().parse().unwrap_or(0);
						}
					}
				}
				let body_start = pos + 4;
				while buf.len() < body_start + cl {
					let n = client.read(&mut tmp).await.unwrap();
					if n == 0 {
						break;
					}
					buf.extend_from_slice(&tmp[..n]);
				}
				return (head, buf[body_start..body_start + cl].to_vec());
			}
		}
		panic!("no response");
	}

	fn status_of(head: &str) -> u16 {
		head.split_whitespace().nth(1).unwrap().parse().unwrap()
	}

	#[tokio::test(start_paused = true)]
	async fn handle_connection_closes_on_initial_request_timeout() {
		// Slow-loris defence: a client that opens TCP and never sends a
		// complete RTSP request must not pin the accept slot
		// indefinitely. Under paused virtual time, advance past
		// INITIAL_REQUEST_TIMEOUT and verify the handler returns.
		use std::net::Ipv4Addr;
		let provider = MockProv::with_cameras(&["cam1"]);
		let (client, server) = tokio::io::duplex(64 * 1024);
		let cancel = CancellationToken::new();
		let task = tokio::spawn(async move {
			handle_connection(
				server,
				provider,
				vec![],
				"test".into(),
				Arc::new(crate::server::udp_pool::UdpPortPool::new()),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				false,
				cancel.clone(),
			)
			.await;
		});

		// Hold the client side open but send nothing.
		tokio::task::yield_now().await;
		tokio::time::advance(INITIAL_REQUEST_TIMEOUT + std::time::Duration::from_secs(1)).await;

		let res = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
		assert!(
			res.is_ok(),
			"handle_connection must close on initial-request timeout"
		);
		drop(client);
	}

	#[tokio::test(start_paused = true)]
	async fn handle_connection_reaps_post_request_idleness_when_no_session() {
		// New rolling-deadline contract: a client that sends OPTIONS
		// (no session created) and then idles is reaped one window
		// after the request, not "never". Replaces the old
		// "disarms-on-first-dispatch" test which codified an
		// over-eager design that left fork-bomb-after-OPTIONS clients
		// holding accept slots indefinitely.
		use std::net::Ipv4Addr;
		let provider = MockProv::with_cameras(&["cam1"]);
		let (mut client, server) = tokio::io::duplex(64 * 1024);
		let cancel = CancellationToken::new();
		let cancel_for_task = cancel.clone();
		let task = tokio::spawn(async move {
			handle_connection(
				server,
				provider,
				vec![],
				"test".into(),
				Arc::new(crate::server::udp_pool::UdpPortPool::new()),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				false,
				cancel_for_task,
			)
			.await;
		});

		// Send a single OPTIONS to establish "request-active" but
		// without creating any session.
		use tokio::io::AsyncWriteExt;
		client
			.write_all(b"OPTIONS rtsp://x/cam1 RTSP/1.0\r\nCSeq: 1\r\n\r\n")
			.await
			.unwrap();
		let mut sink = vec![0u8; 4096];
		use tokio::io::AsyncReadExt;
		let _ = tokio::time::timeout(
			std::time::Duration::from_millis(100),
			client.read(&mut sink),
		)
		.await;

		// Advance past the rolling deadline. With no session active,
		// the handler must close.
		tokio::time::advance(INITIAL_REQUEST_TIMEOUT + std::time::Duration::from_secs(1)).await;
		let res = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
		assert!(
			res.is_ok(),
			"handler must reap idle pre-session connection past the rolling deadline"
		);
		// The cancel token is unused once the handler self-exits via
		// the deadline arm; keeping it around just so it can be
		// silently dropped at end-of-scope.
		let _ = cancel;
	}

	#[tokio::test(start_paused = true)]
	async fn handle_connection_with_active_session_survives_idleness() {
		// Once SETUP creates a session, the slow-loris arm must go
		// silent — the keepalive sweep handles idle reaping with its
		// own (longer, session-scoped) policy. Without this, an
		// established RTP-over-TCP-interleaved client whose RTSP
		// control channel goes silent during streaming would be
		// killed mid-stream.
		use std::net::Ipv4Addr;
		use tokio::io::{AsyncReadExt, AsyncWriteExt};
		let provider = MockProv::with_cameras(&["cam1"]);
		let (mut client, server) = tokio::io::duplex(64 * 1024);
		let cancel = CancellationToken::new();
		let cancel_for_task = cancel.clone();
		let task = tokio::spawn(async move {
			handle_connection(
				server,
				provider,
				vec![],
				"test".into(),
				Arc::new(crate::server::udp_pool::UdpPortPool::new()),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
				false,
				cancel_for_task,
			)
			.await;
		});

		// SETUP creates a session — pre-session arm goes inert.
		client
			.write_all(b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n")
			.await
			.unwrap();
		let mut sink = vec![0u8; 4096];
		let _ = tokio::time::timeout(
			std::time::Duration::from_millis(100),
			client.read(&mut sink),
		)
		.await;

		// Advance past the rolling deadline. With a session active,
		// the handler must stay alive.
		tokio::time::advance(INITIAL_REQUEST_TIMEOUT + std::time::Duration::from_secs(5)).await;
		assert!(
			!task.is_finished(),
			"handler must survive idleness once a session exists"
		);

		cancel.cancel();
		let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
		drop(client);
	}

	#[tokio::test]
	async fn dispatch_options_returns_200_with_public_header() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"OPTIONS rtsp://x/cam1 RTSP/1.0\r\nCSeq: 1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
		assert!(head.contains("Public:"));
		assert!(head.contains("OPTIONS"));
	}

	#[tokio::test]
	async fn dispatch_malformed_returns_400_and_err() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"not a valid rtsp request\r\n\r\n";
		let res = dispatch_request(&state, req).await;
		assert!(res.is_err());
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 400);
	}

	#[tokio::test]
	async fn getparameter_echoes_session_header() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"GET_PARAMETER rtsp://x/cam1 RTSP/1.0\r\nCSeq: 2\r\nSession: abc123\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
		assert!(head.contains("Session:"));
	}

	#[tokio::test]
	async fn pause_returns_200_ok() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"PAUSE rtsp://x/cam1 RTSP/1.0\r\nCSeq: 3\r\nSession: sid\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
	}

	#[tokio::test]
	async fn describe_unknown_camera_returns_404() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"DESCRIBE rtsp://x/nocam/main RTSP/1.0\r\nCSeq: 4\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 404);
	}

	#[tokio::test]
	async fn describe_bad_path_returns_404() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		// Empty / single-segment path fails url::resolve.
		let req = b"DESCRIBE rtsp://x/ RTSP/1.0\r\nCSeq: 4\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 404);
	}

	#[tokio::test]
	async fn describe_provider_internal_error_returns_503() {
		let provider = MockProv::with_cameras(&["cam1"]);
		provider
			.internal_error
			.store(true, std::sync::atomic::Ordering::SeqCst);
		let (state, mut client) = make_state(provider);
		let req = b"DESCRIBE rtsp://x/cam1/main RTSP/1.0\r\nCSeq: 4\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 503);
	}

	#[tokio::test]
	async fn describe_happy_path_returns_200_with_sdp() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"DESCRIBE rtsp://x/cam1/main RTSP/1.0\r\nCSeq: 4\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, body) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
		assert!(head.contains("application/sdp"));
		assert!(head.contains("Content-Base:"));
		// SDP should include our advertised IP.
		assert!(!body.is_empty());
	}

	#[tokio::test]
	async fn describe_extern_fallback_to_sub_on_unavailable() {
		let provider = MockProv::with_cameras(&["cam1"]);
		provider
			.unavailable_extern
			.store(true, std::sync::atomic::Ordering::SeqCst);
		let (state, mut client) = make_state(provider);
		let req = b"DESCRIBE rtsp://x/cam1/extern RTSP/1.0\r\nCSeq: 4\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		// Sub succeeds → 200.
		assert_eq!(status_of(&head), 200);
	}

	#[tokio::test]
	async fn describe_without_auth_when_users_set_returns_401() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let users = vec![UserCred {
			name: "alice".into(),
			password: "pw".into(),
		}];
		let (state, mut client) = make_state_with_users(provider, users);
		let req = b"DESCRIBE rtsp://x/cam1/main RTSP/1.0\r\nCSeq: 4\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 401);
		assert!(head.contains("WWW-Authenticate"));
	}

	#[tokio::test]
	async fn describe_bad_basic_creds_returns_403() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let users = vec![UserCred {
			name: "alice".into(),
			password: "pw".into(),
		}];
		let (state, mut client) = make_state_with_users(provider, users);
		use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
		let bad = B64.encode("alice:wrong");
		let req = format!(
			"DESCRIBE rtsp://x/cam1/main RTSP/1.0\r\nCSeq: 4\r\nAuthorization: Basic {bad}\r\n\r\n"
		);
		dispatch_request(&state, req.as_bytes()).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 403);
	}

	#[tokio::test]
	async fn describe_good_basic_creds_returns_200() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let users = vec![UserCred {
			name: "alice".into(),
			password: "pw".into(),
		}];
		let (state, mut client) = make_state_with_users(provider, users);
		use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
		let tok = B64.encode("alice:pw");
		let req = format!(
			"DESCRIBE rtsp://x/cam1/main RTSP/1.0\r\nCSeq: 4\r\nAuthorization: Basic {tok}\r\n\r\n"
		);
		dispatch_request(&state, req.as_bytes()).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
	}

	#[tokio::test]
	async fn setup_unknown_path_returns_404() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"SETUP rtsp://x/ RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 404);
	}

	#[tokio::test]
	async fn setup_missing_transport_returns_400() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 400);
	}

	#[tokio::test]
	async fn setup_bad_transport_returns_461() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: bogus/transport\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 461);
	}

	#[tokio::test]
	async fn setup_unknown_camera_returns_404() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"SETUP rtsp://x/nocam/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 404);
	}

	#[tokio::test]
	async fn setup_provider_access_denied_returns_403() {
		let provider = MockProv::with_cameras(&["cam1"]);
		provider
			.deny_access
			.store(true, std::sync::atomic::Ordering::SeqCst);
		let (state, mut client) = make_state(provider);
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 403);
	}

	#[tokio::test]
	async fn setup_provider_internal_error_returns_503() {
		let provider = MockProv::with_cameras(&["cam1"]);
		provider
			.internal_error
			.store(true, std::sync::atomic::Ordering::SeqCst);
		let (state, mut client) = make_state(provider);
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 503);
	}

	#[tokio::test]
	async fn setup_auth_required_without_creds_returns_401() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let users = vec![UserCred {
			name: "alice".into(),
			password: "pw".into(),
		}];
		let (state, mut client) = make_state_with_users(provider, users);
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 401);
	}

	#[tokio::test]
	async fn setup_forbidden_auth_returns_403() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let users = vec![UserCred {
			name: "alice".into(),
			password: "pw".into(),
		}];
		let (state, mut client) = make_state_with_users(provider, users);
		use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
		let bad = B64.encode("alice:bad");
		let req = format!(
			"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\nAuthorization: Basic {bad}\r\n\r\n"
		);
		dispatch_request(&state, req.as_bytes()).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 403);
	}

	#[tokio::test]
	async fn play_without_session_header_returns_400() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"PLAY rtsp://x/cam1 RTSP/1.0\r\nCSeq: 6\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 400);
	}

	#[tokio::test]
	async fn play_unknown_session_returns_454() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"PLAY rtsp://x/cam1 RTSP/1.0\r\nCSeq: 6\r\nSession: nosuch\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 454);
	}

	#[tokio::test]
	async fn teardown_without_session_returns_400() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"TEARDOWN rtsp://x/cam1 RTSP/1.0\r\nCSeq: 7\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 400);
	}

	#[tokio::test]
	async fn teardown_unknown_session_returns_454() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"TEARDOWN rtsp://x/cam1 RTSP/1.0\r\nCSeq: 7\r\nSession: nosuch\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 454);
	}

	#[test]
	fn extract_path_strips_scheme_host() {
		assert_eq!(extract_path("rtsp://host:8554/cam1/main"), "/cam1/main");
	}

	#[test]
	fn extract_path_returns_slash_for_bare_authority() {
		assert_eq!(extract_path("rtsp://host:8554"), "/");
	}

	#[test]
	fn extract_path_relative_passes_through() {
		assert_eq!(extract_path("/cam1/main"), "/cam1/main");
	}

	#[test]
	fn scheme_matches_transport_table() {
		use super::scheme_matches_transport;
		assert!(scheme_matches_transport("rtsp://h/c", false));
		assert!(scheme_matches_transport("rtsps://h/c", true));
		assert!(!scheme_matches_transport("rtsp://h/c", true));
		assert!(!scheme_matches_transport("rtsps://h/c", false));
		// Relative URIs / unknown schemes accepted (downstream 4xx).
		assert!(scheme_matches_transport("trackID=0", false));
		assert!(scheme_matches_transport("trackID=0", true));
		assert!(scheme_matches_transport("file:///etc/passwd", false));
	}

	#[tokio::test]
	async fn rtsps_uri_on_plain_connection_returns_400() {
		use tokio::io::AsyncWriteExt;
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		// is_tls defaults to false in make_state — that is the test scenario.
		let req = b"DESCRIBE rtsps://x/cam1/main RTSP/1.0\r\nCSeq: 1\r\n\r\n";
		client.write_all(req).await.unwrap();
		// Drain client write side; dispatch_request handles the bytes.
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 400, "got: {head}");
	}

	#[tokio::test]
	async fn rtsp_uri_on_plain_connection_dispatches_normally() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"OPTIONS rtsp://x/cam1 RTSP/1.0\r\nCSeq: 7\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
	}

	#[test]
	fn advertised_server_ip_prefers_explicit_bind() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (client, _server) = tokio::io::duplex(4096);
		let writer: Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
			Arc::new(Mutex::new(Box::new(client)));
		let explicit = std::net::IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
		let state = Arc::new(ConnectionState {
			provider,
			users: vec![],
			realm: "t".into(),
			current_nonce: Mutex::new(Nonce::random()),
			sessions: Arc::new(SessionRegistry::new()),
			udp_pool: Arc::new(UdpPortPool::new()),
			server_bind_ip: explicit,
			peer_ip: std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
			local_ip: std::net::IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
			is_tls: false,
			writer,
		});
		assert_eq!(advertised_server_ip(&state), explicit);
	}

	#[test]
	fn advertised_server_ip_uses_local_when_bind_unspecified() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (client, _server) = tokio::io::duplex(4096);
		let writer: Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
			Arc::new(Mutex::new(Box::new(client)));
		let local = std::net::IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
		let state = Arc::new(ConnectionState {
			provider,
			users: vec![],
			realm: "t".into(),
			current_nonce: Mutex::new(Nonce::random()),
			sessions: Arc::new(SessionRegistry::new()),
			udp_pool: Arc::new(UdpPortPool::new()),
			server_bind_ip: std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED),
			peer_ip: std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
			local_ip: local,
			is_tls: false,
			writer,
		});
		assert_eq!(advertised_server_ip(&state), local);
	}

	#[test]
	fn generate_session_id_returns_numeric_string() {
		let id = generate_session_id_for_sdp();
		assert!(id.chars().all(|c| c.is_ascii_digit()));
	}

	#[tokio::test]
	async fn setup_tcp_interleaved_then_teardown_happy_path() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		// First SETUP creates session + spawns send task.
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
		assert!(head.contains("Session:"));
		// Extract session ID.
		let sid = head
			.split("\r\n")
			.find_map(|l| l.strip_prefix("Session: "))
			.unwrap()
			.split(';')
			.next()
			.unwrap()
			.to_string();

		// TEARDOWN the session.
		let req = format!("TEARDOWN rtsp://x/cam1 RTSP/1.0\r\nCSeq: 7\r\nSession: {sid}\r\n\r\n");
		dispatch_request(&state, req.as_bytes()).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
	}

	#[tokio::test]
	async fn setup_append_track_on_existing_session() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		// First SETUP (video).
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		let sid = head
			.split("\r\n")
			.find_map(|l| l.strip_prefix("Session: "))
			.unwrap()
			.split(';')
			.next()
			.unwrap()
			.to_string();

		// Second SETUP (audio) - append path.
		let req = format!(
			"SETUP rtsp://x/cam1/main/trackID=1 RTSP/1.0\r\nCSeq: 6\r\nSession: {sid}\r\nTransport: RTP/AVP/TCP;unicast;interleaved=2-3\r\n\r\n"
		);
		dispatch_request(&state, req.as_bytes()).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
	}

	#[tokio::test]
	async fn play_on_existing_session_returns_200() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		let sid = head
			.split("\r\n")
			.find_map(|l| l.strip_prefix("Session: "))
			.unwrap()
			.split(';')
			.next()
			.unwrap()
			.to_string();

		let req = format!("PLAY rtsp://x/cam1 RTSP/1.0\r\nCSeq: 6\r\nSession: {sid}\r\n\r\n");
		dispatch_request(&state, req.as_bytes()).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
	}

	#[tokio::test]
	async fn setup_udp_transport_happy_path() {
		let provider = MockProv::with_cameras(&["cam1"]);
		let (state, mut client) = make_state(provider);
		let req = b"SETUP rtsp://x/cam1/main/trackID=0 RTSP/1.0\r\nCSeq: 5\r\nTransport: RTP/AVP;unicast;client_port=50000-50001\r\n\r\n";
		dispatch_request(&state, req).await.unwrap();
		let (head, _) = read_response(&mut client).await;
		assert_eq!(status_of(&head), 200);
		assert!(head.contains("server_port="));
	}
}
