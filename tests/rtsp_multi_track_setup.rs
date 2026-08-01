//! Second SETUP on an existing Session ID appends the audio track
//! rather than returning 455.
//!
//! Exercises the Task 11 append path: a client issues SETUP for the
//! video track, then a second SETUP for the audio track on the same
//! session ID. The server MUST answer the second SETUP with 200 OK
//! (plus a Transport header echoing the new interleaved channel pair)
//! rather than the historical 455 MethodNotValidInThisState.
//!
//! Dispatch to the new audio track is Task 12's job; this test stops
//! at the SETUP handshake.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use bairelay::rtsp::buffer::LastFrameBuffer;
use bairelay::rtsp::codec::{AudioCodec, VideoCodec};
use bairelay::rtsp::provider::{
	AudioPayload, Frame, StreamError, StreamProvider, SubscriptionHandle,
};
use bairelay::rtsp::sdp::{AudioParams, SdpParams, VideoParams};
use bairelay::rtsp::server::{RtspServer, ServerConfig};
use bairelay::rtsp::url::StreamKind;

/// Provider that exposes a single camera `cam` with both a video and an
/// audio track so the generated SDP carries `m=video` and `m=audio`.
struct TwoTrackMockProvider;

#[async_trait]
impl StreamProvider for TwoTrackMockProvider {
	async fn subscribe(
		&self,
		_camera: &str,
		_kind: StreamKind,
		_user: Option<&str>,
	) -> Result<SubscriptionHandle, StreamError> {
		let (_tx, rx) = tokio::sync::broadcast::channel::<Frame>(8);
		Ok(SubscriptionHandle {
			frames: rx,
			sdp_params: SdpParams {
				server_ip: "127.0.0.1".into(),
				session_id: "1".into(),
				session_name: "cam".into(),
				video: Some(VideoParams {
					codec: VideoCodec::H264,
					payload_type: 96,
					sps: vec![0x67, 0x42, 0, 0x1f],
					pps: vec![0x68, 0xce, 0x38, 0x80],
					vps: None,
					profile_level_id: [0x42, 0, 0x1f],
				}),
				audio: Some(AudioParams {
					codec: AudioCodec::Aac,
					payload_type: 97,
					sample_rate: 16000,
					channels: 1,
					asc_hex: Some("1408".into()),
				}),
			},
			last_frame: Arc::new(LastFrameBuffer::new()),
			guard: Box::new(()),
		})
	}
}

/// Bind the server on a free loopback port and return `(addr, cancel)`.
///
/// Mirrors the retry loop used by `rtsp_integration_test.rs::spawn_server_with`
/// — grabbing `127.0.0.1:0` in one listener, dropping it, then having
/// `RtspServer::serve` rebind the same port is inherently race-prone on
/// loaded CI. Poll for readiness and retry on a fresh port if the bind
/// didn't win.
async fn spawn_server() -> (SocketAddr, CancellationToken) {
	const MAX_ATTEMPTS: u32 = 5;
	const READY_POLLS: u32 = 10;
	const READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

	for attempt in 0..MAX_ATTEMPTS {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		drop(listener);

		let config = ServerConfig {
			bind: addr,
			realm: "bairelay-test".to_string(),
			users: vec![],
			tls: None,
			max_connections: None,
		};
		let cancel = CancellationToken::new();
		let cancel_for_server = cancel.clone();
		let provider: Arc<dyn StreamProvider> = Arc::new(TwoTrackMockProvider);
		let server_task = tokio::spawn(async move {
			let _ = RtspServer::serve(config, provider, cancel_for_server).await;
		});

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

		cancel.cancel();
		let _ = server_task.await;
		eprintln!("spawn_server: attempt {attempt} could not reach {addr}, retrying");
	}
	panic!("spawn_server failed after {MAX_ATTEMPTS} attempts");
}

/// Drain bytes from `stream` until we see the end-of-headers marker
/// `\r\n\r\n`. Returns the raw response head (headers only) as a string.
/// Transparently skips any leading TCP-interleaved RTP frames (`$`) that
/// may arrive before the response — a safeguard copied from the /// harness, defensive against spurious frames on SETUP.
async fn read_response_head(stream: &mut TcpStream) -> String {
	let mut buf: Vec<u8> = Vec::new();
	let mut tmp = [0u8; 4096];
	loop {
		while !buf.is_empty() && buf[0] == 0x24 {
			if buf.len() < 4 {
				let n = stream.read(&mut tmp).await.unwrap();
				assert!(n > 0, "EOF before complete interleaved header");
				buf.extend_from_slice(&tmp[..n]);
				continue;
			}
			let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
			let need = 4 + len;
			while buf.len() < need {
				let n = stream.read(&mut tmp).await.unwrap();
				assert!(n > 0, "EOF before complete interleaved frame");
				buf.extend_from_slice(&tmp[..n]);
			}
			buf.drain(..need);
		}
		if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
			return std::str::from_utf8(&buf[..pos]).unwrap().to_string();
		}
		let n = tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut tmp))
			.await
			.expect("timeout waiting for RTSP response")
			.unwrap();
		assert!(n > 0, "connection closed before RTSP response");
		buf.extend_from_slice(&tmp[..n]);
	}
}

#[tokio::test]
async fn second_setup_on_existing_session_appends_track() {
	let (addr, cancel) = spawn_server().await;

	let mut client = TcpStream::connect(addr).await.unwrap();

	// DESCRIBE — confirm the mock provider returns an SDP with both
	// video and audio, so the subsequent SETUPs are meaningful.
	let req = format!(
		"DESCRIBE rtsp://{addr}/cam RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	// Read full response including SDP body. For DESCRIBE we need more
	// than just the head because Content-Length is set; grab a generous
	// buffer and inspect it.
	let mut buf = [0u8; 4096];
	let n = client.read(&mut buf).await.unwrap();
	let resp = std::str::from_utf8(&buf[..n]).unwrap();
	assert!(resp.starts_with("RTSP/1.0 200"), "DESCRIBE failed: {resp}");
	assert!(resp.contains("m=video"), "missing video SDP: {resp}");
	assert!(resp.contains("m=audio"), "missing audio SDP: {resp}");

	// SETUP video (trackID=0).
	let req = format!(
		"SETUP rtsp://{addr}/cam/trackID=0 RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	let resp = read_response_head(&mut client).await;
	assert!(resp.starts_with("RTSP/1.0 200"), "first SETUP: {resp}");

	let session_id = resp
		.lines()
		.find_map(|l| l.strip_prefix("Session: "))
		.and_then(|s| s.split(';').next())
		.expect("Session header in first SETUP")
		.to_string();

	// SETUP audio (trackID=1) on the SAME session — must return 200, not 455.
	let req = format!(
		"SETUP rtsp://{addr}/cam/trackID=1 RTSP/1.0\r\nCSeq: 3\r\nSession: {session_id}\r\nTransport: RTP/AVP/TCP;unicast;interleaved=2-3\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	let resp = read_response_head(&mut client).await;
	assert!(
		resp.starts_with("RTSP/1.0 200"),
		"second SETUP must be 200 OK, not 455. Got: {resp}"
	);
	assert!(
		resp.contains(&format!("Session: {session_id}")),
		"second SETUP response must echo the same Session ID: {resp}"
	);
	assert!(
		resp.contains("interleaved=2-3"),
		"second SETUP Transport header must echo interleaved=2-3: {resp}"
	);

	cancel.cancel();
}

/// Provider that retains its broadcast sender so the test can inject
/// frames after SETUP+PLAY. Unlike `TwoTrackMockProvider` (which drops
/// the tx immediately), this one keeps `video_tx` alive for the
/// lifetime of the provider.
struct EmittingMockProvider {
	tx: broadcast::Sender<Frame>,
}

impl EmittingMockProvider {
	fn new() -> Self {
		// Capacity 32 is comfortably above the burst of frames the
		// test emits (two). The session send loop subscribes once per
		// SETUP; a small buffer is enough.
		let (tx, _rx) = broadcast::channel::<Frame>(32);
		Self { tx }
	}
}

#[async_trait]
impl StreamProvider for EmittingMockProvider {
	async fn subscribe(
		&self,
		_camera: &str,
		_kind: StreamKind,
		_user: Option<&str>,
	) -> Result<SubscriptionHandle, StreamError> {
		Ok(SubscriptionHandle {
			frames: self.tx.subscribe(),
			sdp_params: SdpParams {
				server_ip: "127.0.0.1".into(),
				session_id: "1".into(),
				session_name: "cam".into(),
				video: Some(VideoParams {
					codec: VideoCodec::H264,
					payload_type: 96,
					sps: vec![0x67, 0x42, 0, 0x1f],
					pps: vec![0x68, 0xce, 0x38, 0x80],
					vps: None,
					profile_level_id: [0x42, 0, 0x1f],
				}),
				audio: Some(AudioParams {
					codec: AudioCodec::Aac,
					payload_type: 97,
					sample_rate: 16000,
					channels: 1,
					asc_hex: Some("1408".into()),
				}),
			},
			last_frame: Arc::new(LastFrameBuffer::new()),
			guard: Box::new(()),
		})
	}
}

/// Bind and spawn the RTSP server, returning `(addr, cancel, provider)`.
/// The `provider` is kept alive on the test side so the test can emit
/// frames on its broadcast sender after PLAY.
async fn spawn_server_with_emitting_provider(
) -> (SocketAddr, CancellationToken, Arc<EmittingMockProvider>) {
	const MAX_ATTEMPTS: u32 = 5;
	const READY_POLLS: u32 = 10;
	const READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

	for attempt in 0..MAX_ATTEMPTS {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		drop(listener);

		let config = ServerConfig {
			bind: addr,
			realm: "bairelay-test".to_string(),
			users: vec![],
			tls: None,
			max_connections: None,
		};
		let cancel = CancellationToken::new();
		let cancel_for_server = cancel.clone();
		let provider = Arc::new(EmittingMockProvider::new());
		let provider_for_server: Arc<dyn StreamProvider> = provider.clone();
		let server_task = tokio::spawn(async move {
			let _ = RtspServer::serve(config, provider_for_server, cancel_for_server).await;
		});

		let mut ready = false;
		for _ in 0..READY_POLLS {
			tokio::time::sleep(READY_POLL_INTERVAL).await;
			if TcpStream::connect(addr).await.is_ok() {
				ready = true;
				break;
			}
		}
		if ready {
			return (addr, cancel, provider);
		}

		cancel.cancel();
		let _ = server_task.await;
		eprintln!("spawn_server: attempt {attempt} could not reach {addr}, retrying");
	}
	panic!("spawn_server failed after {MAX_ATTEMPTS} attempts");
}

/// Read interleaved RTP frames from the TCP stream until we've seen at
/// least one on each of the two channels `video_ch` and `audio_ch`, or
/// the deadline expires. Returns `(video_pkt, audio_pkt)` — the raw
/// RTP payload (excluding the 4-byte interleaved header) of the FIRST
/// packet observed on each channel, or `None` if the deadline expired
/// before a packet arrived on that channel.
///
/// Any leading bytes that are not an interleaved frame header (`$`)
/// are discarded — Task 12 only cares about the dispatch landing on
/// the right channel.
async fn read_channels_until_both(
	stream: &mut TcpStream,
	video_ch: u8,
	audio_ch: u8,
	deadline: std::time::Duration,
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
	let mut buf: Vec<u8> = Vec::new();
	let mut tmp = [0u8; 4096];
	let mut video_pkt: Option<Vec<u8>> = None;
	let mut audio_pkt: Option<Vec<u8>> = None;
	let start = std::time::Instant::now();
	while (video_pkt.is_none() || audio_pkt.is_none()) && start.elapsed() < deadline {
		let remaining = deadline.saturating_sub(start.elapsed());
		if remaining.is_zero() {
			break;
		}
		let read = tokio::time::timeout(remaining, stream.read(&mut tmp)).await;
		match read {
			Ok(Ok(0)) => break,
			Ok(Ok(n)) => buf.extend_from_slice(&tmp[..n]),
			Ok(Err(_)) | Err(_) => break,
		}
		// Consume as many complete interleaved frames as we have.
		loop {
			if buf.is_empty() {
				break;
			}
			if buf[0] != 0x24 {
				// Non-interleaved byte (maybe a lingering RTSP line).
				// Skip one byte and try again.
				buf.drain(..1);
				continue;
			}
			if buf.len() < 4 {
				break;
			}
			let channel = buf[1];
			let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
			if buf.len() < 4 + len {
				break;
			}
			let payload = buf[4..4 + len].to_vec();
			if channel == video_ch && video_pkt.is_none() {
				video_pkt = Some(payload);
			} else if channel == audio_ch && audio_pkt.is_none() {
				audio_pkt = Some(payload);
			}
			buf.drain(..4 + len);
		}
	}
	(video_pkt, audio_pkt)
}

#[tokio::test]
async fn play_dispatches_video_and_audio_to_separate_channels() {
	let (addr, cancel, provider) = spawn_server_with_emitting_provider().await;

	let mut client = TcpStream::connect(addr).await.unwrap();

	// DESCRIBE — advertise both tracks.
	let req = format!(
		"DESCRIBE rtsp://{addr}/cam RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	let mut buf = [0u8; 4096];
	let n = client.read(&mut buf).await.unwrap();
	let resp = std::str::from_utf8(&buf[..n]).unwrap();
	assert!(resp.starts_with("RTSP/1.0 200"), "DESCRIBE failed: {resp}");

	// SETUP video on channels 0-1.
	let req = format!(
		"SETUP rtsp://{addr}/cam/trackID=0 RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	let resp = read_response_head(&mut client).await;
	assert!(resp.starts_with("RTSP/1.0 200"), "video SETUP: {resp}");
	let session_id = resp
		.lines()
		.find_map(|l| l.strip_prefix("Session: "))
		.and_then(|s| s.split(';').next())
		.expect("Session header")
		.to_string();

	// SETUP audio on channels 2-3, same session.
	let req = format!(
		"SETUP rtsp://{addr}/cam/trackID=1 RTSP/1.0\r\nCSeq: 3\r\nSession: {session_id}\r\nTransport: RTP/AVP/TCP;unicast;interleaved=2-3\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	let resp = read_response_head(&mut client).await;
	assert!(resp.starts_with("RTSP/1.0 200"), "audio SETUP: {resp}");

	// PLAY.
	let req =
		format!("PLAY rtsp://{addr}/cam RTSP/1.0\r\nCSeq: 4\r\nSession: {session_id}\r\n\r\n");
	client.write_all(req.as_bytes()).await.unwrap();
	let resp = read_response_head(&mut client).await;
	assert!(resp.starts_with("RTSP/1.0 200"), "PLAY: {resp}");

	// Give the session task a beat to pick up the audio track. The session
	// task was spawned on the first (video) SETUP and entered its loop with
	// a video-only runtime; the audio SETUP that just completed appended the
	// track and fired tracks_changed.notify_one(). The session loop's
	// tracks_notified arm wakes, rebuilds the RuntimeTrack list, and
	// continues — audio frames dispatch on channel 2 from there. See
	// `notify_wakes_parked_session_to_pick_up_appended_audio_track` for the
	// isolated regression test.
	tokio::time::sleep(std::time::Duration::from_millis(50)).await;

	// Emit one Frame::Video and one Frame::Audio. The test's
	// provider `tx` is the shared broadcast sender every
	// SubscriptionHandle was fanned out from.
	let video = Frame::Video {
		codec: VideoCodec::H264,
		nals: vec![Bytes::from_static(&[0x41, 0xAA, 0xBB, 0xCC])],
		pts_90khz: 9000,
		keyframe: false,
		access_unit_end: true,
	};
	provider.tx.send(video).expect("broadcast video");

	// Small gap so the video frame is fully dispatched before the
	// audio frame arrives — otherwise on a fast machine both may be
	// bunched into one select! iteration and processed serially,
	// which still works but is harder to reason about in the test.
	tokio::time::sleep(std::time::Duration::from_millis(20)).await;

	let audio = Frame::Audio {
		payload: AudioPayload::Aac {
			au_data: Bytes::from_static(&[0xAA; 20]),
			sample_rate: 16000,
			channels: 1,
		},
		pts: 16000,
	};
	provider.tx.send(audio).expect("broadcast audio");

	// Read for up to 2 s, asserting both channels land their frames.
	let (video_pkt, audio_pkt) =
		read_channels_until_both(&mut client, 0, 2, std::time::Duration::from_secs(2)).await;
	let video_pkt = video_pkt.expect("expected at least one RTP frame on video channel 0");
	let audio_pkt = audio_pkt.expect("expected at least one RTP frame on audio channel 2");

	// Tighten the regression surface — routing-by-channel is necessary
	// but not sufficient. Also verify the packets carry the right
	// payload type, distinct SSRCs (handle_setup generates them via
	// independent rand::random() calls), and non-zero sequence numbers
	// (uninitialised counters would show as 0/0).
	assert!(video_pkt.len() >= 12, "video RTP header too short");
	assert!(audio_pkt.len() >= 12, "audio RTP header too short");

	// RTP header byte layout: byte 0 = V/P/X/CC, byte 1 = M/PT. The
	// marker bit occupies the MSB of byte 1; mask it off to get the
	// 7-bit payload type.
	let video_pt = video_pkt[1] & 0x7F;
	let audio_pt = audio_pkt[1] & 0x7F;
	assert_eq!(video_pt, 96, "video PT must be 96 (H.264), got {video_pt}");
	assert_eq!(audio_pt, 97, "audio PT must be 97 (AAC), got {audio_pt}");

	let video_ssrc = u32::from_be_bytes([video_pkt[8], video_pkt[9], video_pkt[10], video_pkt[11]]);
	let audio_ssrc = u32::from_be_bytes([audio_pkt[8], audio_pkt[9], audio_pkt[10], audio_pkt[11]]);
	assert_ne!(
		video_ssrc, audio_ssrc,
		"video and audio SSRCs must differ (got {video_ssrc:#x} for both)",
	);

	let video_seq = u16::from_be_bytes([video_pkt[2], video_pkt[3]]);
	let audio_seq = u16::from_be_bytes([audio_pkt[2], audio_pkt[3]]);
	assert!(
		!(video_seq == 0 && audio_seq == 0),
		"both seq numbers zero suggests counters never initialised",
	);

	cancel.cancel();
}

/// Read interleaved RTP frames from `stream` until we've seen at least
/// one on `channel`, or the deadline expires. Returns the raw RTP
/// payload (excluding the 4-byte interleaved header) of the FIRST
/// matching packet, or `None` if the deadline expired first.
///
/// A lean twin of `read_channels_until_both` used by B4's tight-deadline
/// audio dispatch assertion.
async fn read_channel_first_packet(
	stream: &mut TcpStream,
	channel: u8,
	deadline: std::time::Duration,
) -> Option<Vec<u8>> {
	let mut buf: Vec<u8> = Vec::new();
	let mut tmp = [0u8; 4096];
	let start = std::time::Instant::now();
	while start.elapsed() < deadline {
		let remaining = deadline.saturating_sub(start.elapsed());
		if remaining.is_zero() {
			break;
		}
		let read = tokio::time::timeout(remaining, stream.read(&mut tmp)).await;
		match read {
			Ok(Ok(0)) => break,
			Ok(Ok(n)) => buf.extend_from_slice(&tmp[..n]),
			Ok(Err(_)) | Err(_) => break,
		}
		loop {
			if buf.is_empty() {
				break;
			}
			if buf[0] != 0x24 {
				buf.drain(..1);
				continue;
			}
			if buf.len() < 4 {
				break;
			}
			let ch = buf[1];
			let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
			if buf.len() < 4 + len {
				break;
			}
			let payload = buf[4..4 + len].to_vec();
			buf.drain(..4 + len);
			if ch == channel {
				return Some(payload);
			}
		}
	}
	None
}

/// Isolates B3's `tracks_changed.notify_one()` arm cleanly.
///
/// The sibling test `play_dispatches_video_and_audio_to_separate_channels`
/// also exercises the notify path — the session task is spawned on the
/// first SETUP (video-only), and the audio SETUP arrives against the
/// already-running task; the notify arm picks it up before PLAY completes.
/// But that test uses a 2-second deadline and sends both video and audio
/// frames, so in principle a future refactor could let the defensive
/// threshold path (4 consecutive no-track drops → forced rebuild) rescue
/// it silently.
///
/// This test pins the behaviour down: ONE audio frame, 50 ms deadline.
/// The threshold path needs 4 drops to trigger, which cannot happen with
/// a single frame, and no other mechanism exists to dispatch audio on a
/// runtime that still has only the video track. A pass is therefore
/// definitive evidence that the notify arm fired.
///
/// Parked-proof: before the audio SETUP, we SETUP video → PLAY → drive
/// video frames through the loop so the session task is demonstrably
/// parked on `frames.recv()` with a video-only runtime. Only then do we
/// issue the audio SETUP (which calls `SessionEntry::append_track` and
/// fires `tracks_changed.notify_one()`) and emit the single audio frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_wakes_parked_session_to_pick_up_appended_audio_track() {
	let (addr, cancel, provider) = spawn_server_with_emitting_provider().await;

	let mut client = TcpStream::connect(addr).await.unwrap();

	// DESCRIBE — advertise both tracks so SETUP audio is meaningful.
	let req = format!(
		"DESCRIBE rtsp://{addr}/cam RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	let mut buf = [0u8; 4096];
	let n = client.read(&mut buf).await.unwrap();
	let resp = std::str::from_utf8(&buf[..n]).unwrap();
	assert!(resp.starts_with("RTSP/1.0 200"), "DESCRIBE failed: {resp}");

	// SETUP video (trackID=0) on channels 0-1. This spawns the session
	// task with a video-only runtime track list.
	let req = format!(
		"SETUP rtsp://{addr}/cam/trackID=0 RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	let resp = read_response_head(&mut client).await;
	assert!(resp.starts_with("RTSP/1.0 200"), "video SETUP: {resp}");
	let session_id = resp
		.lines()
		.find_map(|l| l.strip_prefix("Session: "))
		.and_then(|s| s.split(';').next())
		.expect("Session header")
		.to_string();

	// PLAY — session task enters its loop; `rebuild_runtime` fires once
	// before the loop with video-only in `session_tracks`.
	let req =
		format!("PLAY rtsp://{addr}/cam RTSP/1.0\r\nCSeq: 3\r\nSession: {session_id}\r\n\r\n");
	client.write_all(req.as_bytes()).await.unwrap();
	let resp = read_response_head(&mut client).await;
	assert!(resp.starts_with("RTSP/1.0 200"), "PLAY: {resp}");

	// Drive a video frame through the loop so we *know* the session task
	// has entered its select! and parked on `frames.recv()`. Reading back
	// an RTP packet on channel 0 proves the task is actively dispatching.
	let video = Frame::Video {
		codec: VideoCodec::H264,
		nals: vec![Bytes::from_static(&[0x41, 0xAA, 0xBB, 0xCC])],
		pts_90khz: 9000,
		keyframe: false,
		access_unit_end: true,
	};
	provider.tx.send(video).expect("broadcast video");
	let video_pkt = read_channel_first_packet(&mut client, 0, std::time::Duration::from_secs(2))
		.await
		.expect("session task must dispatch initial video frame on channel 0");
	assert!(video_pkt.len() >= 12, "video RTP header too short");

	// Now the session task is provably parked on `frames.recv()` with a
	// video-only runtime. A second SETUP for audio fires
	// `tracks_changed.notify_one()` via `SessionEntry::append_track`.
	let req = format!(
		"SETUP rtsp://{addr}/cam/trackID=1 RTSP/1.0\r\nCSeq: 4\r\nSession: {session_id}\r\nTransport: RTP/AVP/TCP;unicast;interleaved=2-3\r\n\r\n"
	);
	client.write_all(req.as_bytes()).await.unwrap();
	let resp = read_response_head(&mut client).await;
	assert!(
		resp.starts_with("RTSP/1.0 200"),
		"second SETUP (audio) must be 200 OK: {resp}"
	);

	// Emit ONE audio frame. Without the notify arm, the session task
	// would drop this (and the next two) as `no_track_drops` ticks up to
	// the threshold of 4; only the 4th frame would trigger the defensive
	// rebuild and only the 5th would dispatch. One-shot therefore proves
	// the notify path, not the threshold path, picked up the new track.
	let audio = Frame::Audio {
		payload: AudioPayload::Aac {
			au_data: Bytes::from_static(&[0xAA; 20]),
			sample_rate: 16000,
			channels: 1,
		},
		pts: 16000,
	};
	provider.tx.send(audio).expect("broadcast audio");

	// Tight deadline: 50 ms is well under the threshold path's minimum
	// (~4 × frame-time at 30 Hz ≈ 133 ms), but comfortably above a
	// scheduler round-trip even on loaded CI. If this times out, the
	// notify arm is broken or the connection handler stopped passing the
	// notify to `session_task::run`.
	let audio_pkt = tokio::time::timeout(
		std::time::Duration::from_millis(50),
		read_channel_first_packet(&mut client, 2, std::time::Duration::from_millis(50)),
	)
	.await
	.expect("outer timeout must not elapse before inner returns")
	.expect("notify path must dispatch first audio frame within 50ms of append_track");

	assert!(audio_pkt.len() >= 12, "audio RTP header too short");
	let audio_pt = audio_pkt[1] & 0x7F;
	assert_eq!(audio_pt, 97, "audio PT must be 97 (AAC), got {audio_pt}");

	cancel.cancel();
}
