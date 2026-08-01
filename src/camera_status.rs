//! What a camera is doing, and where that gets reported.
//!
//! Pollers and listeners used to call the MQTT publisher directly and
//! then separately remember to update the republish cache. They now
//! describe the fact — a [`CameraEvent`] — and hand it to a
//! [`StatusReporter`], which owns both halves so they cannot drift.
//! Tests assert on the events rather than on a broker.

use async_trait::async_trait;
use bytes::Bytes;

/// Something a camera task observed and wants reported.
///
/// Deliberately reporting-shaped, not protocol-shaped: variants say
/// what is true of the camera, never how a broker should encode it.
/// Topic strings and payload formats live in the reporter
/// implementation (`src/mqtt_status.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraEvent {
	/// The camera session came up or went down.
	Connection(bool),
	/// Motion started or stopped.
	Motion(bool),
	/// Motion state is not yet known — published once at startup so
	/// subscribers don't inherit a stale retained value.
	MotionUnknown,
	/// Battery charge, already clamped to 0–100 at the wire boundary.
	BatteryLevel(u8),
	/// The floodlight turned on or off.
	Floodlight(bool),
	/// The floodlight *tasks* engine (its schedule) was enabled or
	/// disabled. Distinct from [`Self::Floodlight`]: one is the lamp,
	/// the other is the automation that drives it.
	FloodlightTasks(bool),
	/// The PIR sensor was enabled or disabled.
	Pir(bool),
	/// A fresh preview image (JPEG bytes, already overlaid).
	Preview(Bytes),
}

impl CameraEvent {
	/// Whether this event's value should survive a broker restart by
	/// being re-sent from the status cache on reconnect.
	///
	/// Connection state is republished by the connect/disconnect
	/// transitions themselves, and previews refresh on their own timer,
	/// so neither needs caching — see `status_cache`'s module docs.
	pub fn is_cacheable(&self) -> bool {
		matches!(
			self,
			CameraEvent::Motion(_)
				| CameraEvent::BatteryLevel(_)
				| CameraEvent::Floodlight(_)
				| CameraEvent::FloodlightTasks(_)
				| CameraEvent::Pir(_)
		)
	}
}

/// A status report failed to reach its destination.
///
/// Deliberately opaque: no caller branches on why a report failed —
/// they all log and carry on — so a broker's error taxonomy would be
/// detail without a consumer. Implementations flatten their own error
/// into the message.
#[derive(Debug, thiserror::Error)]
#[error("status report failed: {0}")]
pub struct StatusError(pub String);

/// Where a camera's status events go.
///
/// One instance is bound to one camera, so events carry no name.
/// Implementations translate events into whatever the outside world
/// speaks; `src/mqtt_status.rs` turns them into retained MQTT topics
/// and keeps the republish cache in step.
///
/// Declared here, next to the events it carries and the tasks that
/// produce them, rather than in the MQTT crate — what gets reported is
/// a camera concern, how it is encoded is the broker's.
#[async_trait]
pub trait StatusReporter: Send + Sync {
	async fn report(&self, event: CameraEvent) -> Result<(), StatusError>;
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cacheable_covers_exactly_the_republished_topics() {
		assert!(CameraEvent::Motion(true).is_cacheable());
		assert!(CameraEvent::BatteryLevel(50).is_cacheable());
		assert!(CameraEvent::Floodlight(true).is_cacheable());
		assert!(CameraEvent::FloodlightTasks(false).is_cacheable());
		assert!(CameraEvent::Pir(true).is_cacheable());

		// Republished by their own transitions / timers instead.
		assert!(!CameraEvent::Connection(true).is_cacheable());
		assert!(!CameraEvent::MotionUnknown.is_cacheable());
		assert!(!CameraEvent::Preview(Bytes::new()).is_cacheable());
	}
}
