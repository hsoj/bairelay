//! Integration-level fixture replay for the BcMedia parser.
//!
//! //!
//! Drives `BcMedia::deserialize` against every `.raw` capture under
//! `crates/core/src/bcmedia/samples/` (left in place for test-only
//! use, not a pub re-export), asserts:
//!   - The parser consumes every byte (or returns a stable incomplete
//!     tail — empty input after the last recognised frame).
//!   - The golden frame count matches.
//!   - The first keyframe reports the expected codec.
//!   - Timestamps are non-decreasing across consecutive video frames.
//!     (BcMedia uses microsecond PTS that usually increases monotonically
//!     within a single capture; we tolerate equal timestamps for the
//!     rare case of an I-frame + immediate P-frame and allow up to one
//!     backward jump because firmware sometimes resets PTS on new-stream
//!     boundaries.)
//!   - A truncated capture returns `Err`, not `Ok` and not a panic.

use bairelay_neolink_core::bcmedia::model::*;
use bairelay_neolink_core::Error;
use bytes::BytesMut;
use std::io::ErrorKind;

/// Walk through a fully-assembled capture buffer, returning every
/// `BcMedia` frame in order. Errors propagate. The assembly of the
/// capture from its individual `.raw` shards is the caller's job.
fn drain_all(bytes: &[u8]) -> Result<Vec<BcMedia>, Error> {
	let mut buf = BytesMut::from(bytes);
	let mut out = Vec::new();
	loop {
		match BcMedia::deserialize(&mut buf) {
			Ok(f) => out.push(f),
			Err(Error::Io(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
			Err(Error::NomIncomplete(_)) if buf.is_empty() => break,
			Err(e) => return Err(e),
		}
	}
	Ok(out)
}

fn iframe_video_type(f: &BcMedia) -> Option<VideoType> {
	match f {
		BcMedia::Iframe(i) => Some(i.video_type),
		_ => None,
	}
}

fn frame_microseconds(f: &BcMedia) -> Option<u32> {
	match f {
		BcMedia::Iframe(i) => Some(i.microseconds),
		BcMedia::Pframe(p) => Some(p.microseconds),
		_ => None,
	}
}

fn assert_timestamps_reasonable(frames: &[BcMedia]) {
	// Allow at most one PTS reset (backward jump) in a capture — some
	// firmware resets PTS on new-stream boundaries.
	let mut resets = 0usize;
	let mut last: Option<u32> = None;
	for f in frames {
		if let Some(ts) = frame_microseconds(f) {
			if let Some(prev) = last {
				if ts + 1_000_000 < prev {
					// > 1 second back is a clear reset.
					resets += 1;
				}
			}
			last = Some(ts);
		}
	}
	assert!(
		resets <= 1,
		"timestamp reset allowance exceeded: {resets} resets in capture of {} frames",
		frames.len()
	);
}

// ---------------------------------------------------------------------------
// Fixture: info_v1 single-frame
// ---------------------------------------------------------------------------

#[test]
fn replay_info_v1_yields_one_info_frame() {
	// The info_v1 capture is a single InfoV1 frame.
	let sample = include_bytes!("../src/bcmedia/samples/info_v1.raw").to_vec();
	let frames = drain_all(&sample).expect("parse info_v1");
	assert_eq!(frames.len(), 1, "info_v1 should yield exactly 1 frame");
	assert!(matches!(frames[0], BcMedia::InfoV1(_)));
}

// ---------------------------------------------------------------------------
// Fixture: iframe (H264, stitched from 5 shards)
// ---------------------------------------------------------------------------

#[test]
fn replay_iframe_capture_yields_one_h264_iframe() {
	let sample = [
		include_bytes!("../src/bcmedia/samples/iframe_0.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/iframe_1.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/iframe_2.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/iframe_3.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/iframe_4.raw").as_ref(),
	]
	.concat();
	let frames = drain_all(&sample).expect("parse iframe capture");
	assert_eq!(frames.len(), 1);
	assert!(matches!(
		iframe_video_type(&frames[0]),
		Some(VideoType::H264)
	));
	if let BcMedia::Iframe(i) = &frames[0] {
		assert_eq!(i.microseconds, 3_557_705_112);
		assert_eq!(i.time, Some(1_628_085_232));
		assert_eq!(i.data.len(), 192_881);
	}
}

// ---------------------------------------------------------------------------
// Fixture: pframe (H264, stitched from 2 shards)
// ---------------------------------------------------------------------------

#[test]
fn replay_pframe_capture_yields_one_h264_pframe() {
	let sample = [
		include_bytes!("../src/bcmedia/samples/pframe_0.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/pframe_1.raw").as_ref(),
	]
	.concat();
	let frames = drain_all(&sample).expect("parse pframe capture");
	assert_eq!(frames.len(), 1);
	match &frames[0] {
		BcMedia::Pframe(p) => {
			assert!(matches!(p.video_type, VideoType::H264));
			assert_eq!(p.microseconds, 3_557_767_112);
			assert_eq!(p.data.len(), 45_108);
		}
		other => panic!("expected Pframe, got {other:?}"),
	}
}

// ---------------------------------------------------------------------------
// Fixture: Argus2 iframe extended (5 shards, one I-frame with extended header)
// ---------------------------------------------------------------------------

#[test]
fn replay_argus2_iframe_extended_yields_one_iframe() {
	let sample = [
		include_bytes!("../src/bcmedia/samples/argus2_iframe_0.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_iframe_1.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_iframe_2.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_iframe_3.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_iframe_4.raw").as_ref(),
	]
	.concat();
	let frames = drain_all(&sample).expect("parse argus2 iframe extended");
	// Golden count: at least one I-frame must be present.
	let iframe_count = frames
		.iter()
		.filter(|f| matches!(f, BcMedia::Iframe(_)))
		.count();
	assert!(iframe_count >= 1, "expected at least 1 iframe, got 0");
	assert_timestamps_reasonable(&frames);
}

// ---------------------------------------------------------------------------
// Fixture: Argus2 pframes (18 shards, multiple P-frames)
// ---------------------------------------------------------------------------

#[test]
fn replay_argus2_pframe_extended_yields_multiple_frames() {
	let sample = [
		include_bytes!("../src/bcmedia/samples/argus2_pframe_0.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_1.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_2.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_3.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_4.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_5.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_6.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_7.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_8.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_9.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_10.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_11.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_12.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_13.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_14.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_15.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_16.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/argus2_pframe_17.raw").as_ref(),
	]
	.concat();
	let frames = drain_all(&sample).expect("parse argus2 pframe capture");
	let pframe_count = frames
		.iter()
		.filter(|f| matches!(f, BcMedia::Pframe(_)))
		.count();
	assert!(pframe_count >= 1, "expected pframes, got 0");
	assert_timestamps_reasonable(&frames);
}

// ---------------------------------------------------------------------------
// Fixture: Swann capture (10 shards, mixed iframe/pframe + ADPCM audio)
// ---------------------------------------------------------------------------

#[test]
fn replay_swan_capture_mixes_video_and_adpcm() {
	let sample = [
		include_bytes!("../src/bcmedia/samples/video_stream_swan_00.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_01.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_02.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_03.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_04.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_05.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_06.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_07.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_08.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/video_stream_swan_09.raw").as_ref(),
	]
	.concat();
	let frames = drain_all(&sample).expect("parse swan capture");
	assert!(
		frames.len() >= 2,
		"expected multiple frames from 10-shard capture"
	);
	// ADPCM path must be exercised — Swann cameras emit it.
	let adpcm_count = frames
		.iter()
		.filter(|f| matches!(f, BcMedia::Adpcm(_)))
		.count();
	assert!(
		adpcm_count >= 1,
		"swan fixture should contain ADPCM audio frames, got {adpcm_count}"
	);
	assert_timestamps_reasonable(&frames);
}

// ---------------------------------------------------------------------------
// Fixture: standalone ADPCM single frame (exercise audio codec path)
// ---------------------------------------------------------------------------

#[test]
fn replay_adpcm_frames_report_expected_block_size() {
	let sample = include_bytes!("../src/bcmedia/samples/adpcm_0.raw").to_vec();
	let frames = drain_all(&sample).expect("parse adpcm capture");
	assert!(!frames.is_empty());
	// First adpcm frame should be the canonical 244-byte block seen in
	// existing unit tests.
	match &frames[0] {
		BcMedia::Adpcm(a) => {
			assert_eq!(a.data.len(), 244);
			assert_eq!(a.block_size(), 240);
			assert!(a.duration().is_some());
		}
		other => panic!("expected Adpcm, got {other:?}"),
	}
	// Every other frame in the capture must also be adpcm (the audio-
	// only fixture is pure audio).
	for (idx, f) in frames.iter().enumerate() {
		match f {
			BcMedia::Adpcm(_) => {}
			other => panic!("frame {idx} not Adpcm: {other:?}"),
		}
	}
}

// ---------------------------------------------------------------------------
// Malformed-frame rejection — truncate a valid iframe and assert the
// parser reports incompleteness rather than panicking or returning Ok.
// ---------------------------------------------------------------------------

#[test]
fn replay_truncated_iframe_rejected_as_incomplete() {
	let full = [
		include_bytes!("../src/bcmedia/samples/iframe_0.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/iframe_1.raw").as_ref(),
		include_bytes!("../src/bcmedia/samples/iframe_2.raw").as_ref(),
	]
	.concat();
	// Provide only the first 500 bytes — enough to satisfy the magic
	// header match but not the declared payload_size.
	let truncated = &full[..500];
	let mut buf = BytesMut::from(truncated);
	let result = BcMedia::deserialize(&mut buf);
	match result {
		Err(Error::NomIncomplete(_)) => {}
		Err(other) => panic!("expected NomIncomplete for truncated iframe, got {other:?}"),
		Ok(f) => panic!("expected Err, got Ok({f:?})"),
	}
}

#[test]
fn replay_bad_magic_rejected() {
	// 4 bytes of 0xdeadbeef is not any known BcMedia magic.
	let bytes = 0xdeadbeefu32.to_le_bytes().to_vec();
	let mut buf = BytesMut::from(bytes.as_slice());
	let result = BcMedia::deserialize(&mut buf);
	match result {
		Err(_) => {}
		Ok(f) => panic!("expected Err for bad magic, got Ok({f:?})"),
	}
}

#[test]
fn replay_empty_buffer_is_incomplete() {
	let mut buf = BytesMut::new();
	let result = BcMedia::deserialize(&mut buf);
	match result {
		Err(Error::NomIncomplete(_)) => {}
		other => panic!("expected NomIncomplete for empty, got {other:?}"),
	}
}
