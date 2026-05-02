//! The `StreamProvider` trait — the boundary between the RTSP server
//! layer and the application (binary) that owns cameras.
//!
//! Implementors: the bairelay binary's `CameraProvider`.

use std::sync::Arc;

use bytes::Bytes;
use thiserror::Error;
use tokio::sync::broadcast;

use crate::buffer::LastFrameBuffer;
use crate::codec::{AudioCodec, VideoCodec};
use crate::sdp::SdpParams;
use crate::url::StreamKind;

/// Opaque RAII guard the binary returns inside a [`SubscriptionHandle`].
///
/// Dropping it signals "this session is done" — the binary uses it to
/// release the wake lock.
pub type SessionGuard = Box<dyn Send + Sync>;

/// A single access unit worth of media data, streamed through a
/// `broadcast::Receiver` from the provider to each subscriber.
#[derive(Debug, Clone)]
pub enum Frame {
	/// Encoded video access unit (one or more NAL units).
	Video {
		/// Codec the NAL units belong to (H.264 or H.265).
		codec: VideoCodec,
		/// NAL unit bodies, Annex-B start codes stripped, in order.
		/// The first NAL is the first of the access unit; the last
		/// NAL of the access unit has `access_unit_end == true`.
		nals: Vec<Bytes>,
		/// Presentation timestamp in the 90 kHz RTP video clock.
		pts_90khz: u32,
		/// True when this access unit begins with an IDR/keyframe.
		keyframe: bool,
		/// True on the final NAL of an access unit (marker bit hint).
		access_unit_end: bool,
	},
	/// Encoded audio access unit.
	Audio {
		/// Payload — codec-specific.
		payload: AudioPayload,
		/// Presentation timestamp in the codec's own clock rate.
		pts: u32,
	},
}

/// Per-codec audio payload variants carried by [`Frame::Audio`].
#[derive(Debug, Clone)]
pub enum AudioPayload {
	/// AAC access unit with raw ADTS-free bytes and stream parameters.
	Aac {
		/// AAC access unit data (no ADTS header, no LATM framing).
		au_data: Bytes,
		/// Sample rate in Hz.
		sample_rate: u32,
		/// Channel count.
		channels: u8,
	},
	/// G.711 µ-law PCM samples (8 kHz, mono, 8-bit).
	G711Ulaw {
		/// Raw µ-law-encoded samples, one byte per sample.
		samples: Bytes,
	},
}

impl AudioPayload {
	/// Return the audio codec associated with this payload variant.
	pub fn codec(&self) -> AudioCodec {
		match self {
			AudioPayload::Aac { .. } => AudioCodec::Aac,
			AudioPayload::G711Ulaw { .. } => AudioCodec::G711Ulaw,
		}
	}
}

/// Returned by [`StreamProvider::subscribe`]. The caller drops this when
/// the RTSP session ends — doing so drops the guard (releases the wake
/// lock).
pub struct SubscriptionHandle {
	/// Receiver for the fan-out broadcast of encoded frames.
	pub frames: broadcast::Receiver<Frame>,
	/// SDP parameters for the session's `DESCRIBE` response.
	pub sdp_params: SdpParams,
	/// Shared last-frame buffer used for RTSP placeholder + MQTT preview.
	pub last_frame: Arc<LastFrameBuffer>,
	/// Opaque drop-guard; dropping releases the wake lock in the binary.
	pub guard: SessionGuard,
}

/// Errors returned by [`StreamProvider::subscribe`].
#[derive(Debug, Error)]
pub enum StreamError {
	/// No camera is configured under the requested name.
	#[error("unknown camera")]
	UnknownCamera,
	/// Authentication failed or the user lacks permission for this stream.
	#[error("access denied")]
	AccessDenied,
	/// The camera is configured but not currently available.
	#[error("stream unavailable: {0}")]
	Unavailable(String),
	/// An unexpected internal error occurred while handling the request.
	#[error("internal error: {0}")]
	Internal(String),
}

/// Abstraction the RTSP server layer uses to fetch live media from the
/// binary that owns the cameras.
#[async_trait::async_trait]
pub trait StreamProvider: Send + Sync + 'static {
	/// Subscribe to a camera's stream. Called on RTSP `SETUP`.
	async fn subscribe(
		&self,
		camera: &str,
		kind: StreamKind,
		authenticated_user: Option<&str>,
	) -> Result<SubscriptionHandle, StreamError>;
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Mutex;
	use tokio::sync::broadcast;

	struct MockProvider {
		guard_drops: Arc<Mutex<u32>>,
	}

	struct CountingGuard(Arc<Mutex<u32>>);
	impl Drop for CountingGuard {
		fn drop(&mut self) {
			*self.0.lock().unwrap() += 1;
		}
	}

	#[async_trait::async_trait]
	impl StreamProvider for MockProvider {
		async fn subscribe(
			&self,
			camera: &str,
			_kind: StreamKind,
			_user: Option<&str>,
		) -> Result<SubscriptionHandle, StreamError> {
			if camera != "cam1" {
				return Err(StreamError::UnknownCamera);
			}
			let (_tx, rx) = broadcast::channel::<Frame>(32);
			Ok(SubscriptionHandle {
				frames: rx,
				sdp_params: SdpParams {
					server_ip: "127.0.0.1".into(),
					session_id: "1".into(),
					session_name: camera.into(),
					video: None,
					audio: None,
				},
				last_frame: Arc::new(LastFrameBuffer::new()),
				guard: Box::new(CountingGuard(Arc::clone(&self.guard_drops))),
			})
		}
	}

	#[tokio::test]
	async fn drop_subscription_drops_guard() {
		let drops = Arc::new(Mutex::new(0u32));
		let p = MockProvider {
			guard_drops: Arc::clone(&drops),
		};
		let sub = p.subscribe("cam1", StreamKind::Main, None).await.unwrap();
		assert_eq!(*drops.lock().unwrap(), 0);
		drop(sub);
		assert_eq!(*drops.lock().unwrap(), 1);
	}

	#[tokio::test]
	async fn unknown_camera_returns_error() {
		let p = MockProvider {
			guard_drops: Arc::new(Mutex::new(0)),
		};
		let res = p.subscribe("missing", StreamKind::Main, None).await;
		assert!(matches!(res, Err(StreamError::UnknownCamera)));
	}

	#[test]
	fn audio_payload_codec_maps_each_variant() {
		let aac = AudioPayload::Aac {
			au_data: Bytes::from_static(b""),
			sample_rate: 48_000,
			channels: 2,
		};
		assert_eq!(aac.codec(), AudioCodec::Aac);

		let ulaw = AudioPayload::G711Ulaw {
			samples: Bytes::from_static(b""),
		};
		assert_eq!(ulaw.codec(), AudioCodec::G711Ulaw);
	}
}
