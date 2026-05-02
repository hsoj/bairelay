//! Integration-test harness for RTSP server.
//!
//! Tasks 28-32 add actual tests on top of this scaffolding.
//! Task 27 provides:
//! - `MockProvider` (implements `StreamProvider`)
//! - `spawn_server_with` helper
//! - Synthetic NAL + SdpParams generators

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use bairelay_rtsp::buffer::LastFrameBuffer;
use bairelay_rtsp::codec::VideoCodec;
use bairelay_rtsp::provider::{Frame, StreamError, StreamProvider, SubscriptionHandle};
use bairelay_rtsp::rtsp::auth::UserCred;
use bairelay_rtsp::sdp::{SdpParams, VideoParams};
use bairelay_rtsp::server::{RtspServer, ServerConfig};
use bairelay_rtsp::url::StreamKind;

struct Guard {
	counter: Arc<AtomicU32>,
}

impl Drop for Guard {
	fn drop(&mut self) {
		self.counter.fetch_add(1, Ordering::SeqCst);
	}
}

/// Test provider that emits whatever frames the test pushes into `frames_tx`.
pub struct MockProvider {
	pub frames_tx: broadcast::Sender<Frame>,
	pub guard_drops: Arc<AtomicU32>,
	pub last_frame: Arc<LastFrameBuffer>,
	pub sdp_params: SdpParams,
	/// Camera names recognised by the mock.
	known_cameras: HashSet<String>,
	/// (camera, kind) combos that should fail subscribe with Unavailable —
	/// for Task 32 extern fallback.
	unavailable: HashSet<(String, StreamKind)>,
}

impl MockProvider {
	/// Build a provider that knows about the given camera names. No
	/// (camera, kind) combos are marked unavailable.
	pub fn new(cameras: &[&str]) -> Arc<Self> {
		Self::build(cameras, &[])
	}

	/// Build a provider that knows about `cameras` and rejects any
	/// `(camera, kind)` listed in `unavailable` with [`StreamError::Unavailable`].
	pub fn with_unavailable(cameras: &[&str], unavailable: &[(&str, StreamKind)]) -> Arc<Self> {
		Self::build(cameras, unavailable)
	}

	fn build(cameras: &[&str], unavailable: &[(&str, StreamKind)]) -> Arc<Self> {
		let (tx, _rx) = broadcast::channel(64);
		Arc::new(Self {
			frames_tx: tx,
			guard_drops: Arc::new(AtomicU32::new(0)),
			last_frame: Arc::new(LastFrameBuffer::new()),
			sdp_params: sample_sdp_params(),
			known_cameras: cameras.iter().map(|s| (*s).to_string()).collect(),
			unavailable: unavailable
				.iter()
				.map(|(c, k)| ((*c).to_string(), *k))
				.collect(),
		})
	}
}

#[async_trait::async_trait]
impl StreamProvider for MockProvider {
	async fn subscribe(
		&self,
		camera: &str,
		kind: StreamKind,
		_authenticated_user: Option<&str>,
	) -> Result<SubscriptionHandle, StreamError> {
		if !self.known_cameras.contains(camera) {
			return Err(StreamError::UnknownCamera);
		}
		if self.unavailable.contains(&(camera.to_string(), kind)) {
			return Err(StreamError::Unavailable("not supported".to_string()));
		}
		let guard = Box::new(Guard {
			counter: Arc::clone(&self.guard_drops),
		});
		Ok(SubscriptionHandle {
			frames: self.frames_tx.subscribe(),
			sdp_params: self.sdp_params.clone(),
			last_frame: Arc::clone(&self.last_frame),
			guard,
		})
	}
}

/// Spawn the server on a free loopback port; return `(addr, cancel)`.
///
/// The returned cancel token stops the server. Tests should call
/// `cancel.cancel()` at end.
///
/// The listener is bound once with `127.0.0.1:0` to claim a free port,
/// then dropped and rebound inside `RtspServer::serve`. The `0`-to-real
/// port handshake is race-prone on loaded CI (another process can grab
/// the port between our drop and the server's rebind, and the server
/// task doesn't signal readiness back up). We mitigate by polling a TCP
/// connect against the picked address with a short timeout and retrying
/// on a fresh port if the server isn't reachable, up to 5 attempts.
pub async fn spawn_server_with(
	provider: Arc<dyn StreamProvider>,
	users: Vec<UserCred>,
) -> (SocketAddr, CancellationToken) {
	const MAX_ATTEMPTS: u32 = 5;
	const READY_POLLS: u32 = 10;
	const READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

	for attempt in 0..MAX_ATTEMPTS {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		drop(listener);

		let config = ServerConfig {
			bind: addr,
			realm: "bairelay-test".to_string(),
			users: users.clone(),
			tls: None,
			max_connections: None,
		};
		let cancel = CancellationToken::new();
		let cancel_for_server = cancel.clone();
		let provider_for_server = Arc::clone(&provider);
		let server_task = tokio::spawn(async move {
			let _ = RtspServer::serve(config, provider_for_server, cancel_for_server).await;
		});

		// Poll until we can actually TCP-connect; that proves the server
		// bound successfully.
		let mut ready = false;
		for _ in 0..READY_POLLS {
			tokio::time::sleep(READY_POLL_INTERVAL).await;
			if TcpStream::connect(addr).await.is_ok() {
				ready = true;
				break;
			}
		}
		if ready {
			return (addr, cancel);
		}

		// Port wasn't reachable — someone else likely grabbed it. Cancel
		// the spawned server, join it so it doesn't leak, then try a new
		// port on the next iteration.
		cancel.cancel();
		let _ = server_task.await;
		eprintln!("spawn_server_with: attempt {attempt} could not reach {addr}, retrying");
	}
	panic!("spawn_server_with failed after {MAX_ATTEMPTS} attempts");
}

/// Synthetic H.264 burst: `(SPS, PPS, IDR)` — structurally valid NAL bytes,
/// not real-decodable.
pub fn h264_iframe_burst() -> (Bytes, Bytes, Bytes) {
	let sps = Bytes::from_static(&[0x67, 0x42, 0x00, 0x1F, 0xAC, 0x34, 0xCA, 0x00, 0x00]);
	let pps = Bytes::from_static(&[0x68, 0xCE, 0x3C, 0x80]);
	let idr = Bytes::from_static(&[
		0x65, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11, 0x22, 0x33,
	]);
	(sps, pps, idr)
}

/// SDP parameters suitable for the mock — video H.264, no audio.
pub fn sample_sdp_params() -> SdpParams {
	let (sps, pps, _idr) = h264_iframe_burst();
	SdpParams {
		server_ip: "127.0.0.1".to_string(),
		session_id: "12345".to_string(),
		session_name: "bairelay-test".to_string(),
		video: Some(VideoParams {
			codec: VideoCodec::H264,
			payload_type: 96,
			sps: sps.to_vec(),
			pps: pps.to_vec(),
			vps: None,
			profile_level_id: [0x42, 0x00, 0x1F],
		}),
		audio: None,
	}
}

/// A `Frame::Video` containing `(SPS, PPS, IDR)` as one access unit — for tests.
pub fn keyframe_access_unit(pts_90khz: u32) -> Frame {
	let (sps, pps, idr) = h264_iframe_burst();
	Frame::Video {
		codec: VideoCodec::H264,
		nals: vec![sps, pps, idr],
		pts_90khz,
		keyframe: true,
		access_unit_end: true,
	}
}

// ----------------------------------------------------------------------------
// Raw-bytes RTSP client helpers used by the integration tests.
// ----------------------------------------------------------------------------

/// Write an entire RTSP request to `stream`.
async fn write_request(stream: &mut TcpStream, req: &[u8]) {
	stream.write_all(req).await.unwrap();
	stream.flush().await.unwrap();
}

/// Parsed RTSP response from [`read_rtsp_response`].
#[derive(Debug)]
struct RtspResponse {
	status: u16,
	headers: std::collections::HashMap<String, String>,
	#[allow(dead_code)]
	body: Vec<u8>,
}

/// Read a single RTSP response from `stream`, transparently skipping any
/// TCP-interleaved RTP/RTCP frames (`$ ch len payload`) that appear before
/// the response status line.
async fn read_rtsp_response(stream: &mut TcpStream) -> RtspResponse {
	let mut buf: Vec<u8> = Vec::new();
	let mut tmp = [0u8; 4096];

	loop {
		// Strip any leading interleaved frames first.
		while !buf.is_empty() && buf[0] == 0x24 {
			if buf.len() < 4 {
				let n = stream.read(&mut tmp).await.unwrap();
				if n == 0 {
					panic!("EOF before complete interleaved header");
				}
				buf.extend_from_slice(&tmp[..n]);
				continue;
			}
			let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
			let need = 4 + len;
			while buf.len() < need {
				let n = stream.read(&mut tmp).await.unwrap();
				if n == 0 {
					panic!("EOF before complete interleaved frame");
				}
				buf.extend_from_slice(&tmp[..n]);
			}
			buf.drain(..need);
		}

		if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
			let header_end = pos + 4;
			let head = std::str::from_utf8(&buf[..pos]).unwrap();
			let mut lines = head.split("\r\n");
			let status_line = lines.next().unwrap();
			let status: u16 = status_line
				.split_whitespace()
				.nth(1)
				.unwrap()
				.parse()
				.unwrap();
			let mut headers: std::collections::HashMap<String, String> =
				std::collections::HashMap::new();
			let mut content_length = 0usize;
			for line in lines {
				if line.is_empty() {
					continue;
				}
				if let Some((k, v)) = line.split_once(':') {
					let key = k.trim().to_ascii_lowercase();
					let value = v.trim().to_string();
					if key == "content-length" {
						content_length = value.parse().unwrap_or(0);
					}
					// Multiple WWW-Authenticate headers are allowed; keep the
					// first (Digest) since that's what these tests use. A more
					// general client would fold values into a list.
					headers.entry(key).or_insert(value);
				}
			}
			let mut body = buf[header_end..].to_vec();
			while body.len() < content_length {
				let n = stream.read(&mut tmp).await.unwrap();
				if n == 0 {
					break;
				}
				body.extend_from_slice(&tmp[..n]);
			}
			body.truncate(content_length);
			return RtspResponse {
				status,
				headers,
				body,
			};
		}

		let n = tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut tmp))
			.await
			.expect("timeout waiting for RTSP response")
			.unwrap();
		if n == 0 {
			panic!("connection closed before RTSP response");
		}
		buf.extend_from_slice(&tmp[..n]);
	}
}

/// Extract the bare session ID from a `Session: SID;timeout=30` header.
fn session_id_from(headers: &std::collections::HashMap<String, String>) -> String {
	let raw = headers.get("session").expect("missing Session header");
	raw.split(';').next().unwrap().trim().to_string()
}

// This task is harness-only; Tasks 28–32 add real `#[tokio::test]` functions.
// Add a trivial test here so `cargo test` still runs this test binary.
#[tokio::test]
async fn harness_compiles() {
	let provider = MockProvider::new(&["cam1"]);
	let (_addr, cancel) = spawn_server_with(provider, vec![]).await;
	cancel.cancel();
}

// ----------------------------------------------------------------------------
// Task 28 — full OPTIONS/DESCRIBE/SETUP/PLAY/TEARDOWN round-trip over TCP.
// ----------------------------------------------------------------------------

#[tokio::test]
async fn round_trip_tcp_interleaved() {
	let provider = MockProvider::new(&["cam1"]);
	let guard_drops = Arc::clone(&provider.guard_drops);
	let (addr, cancel) = spawn_server_with(provider, vec![]).await;

	let mut stream = TcpStream::connect(addr).await.unwrap();

	// 1) OPTIONS
	let req = format!("OPTIONS rtsp://{addr}/cam1 RTSP/1.0\r\nCSeq: 1\r\n\r\n",);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "OPTIONS should return 200");
	let public = resp
		.headers
		.get("public")
		.expect("OPTIONS response missing Public header");
	for method in ["OPTIONS", "DESCRIBE", "SETUP", "PLAY", "TEARDOWN"] {
		assert!(
			public.contains(method),
			"Public header missing {method}: {public}"
		);
	}

	// 2) DESCRIBE
	let req = format!(
		"DESCRIBE rtsp://{addr}/cam1 RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n",
	);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "DESCRIBE should return 200");
	let body = std::str::from_utf8(&resp.body).expect("SDP body should be UTF-8");
	assert!(
		body.starts_with("v=0"),
		"SDP body should start with v=0, got: {body}"
	);
	assert!(body.contains("m=video"), "SDP body should contain m=video");

	// 3) SETUP
	let req = format!(
		"SETUP rtsp://{addr}/cam1/trackID=0 RTSP/1.0\r\n\
		 CSeq: 3\r\n\
		 Transport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n",
	);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "SETUP should return 200");
	let session = session_id_from(&resp.headers);
	assert!(!session.is_empty(), "Session ID should be non-empty");

	// 4) PLAY
	let req = format!("PLAY rtsp://{addr}/cam1 RTSP/1.0\r\nCSeq: 4\r\nSession: {session}\r\n\r\n",);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "PLAY should return 200");

	// 5) TEARDOWN
	let req =
		format!("TEARDOWN rtsp://{addr}/cam1 RTSP/1.0\r\nCSeq: 5\r\nSession: {session}\r\n\r\n",);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "TEARDOWN should return 200");

	drop(stream);
	cancel.cancel();
	// Allow the connection task to wind down so its `clear()` runs.
	tokio::time::sleep(std::time::Duration::from_millis(100)).await;

	assert!(
		guard_drops.load(Ordering::SeqCst) >= 1,
		"expected at least one guard drop after teardown",
	);
}

// ----------------------------------------------------------------------------
// Task 29 — UDP transport end-to-end: SETUP/PLAY over RTP/AVP, observe a
// packet arrive on the client RTP socket, then TEARDOWN.
// ----------------------------------------------------------------------------

#[tokio::test]
async fn udp_transport_end_to_end() {
	let provider = MockProvider::new(&["cam1"]);
	let frames_tx = provider.frames_tx.clone();
	let guard_drops = Arc::clone(&provider.guard_drops);
	let (addr, cancel) = spawn_server_with(provider, vec![]).await;

	// Bind two client UDP sockets — RTP / RTCP — on loopback.
	let client_rtp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
	let client_rtcp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
	let client_rtp_port = client_rtp.local_addr().unwrap().port();
	let client_rtcp_port = client_rtcp.local_addr().unwrap().port();

	let mut stream = TcpStream::connect(addr).await.unwrap();

	// SETUP — UDP unicast, with the actual client ports we bound.
	let req = format!(
		"SETUP rtsp://{addr}/cam1/trackID=0 RTSP/1.0\r\n\
		 CSeq: 1\r\n\
		 Transport: RTP/AVP;unicast;client_port={client_rtp_port}-{client_rtcp_port}\r\n\r\n",
	);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(
		resp.status, 200,
		"SETUP should return 200, got {}",
		resp.status
	);

	let transport = resp
		.headers
		.get("transport")
		.expect("SETUP response missing Transport header");
	assert!(
		transport.contains(&format!("client_port={client_rtp_port}-{client_rtcp_port}")),
		"Transport header missing client_port: {transport}"
	);
	assert!(
		transport.contains("server_port="),
		"Transport header missing server_port: {transport}"
	);

	let session = session_id_from(&resp.headers);

	// PLAY.
	let req = format!("PLAY rtsp://{addr}/cam1 RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n",);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "PLAY should return 200");

	// Push a keyframe; expect at least one RTP packet on the client RTP port.
	// Allow a beat for the session task to actually attach to the broadcast.
	tokio::time::sleep(std::time::Duration::from_millis(50)).await;
	let _ = frames_tx.send(keyframe_access_unit(9000));

	let mut buf = [0u8; 2048];
	let recv = tokio::time::timeout(
		std::time::Duration::from_secs(2),
		client_rtp.recv_from(&mut buf),
	)
	.await;
	let (n, _from) = recv
		.expect("timed out waiting for RTP packet")
		.expect("recv_from failed");
	assert!(
		n > 12,
		"RTP packet too small: {n} bytes (must include 12-byte header)"
	);

	// TEARDOWN.
	let req =
		format!("TEARDOWN rtsp://{addr}/cam1 RTSP/1.0\r\nCSeq: 3\r\nSession: {session}\r\n\r\n",);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "TEARDOWN should return 200");

	drop(stream);
	cancel.cancel();
	// Allow the connection task's `clear()` to run.
	tokio::time::sleep(std::time::Duration::from_millis(100)).await;

	// Silence the unused-binding warning for client_rtcp — we hold it open so
	// the port stays reserved during the test.
	drop(client_rtcp);

	assert!(
		guard_drops.load(Ordering::SeqCst) >= 1,
		"expected at least one guard drop after UDP teardown",
	);
}

// ----------------------------------------------------------------------------
// Task 30 — release guards on TCP disconnect and on session keepalive
// expiry. The keepalive case is gated behind `#[ignore]` because it has to
// wait out the >30s sweep window.
// ----------------------------------------------------------------------------

/// Drive an RTSP session up to PLAY (TCP-interleaved) and return the live
/// stream + session id so the caller can decide how to terminate it.
async fn setup_and_play_tcp(addr: SocketAddr) -> (TcpStream, String) {
	let mut stream = TcpStream::connect(addr).await.unwrap();
	let req = format!(
		"SETUP rtsp://{addr}/cam1/trackID=0 RTSP/1.0\r\n\
		 CSeq: 1\r\n\
		 Transport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n",
	);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "SETUP should return 200");
	let session = session_id_from(&resp.headers);
	let req = format!("PLAY rtsp://{addr}/cam1 RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n",);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "PLAY should return 200");
	(stream, session)
}

#[tokio::test]
async fn tcp_disconnect_releases_guard() {
	let provider = MockProvider::new(&["cam1"]);
	let guard_drops = Arc::clone(&provider.guard_drops);
	let (addr, cancel) = spawn_server_with(provider, vec![]).await;

	let (stream, _session) = setup_and_play_tcp(addr).await;

	// Drop the TCP socket — this is a hard disconnect; the server's read
	// loop should observe EOF and call `sessions.clear()`, dropping the
	// guard.
	drop(stream);

	// Poll up to ~1s for the guard drop to register.
	let mut observed = 0;
	for _ in 0..10 {
		observed = guard_drops.load(Ordering::SeqCst);
		if observed >= 1 {
			break;
		}
		tokio::time::sleep(std::time::Duration::from_millis(100)).await;
	}
	cancel.cancel();
	assert!(
		observed >= 1,
		"expected guard drop within 1s of TCP disconnect; observed {observed}"
	);
}

#[tokio::test]
#[ignore = "slow: waits for the 30s keepalive sweep — run explicitly with `cargo test -- --ignored`"]
async fn session_timeout_releases_guard() {
	let provider = MockProvider::new(&["cam1"]);
	let guard_drops = Arc::clone(&provider.guard_drops);
	let (addr, cancel) = spawn_server_with(provider, vec![]).await;

	let (_stream, _session) = setup_and_play_tcp(addr).await;

	// Wait out the 30s idle window plus a bit of slack for the 5s sweep tick
	// to fire and the cancellation to propagate.
	tokio::time::sleep(std::time::Duration::from_secs(36)).await;

	let observed = guard_drops.load(Ordering::SeqCst);
	cancel.cancel();
	assert!(
		observed >= 1,
		"expected guard drop after the 30s keepalive sweep; observed {observed}"
	);
}

// ----------------------------------------------------------------------------
// Task 31 — Digest authentication: 401 challenge → success with correct
// response → rejection with a wrong password.
// ----------------------------------------------------------------------------

/// Hex-encoded MD5 of `input` — matches the server's internal helper.
fn md5_hex(input: &str) -> String {
	let digest = md5::compute(input.as_bytes());
	format!("{digest:x}")
}

/// Pull a single quoted-or-bare value out of a digest challenge string.
///
/// Handles both `key="value"` and `key=value` forms. Returns `None` when
/// `key=` is absent.
fn pick_digest_param(challenge: &str, key: &str) -> Option<String> {
	let needle = format!("{key}=");
	let start = challenge.find(&needle)? + needle.len();
	let rest = &challenge[start..];
	if let Some(stripped) = rest.strip_prefix('"') {
		let end = stripped.find('"')?;
		Some(stripped[..end].to_string())
	} else {
		let end = rest.find([',', ' ']).unwrap_or(rest.len());
		Some(rest[..end].to_string())
	}
}

#[tokio::test]
async fn digest_auth_flow() {
	let users = vec![UserCred {
		name: "alice".into(),
		password: "wonderland".into(),
	}];
	let provider = MockProvider::new(&["cam1"]);
	let (addr, cancel) = spawn_server_with(provider, users).await;

	let uri = format!("rtsp://{addr}/cam1");

	// The server's nonce is per-connection (see `ConnectionState`), so all
	// three DESCRIBEs in this test must travel on a single TCP stream — the
	// challenge nonce wouldn't be valid on a fresh connection.
	let mut stream = TcpStream::connect(addr).await.unwrap();

	// 1. DESCRIBE without credentials → 401 + WWW-Authenticate: Digest.
	let req = format!("DESCRIBE {uri} RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n",);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 401, "first DESCRIBE should be 401");
	let challenge = resp
		.headers
		.get("www-authenticate")
		.expect("missing WWW-Authenticate")
		.clone();
	assert!(
		challenge.to_ascii_lowercase().starts_with("digest"),
		"WWW-Authenticate must be a Digest challenge: {challenge}"
	);
	let realm = pick_digest_param(&challenge, "realm").expect("missing realm");
	let nonce = pick_digest_param(&challenge, "nonce").expect("missing nonce");

	// 2. DESCRIBE with the correct credentials → 200.
	let nc = "00000001";
	let cnonce = "bairelay-test-cnonce";
	let qop = "auth";
	let ha1 = md5_hex(&format!("alice:{realm}:wonderland"));
	let ha2 = md5_hex(&format!("DESCRIBE:{uri}"));
	let response = md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}"));
	let authz = format!(
		r#"Digest username="alice", realm="{realm}", nonce="{nonce}", uri="{uri}", response="{response}", qop={qop}, nc={nc}, cnonce="{cnonce}""#
	);
	let req = format!(
		"DESCRIBE {uri} RTSP/1.0\r\nCSeq: 2\r\nAuthorization: {authz}\r\nAccept: application/sdp\r\n\r\n",
	);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(resp.status, 200, "DESCRIBE with valid digest should be 200");

	// 3. DESCRIBE with the WRONG password → 401 or 403.
	let bad_ha1 = md5_hex(&format!("alice:{realm}:not-the-password"));
	let bad_response = md5_hex(&format!("{bad_ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}"));
	let bad_authz = format!(
		r#"Digest username="alice", realm="{realm}", nonce="{nonce}", uri="{uri}", response="{bad_response}", qop={qop}, nc={nc}, cnonce="{cnonce}""#
	);
	let req = format!(
		"DESCRIBE {uri} RTSP/1.0\r\nCSeq: 3\r\nAuthorization: {bad_authz}\r\nAccept: application/sdp\r\n\r\n",
	);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert!(
		resp.status == 401 || resp.status == 403,
		"DESCRIBE with bad password should be 401 or 403, got {}",
		resp.status
	);

	drop(stream);
	cancel.cancel();
}

// ----------------------------------------------------------------------------
// Task 32 — fanout to multiple concurrent clients, plus extern→sub fallback.
// ----------------------------------------------------------------------------

/// Read until at least one TCP-interleaved RTP frame (`$ ch len ...`) has
/// been consumed, or the timeout expires. Returns the channel byte of the
/// first frame seen.
async fn await_interleaved_rtp(stream: &mut TcpStream, timeout: std::time::Duration) -> Option<u8> {
	let mut buf = Vec::new();
	let mut tmp = [0u8; 4096];
	let deadline = tokio::time::Instant::now() + timeout;
	loop {
		let now = tokio::time::Instant::now();
		if now >= deadline {
			return None;
		}
		// Drain whatever is available within the remaining budget.
		match tokio::time::timeout_at(deadline, stream.read(&mut tmp)).await {
			Ok(Ok(0)) => return None,
			Ok(Ok(n)) => buf.extend_from_slice(&tmp[..n]),
			Ok(Err(_)) => return None,
			Err(_) => return None,
		}
		// Skip any preceding RTSP-text bytes; we want the first $ frame.
		if let Some(start) = buf.iter().position(|b| *b == 0x24) {
			let remaining = &buf[start..];
			if remaining.len() >= 4 {
				let len = u16::from_be_bytes([remaining[2], remaining[3]]) as usize;
				if remaining.len() >= 4 + len {
					return Some(remaining[1]);
				}
			}
		}
	}
}

#[tokio::test]
async fn multi_client_fanout() {
	let provider = MockProvider::new(&["cam1"]);
	let frames_tx = provider.frames_tx.clone();
	let guard_drops = Arc::clone(&provider.guard_drops);
	let (addr, cancel) = spawn_server_with(provider, vec![]).await;

	// Two independent clients, each running its own SETUP/PLAY ramp-up.
	let (mut s1, _sid1) = setup_and_play_tcp(addr).await;
	let (mut s2, _sid2) = setup_and_play_tcp(addr).await;

	// Allow both session tasks to attach to the broadcast.
	tokio::time::sleep(std::time::Duration::from_millis(100)).await;

	// One frame, both clients should observe interleaved RTP.
	let _ = frames_tx.send(keyframe_access_unit(9000));

	let ch1 = await_interleaved_rtp(&mut s1, std::time::Duration::from_secs(2)).await;
	let ch2 = await_interleaved_rtp(&mut s2, std::time::Duration::from_secs(2)).await;
	assert_eq!(ch1, Some(0), "client 1 should receive RTP on channel 0");
	assert_eq!(ch2, Some(0), "client 2 should receive RTP on channel 0");

	// Drop client 1 → client 2 must keep receiving.
	drop(s1);
	tokio::time::sleep(std::time::Duration::from_millis(100)).await;
	let _ = frames_tx.send(keyframe_access_unit(18000));
	let ch2b = await_interleaved_rtp(&mut s2, std::time::Duration::from_secs(2)).await;
	assert_eq!(
		ch2b,
		Some(0),
		"client 2 should still receive after client 1 drop"
	);

	drop(s2);
	cancel.cancel();
	tokio::time::sleep(std::time::Duration::from_millis(150)).await;

	assert!(
		guard_drops.load(Ordering::SeqCst) >= 2,
		"expected >= 2 guard drops (one per client); got {}",
		guard_drops.load(Ordering::SeqCst)
	);
}

#[tokio::test]
async fn extern_fallback_to_sub() {
	// Provider reports cam1's Extern stream as Unavailable; the server
	// should silently fall back to Sub on both DESCRIBE and SETUP.
	let provider = MockProvider::with_unavailable(&["cam1"], &[("cam1", StreamKind::Extern)]);
	let (addr, cancel) = spawn_server_with(provider, vec![]).await;

	let mut stream = TcpStream::connect(addr).await.unwrap();

	// DESCRIBE /cam1/extern → 200 (server falls back to Sub).
	let req = format!(
		"DESCRIBE rtsp://{addr}/cam1/extern RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n",
	);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(
		resp.status, 200,
		"DESCRIBE /cam1/extern should fall back to Sub and return 200"
	);
	let body = std::str::from_utf8(&resp.body).expect("SDP body should be UTF-8");
	assert!(
		body.starts_with("v=0"),
		"fallback SDP should still be valid"
	);

	// SETUP /cam1/extern → 200 (same fallback path).
	let req = format!(
		"SETUP rtsp://{addr}/cam1/extern/trackID=0 RTSP/1.0\r\n\
		 CSeq: 2\r\n\
		 Transport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n",
	);
	write_request(&mut stream, req.as_bytes()).await;
	let resp = read_rtsp_response(&mut stream).await;
	assert_eq!(
		resp.status, 200,
		"SETUP /cam1/extern should fall back to Sub and return 200"
	);

	drop(stream);
	cancel.cancel();
}
