//! `capture` one-shot: record raw `BcMedia` packets to a `.bcmedia`
//! fixture file.
//!
//! This is a reverse-engineering aid, not a streaming feature: it pulls
//! the camera's wire packets for a bounded window and mirrors them to
//! disk via [`FrameDumper`] — the same writer the daemon's
//! `--dump-bcmedia` flag drives — so the output is byte-identical in
//! format to daemon captures and feeds `tests/fixture_replay.rs` and
//! offline protocol analysis directly. No RTSP server, no MQTT, no
//! wake-lock machinery: connect, pull, write, disconnect.
//!
//! Tested entirely through the [`Video`]/[`VideoStream`] trait seams
//! with scripted streams — no hardware is required by (or reachable
//! from) `cargo test`.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::baichuan::bc_protocol::{StreamKind, VideoStream};
use crate::bcmedia_dump::{BcMediaDumpConfig, FrameDumper};
use crate::camera::Video;
use crate::oneshot::output::Outcome;
use crate::rtsp::url::StreamKind as RtspStreamKind;

/// Record `duration` worth of `BcMedia` packets from `cam`'s `bc_kind`
/// stream into `<out_dir>/<camera_name>-<rtsp_kind>.bcmedia`.
///
/// The window elapsing is the *success* condition. A stream error mid-
/// capture ends the recording early: if packets were already written
/// the partial fixture is kept and reported (a truncated sample is
/// still a sample), while an error — or the window elapsing — with
/// zero packets written is a failed capture and returns `Err`, so the
/// operator never mistakes an empty file for a fixture.
pub async fn run<C: Video + ?Sized>(
	cam: &C,
	camera_name: &str,
	out_dir: &Path,
	bc_kind: StreamKind,
	rtsp_kind: RtspStreamKind,
	duration: Duration,
) -> Result<Outcome> {
	let config = BcMediaDumpConfig::new(out_dir);
	let mut dumper = FrameDumper::create(&config, camera_name, rtsp_kind)
		.with_context(|| format!("cannot open capture file under {}", out_dir.display()))?;

	let mut stream = cam
		.start_video(bc_kind)
		.await
		.context("start_video failed")?;
	let outcome = drain_for_window(&mut *stream, &mut dumper, duration).await;
	let _ = cam.stop_video(bc_kind).await;
	drop(dumper); // flush on Drop before we stat the file

	let packets = match outcome {
		DrainOutcome::WindowElapsed { packets } | DrainOutcome::StreamEnded { packets }
			if packets > 0 =>
		{
			packets
		}
		DrainOutcome::WindowElapsed { .. } => {
			anyhow::bail!(
				"no BcMedia packets received in {}s — stream never started (wrong stream kind, \
				 or the camera refused the preview)",
				duration.as_secs()
			)
		}
		DrainOutcome::StreamEnded { .. } => {
			anyhow::bail!("stream errored before any BcMedia packet arrived")
		}
	};

	let path = config.bcmedia_path(camera_name, rtsp_kind);
	let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
	Ok(Outcome::Capture {
		packets,
		bytes,
		path: path.display().to_string(),
	})
}

/// How one capture window ended.
enum DrainOutcome {
	/// The full window elapsed — the normal end of a capture.
	WindowElapsed { packets: u64 },
	/// The stream returned an error before the window elapsed. The
	/// packets already written stay on disk.
	StreamEnded { packets: u64 },
}

/// Pull packets off `stream` until `window` elapses, mirroring each to
/// `dumper`. Camera-side parse errors on a single packet are logged and
/// skipped (the surrounding packets are still useful for analysis);
/// transport errors end the window.
async fn drain_for_window(
	stream: &mut dyn VideoStream,
	dumper: &mut FrameDumper,
	window: Duration,
) -> DrainOutcome {
	let deadline = tokio::time::Instant::now() + window;
	let mut packets: u64 = 0;
	loop {
		let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
		if remaining.is_zero() {
			return DrainOutcome::WindowElapsed { packets };
		}
		match tokio::time::timeout(remaining, stream.get_data()).await {
			Err(_) => return DrainOutcome::WindowElapsed { packets },
			Ok(Ok(Ok(packet))) => {
				dumper.write_frame(&packet);
				packets += 1;
			}
			Ok(Ok(Err(e))) => {
				tracing::warn!(error = %e, "capture: skipping unparseable packet");
			}
			Ok(Err(e)) => {
				tracing::warn!(error = %e, packets, "capture: stream ended early");
				return DrainOutcome::StreamEnded { packets };
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bcmedia::model::{BcMedia, BcMediaIframe, VideoType};
	use crate::fake_camera::{FakeCameraBuilder, MockStep, MockVideoStream};

	fn iframe() -> BcMedia {
		BcMedia::Iframe(BcMediaIframe {
			video_type: VideoType::H264,
			microseconds: 0,
			time: None,
			data: vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA],
		})
	}

	fn out_dir(tag: &str) -> std::path::PathBuf {
		let d = std::env::temp_dir().join(format!(
			"bairelay-capture-test-{tag}-{}",
			std::process::id()
		));
		let _ = std::fs::remove_dir_all(&d);
		d
	}

	#[tokio::test(flavor = "current_thread", start_paused = true)]
	async fn capture_writes_packets_until_window_elapses() {
		let dir = out_dir("happy");
		let fake = FakeCameraBuilder::new()
			.with_video_stream(Box::new(MockVideoStream::new(vec![
				MockStep::Frame(iframe()),
				MockStep::Frame(iframe()),
				MockStep::Frame(iframe()),
				MockStep::Hang, // idle camera; deadline ends the window
			])))
			.build();

		let outcome = tokio::time::timeout(
			Duration::from_secs(30),
			run(
				&*fake,
				"cam-cap",
				&dir,
				StreamKind::Main,
				RtspStreamKind::Main,
				Duration::from_secs(2),
			),
		)
		.await
		.expect("no hang")
		.expect("capture succeeds");

		match outcome {
			Outcome::Capture {
				packets,
				bytes,
				path,
			} => {
				assert_eq!(packets, 3);
				assert!(bytes > 0, "file must contain serialized packets");
				assert!(path.ends_with("cam-cap-main.bcmedia"), "path: {path}");
			}
			other => panic!("expected Capture outcome, got {other:?}"),
		}
		// Sidecar written alongside the fixture.
		assert!(dir.join("cam-cap-main.meta.json").exists());
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[tokio::test(flavor = "current_thread", start_paused = true)]
	async fn capture_with_zero_packets_is_an_error() {
		let dir = out_dir("empty");
		let fake = FakeCameraBuilder::new()
			.with_video_stream(Box::new(MockVideoStream::new(vec![MockStep::Hang])))
			.build();

		let err = tokio::time::timeout(
			Duration::from_secs(30),
			run(
				&*fake,
				"cam-empty",
				&dir,
				StreamKind::Main,
				RtspStreamKind::Main,
				Duration::from_millis(200),
			),
		)
		.await
		.expect("no hang")
		.expect_err("empty capture must fail");
		assert!(
			err.to_string().contains("no BcMedia packets"),
			"error: {err:#}"
		);
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[tokio::test(flavor = "current_thread", start_paused = true)]
	async fn capture_keeps_partial_fixture_on_mid_stream_error() {
		let dir = out_dir("partial");
		let fake = FakeCameraBuilder::new()
			.with_video_stream(Box::new(MockVideoStream::new(vec![
				MockStep::Frame(iframe()),
				MockStep::OuterErr(crate::baichuan::Error::Other("link dropped")),
			])))
			.build();

		let outcome = tokio::time::timeout(
			Duration::from_secs(30),
			run(
				&*fake,
				"cam-part",
				&dir,
				StreamKind::Main,
				RtspStreamKind::Main,
				Duration::from_secs(5),
			),
		)
		.await
		.expect("no hang")
		.expect("partial capture is still a capture");
		assert!(matches!(outcome, Outcome::Capture { packets: 1, .. }));
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[tokio::test(flavor = "current_thread", start_paused = true)]
	async fn capture_errors_when_stream_dies_before_first_packet() {
		let dir = out_dir("dead");
		let fake = FakeCameraBuilder::new()
			.with_video_stream(Box::new(MockVideoStream::new(vec![MockStep::OuterErr(
				crate::baichuan::Error::Other("refused"),
			)])))
			.build();

		let err = tokio::time::timeout(
			Duration::from_secs(30),
			run(
				&*fake,
				"cam-dead",
				&dir,
				StreamKind::Main,
				RtspStreamKind::Main,
				Duration::from_secs(5),
			),
		)
		.await
		.expect("no hang")
		.expect_err("must fail");
		assert!(err.to_string().contains("before any BcMedia"), "{err:#}");
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[tokio::test(flavor = "current_thread", start_paused = true)]
	async fn capture_skips_unparseable_packets_and_continues() {
		let dir = out_dir("skip");
		let fake = FakeCameraBuilder::new()
			.with_video_stream(Box::new(MockVideoStream::new(vec![
				MockStep::InnerErr(crate::baichuan::Error::Other("bad magic")),
				MockStep::Frame(iframe()),
				MockStep::Hang,
			])))
			.build();

		let outcome = tokio::time::timeout(
			Duration::from_secs(30),
			run(
				&*fake,
				"cam-skip",
				&dir,
				StreamKind::Main,
				RtspStreamKind::Main,
				Duration::from_millis(500),
			),
		)
		.await
		.expect("no hang")
		.expect("capture succeeds past a bad packet");
		assert!(matches!(outcome, Outcome::Capture { packets: 1, .. }));
		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The fixture must round-trip: what `capture` writes,
	/// `BcMedia::deserialize` reads back — the property the replay
	/// harness and offline analysis depend on.
	#[tokio::test(flavor = "current_thread", start_paused = true)]
	async fn captured_fixture_round_trips_through_deserialize() {
		use crate::baichuan::bcmedia::model::BcMedia as M;
		let dir = out_dir("roundtrip");
		let fake = FakeCameraBuilder::new()
			.with_video_stream(Box::new(MockVideoStream::new(vec![
				MockStep::Frame(iframe()),
				MockStep::Frame(iframe()),
				MockStep::Hang,
			])))
			.build();

		tokio::time::timeout(
			Duration::from_secs(30),
			run(
				&*fake,
				"cam-rt",
				&dir,
				StreamKind::Main,
				RtspStreamKind::Main,
				Duration::from_millis(500),
			),
		)
		.await
		.expect("no hang")
		.expect("capture succeeds");

		let bytes = std::fs::read(dir.join("cam-rt-main.bcmedia")).expect("fixture readable");
		let mut buf = bytes::BytesMut::from(bytes.as_slice());
		let mut seen = 0;
		while !buf.is_empty() {
			match M::deserialize(&mut buf) {
				Ok(M::Iframe(i)) => {
					assert_eq!(i.data, vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA]);
					seen += 1;
				}
				Ok(other) => panic!("unexpected packet in fixture: {other:?}"),
				Err(e) => panic!("fixture must round-trip, got {e:?}"),
			}
		}
		assert_eq!(seen, 2);
		let _ = std::fs::remove_dir_all(&dir);
	}
}
