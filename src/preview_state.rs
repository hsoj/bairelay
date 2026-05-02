//! Per-camera state used to drive the MQTT preview overlay.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewState {
	/// Camera is connected and producing frames.
	Live,
	/// Camera is in its connect / reconnect / wake loop.
	Connecting,
	/// Camera is intentionally disconnected (grace period fired).
	Sleeping,
}

impl PreviewState {
	pub fn caption(&self) -> Option<&'static str> {
		match self {
			PreviewState::Live => None,
			PreviewState::Connecting => Some("CONNECTING"),
			PreviewState::Sleeping => Some("SLEEPING"),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn caption_live_is_none() {
		assert_eq!(PreviewState::Live.caption(), None);
	}

	#[test]
	fn caption_nonlive_has_text() {
		assert_eq!(PreviewState::Connecting.caption(), Some("CONNECTING"));
		assert_eq!(PreviewState::Sleeping.caption(), Some("SLEEPING"));
	}
}
