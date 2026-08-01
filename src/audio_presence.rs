//! Per-camera audio presence state. Written by the stream-source reader
//! task and by `startup_wake::warm_one`; read by `CameraProvider::subscribe`
//! to decide whether to wait for an audio track's SDP params before
//! returning a subscription to the RTSP server.
//!
//! The state is monotonic per observation:
//!
//! - `Unknown` → `Present { codec }` on the first observed audio packet.
//! - `Unknown` → `Absent` when the startup-wake audio window expires.
//! - `Absent` → `Present { codec }` is permitted: a camera may start
//!   emitting audio mid-run (mic enabled via control path).
//! - `Present` never downgrades; transient audio drops are expected.

use crate::rtsp::codec::AudioCodec;

/// Per-camera audio presence state. See module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPresence {
	/// Never observed. Subscribe path waits a bounded bonus window.
	Unknown,
	/// Observed at least one audio packet. Subscribe waits for
	/// `SdpParams.audio.is_some()` in addition to video.
	Present { codec: AudioCodec },
	/// Observation window expired without audio. Subscribe skips the
	/// audio wait entirely — saves startup latency on video-only hardware.
	Absent,
}

impl AudioPresence {
	/// Fold a newly-observed codec into the current state.
	///
	/// `Unknown` or `Absent` → `Present { codec }`.
	/// `Present { old }` stays `Present { old }` (first-observation wins;
	/// mid-run codec changes are not expected on Reolink firmware).
	pub fn observed(self, codec: AudioCodec) -> Self {
		match self {
			AudioPresence::Unknown | AudioPresence::Absent => AudioPresence::Present { codec },
			AudioPresence::Present { .. } => self,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn unknown_observed_becomes_present() {
		let p = AudioPresence::Unknown.observed(AudioCodec::Aac);
		assert_eq!(
			p,
			AudioPresence::Present {
				codec: AudioCodec::Aac
			}
		);
	}

	#[test]
	fn absent_observed_upgrades_to_present() {
		let p = AudioPresence::Absent.observed(AudioCodec::G711Ulaw);
		assert_eq!(
			p,
			AudioPresence::Present {
				codec: AudioCodec::G711Ulaw
			},
		);
	}

	#[test]
	fn present_is_sticky_on_further_observations() {
		let start = AudioPresence::Present {
			codec: AudioCodec::Aac,
		};
		let after = start.observed(AudioCodec::G711Ulaw);
		assert_eq!(after, start, "first-observation wins");
	}
}
