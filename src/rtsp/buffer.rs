//! Last-frame buffer for RTSP placeholder and MQTT preview.

use bytes::Bytes;
use std::sync::RwLock;
use std::time::Instant;

use crate::rtsp::codec::VideoCodec;

/// A self-decodable video burst: parameter sets + I-frame + trailing P-frames.
///
/// Parameter sets are codec-specific headers (SPS/PPS for H.264, VPS/SPS/PPS
/// for H.265) that a decoder needs before it can interpret any frame. The
/// I-frame is a full keyframe; P-frames are deltas applied on top. Together
/// these form a minimal, self-contained sequence that a newly attached
/// RTSP client can decode without waiting for the next keyframe from the
/// camera.
#[derive(Debug, Clone)]
pub struct VideoBurst {
	/// Codec used for the NAL units in this burst.
	pub codec: VideoCodec,
	/// Codec parameter sets (SPS/PPS for H.264, VPS/SPS/PPS for H.265).
	pub parameter_sets: Vec<Vec<u8>>,
	/// NAL units that make up the keyframe (I-frame).
	pub iframe_nals: Vec<Vec<u8>>,
	/// NAL units for each P-frame captured after the keyframe, in order.
	pub pframe_nals: Vec<Vec<Vec<u8>>>,
	/// Monotonic timestamp recording when the burst was captured.
	pub captured_at: Instant,
	/// The camera's 90 kHz RTP timestamp at the moment this burst's
	/// I-frame was captured. Used by the session send loop to replay
	/// the burst with a timestamp continuous with the live stream
	/// instead of a hardcoded `0` — without this, a new client sees
	/// a 500+ packet cached burst at ts=0 followed by live packets at
	/// ts=millions-of-ticks, and any downstream re-muxer (HA's go2rtc
	/// `ffmpeg:` wrap in particular) breaks on the timestamp jump.
	pub captured_pts_90khz: u32,
}

/// Combined buffer per camera: video burst for RTSP, JPEG for MQTT.
///
/// Uses [`std::sync::RwLock`] (not the tokio variant) so both blocking and
/// async call sites can read cheaply without awaiting. Writes are rare
/// (one per keyframe/P-frame/snapshot), reads are frequent (one per
/// attached RTSP client), making the read/write asymmetry a good fit.
pub struct LastFrameBuffer {
	video: RwLock<Option<VideoBurst>>,
	jpeg: RwLock<Option<Bytes>>,
}

impl LastFrameBuffer {
	/// Create an empty buffer with no video burst and no JPEG.
	pub fn new() -> Self {
		Self {
			video: RwLock::new(None),
			jpeg: RwLock::new(None),
		}
	}

	/// Replace the current video burst with a new one (typically on keyframe).
	pub fn replace_video(&self, burst: VideoBurst) {
		*self.video.write().expect("video lock poisoned") = Some(burst);
	}

	/// Append a P-frame's NAL units to the current burst. No-op if the
	/// buffer has not yet received a keyframe.
	///
	/// Caps the cumulative byte size of `pframe_nals` at
	/// [`Self::MAX_PFRAME_BYTES`] (8 MiB) and drops the oldest frames
	/// to make room. Argus IDR cadence (1–2 s GOPs) keeps the typical
	/// working set at a few MB; the cap defends against malformed or
	/// extreme-low-bitrate streams that never produce a keyframe and
	/// would otherwise grow this Vec without bound until the next IDR.
	pub fn append_pframe(&self, nals: Vec<Vec<u8>>) {
		if let Some(burst) = self.video.write().expect("video lock poisoned").as_mut() {
			let new_bytes: usize = nals.iter().map(|n| n.len()).sum();
			burst.pframe_nals.push(nals);
			let mut total: usize = burst
				.pframe_nals
				.iter()
				.map(|frame| frame.iter().map(|n| n.len()).sum::<usize>())
				.sum();
			if total + new_bytes > Self::MAX_PFRAME_BYTES {
				// Already accounted for `new_bytes` via the push above;
				// recompute total honestly and evict the oldest entries
				// until we fit under the cap.
				while total > Self::MAX_PFRAME_BYTES && !burst.pframe_nals.is_empty() {
					let dropped: usize = burst.pframe_nals[0].iter().map(|n| n.len()).sum();
					burst.pframe_nals.remove(0);
					total = total.saturating_sub(dropped);
				}
			}
		}
	}

	/// Cumulative-byte cap for [`VideoBurst::pframe_nals`].
	///
	/// 8 MiB is well above any observed Argus working set — typical
	/// per-GOP P-frame totals are a few hundred KiB at 4K HEVC, and
	/// the burst is replaced wholesale on each IDR. The cap exists to
	/// bound the worst case (no IDR for many seconds, or attacker-
	/// shaped malformed streams) without truncating any normal stream.
	pub const MAX_PFRAME_BYTES: usize = 8 * 1024 * 1024;

	/// Return a clone of the current video burst, or `None` if none captured yet.
	pub fn video_snapshot(&self) -> Option<VideoBurst> {
		self.video.read().expect("video lock poisoned").clone()
	}

	/// Return `true` if a video burst has been captured.
	pub fn has_video(&self) -> bool {
		self.video.read().expect("video lock poisoned").is_some()
	}

	/// Replace the stored JPEG preview with a new one.
	pub fn set_jpeg(&self, bytes: Bytes) {
		*self.jpeg.write().expect("jpeg lock poisoned") = Some(bytes);
	}

	/// Return a clone of the stored JPEG, or `None` if none captured yet.
	pub fn jpeg(&self) -> Option<Bytes> {
		self.jpeg.read().expect("jpeg lock poisoned").clone()
	}
}

impl Default for LastFrameBuffer {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Arc;
	use std::thread;

	fn sample_burst() -> VideoBurst {
		VideoBurst {
			codec: VideoCodec::H264,
			parameter_sets: vec![vec![0x67, 0x42, 0x00, 0x1f], vec![0x68, 0xce]],
			iframe_nals: vec![vec![0x65, 0xaa]],
			pframe_nals: vec![],
			captured_at: Instant::now(),
			captured_pts_90khz: 0,
		}
	}

	#[test]
	fn empty_buffer_has_no_video_or_jpeg() {
		let b = LastFrameBuffer::new();
		assert!(!b.has_video());
		assert!(b.jpeg().is_none());
	}

	#[test]
	fn replace_then_snapshot_returns_burst() {
		let b = LastFrameBuffer::new();
		b.replace_video(sample_burst());
		let s = b.video_snapshot().unwrap();
		assert_eq!(s.codec, VideoCodec::H264);
	}

	#[test]
	fn append_pframe_grows_burst() {
		let b = LastFrameBuffer::new();
		b.replace_video(sample_burst());
		b.append_pframe(vec![vec![0x41, 0xbb]]);
		b.append_pframe(vec![vec![0x41, 0xcc]]);
		let s = b.video_snapshot().unwrap();
		assert_eq!(s.pframe_nals.len(), 2);
	}

	#[test]
	fn append_pframe_without_burst_is_noop() {
		let b = LastFrameBuffer::new();
		b.append_pframe(vec![vec![0x41]]);
		assert!(!b.has_video());
	}

	/// Cumulative-byte cap: pushing 9 × 1 MiB P-frames into an 8 MiB
	/// budget must drop oldest until we fit. The total byte count
	/// after a sustained push must stay at or below MAX_PFRAME_BYTES
	/// regardless of how many frames arrive.
	#[test]
	fn append_pframe_drops_oldest_when_over_byte_cap() {
		let b = LastFrameBuffer::new();
		b.replace_video(sample_burst());
		let one_mib_payload = vec![0u8; 1024 * 1024];
		// Push 9 × 1 MiB P-frames; cap is 8 MiB.
		for _ in 0..9 {
			b.append_pframe(vec![one_mib_payload.clone()]);
		}
		let s = b.video_snapshot().unwrap();
		let total: usize = s
			.pframe_nals
			.iter()
			.map(|frame| frame.iter().map(|n| n.len()).sum::<usize>())
			.sum();
		assert!(
			total <= LastFrameBuffer::MAX_PFRAME_BYTES,
			"pframe total {total} exceeded MAX_PFRAME_BYTES {}",
			LastFrameBuffer::MAX_PFRAME_BYTES
		);
	}

	#[test]
	fn jpeg_round_trip() {
		let b = LastFrameBuffer::new();
		b.set_jpeg(Bytes::from_static(b"\xFF\xD8\xFF"));
		assert_eq!(b.jpeg().unwrap().len(), 3);
	}

	#[test]
	fn default_constructs_empty_buffer() {
		let b = LastFrameBuffer::default();
		assert!(!b.has_video());
		assert!(b.jpeg().is_none());
	}

	#[test]
	fn concurrent_readers_and_writers() {
		let b = Arc::new(LastFrameBuffer::new());
		let mut handles = vec![];

		for _ in 0..4 {
			let b = Arc::clone(&b);
			handles.push(thread::spawn(move || {
				for _ in 0..500 {
					b.replace_video(sample_burst());
				}
			}));
		}
		for _ in 0..8 {
			let b = Arc::clone(&b);
			handles.push(thread::spawn(move || {
				for _ in 0..500 {
					let _ = b.video_snapshot();
				}
			}));
		}
		for h in handles {
			h.join().unwrap();
		}
		assert!(b.has_video());
	}
}
