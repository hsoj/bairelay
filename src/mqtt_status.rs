//! Reporting camera status to MQTT.
//!
//! Implements [`StatusReporter`] over `mqtt`. This is the only
//! place that knows a [`CameraEvent`] becomes a retained publish, and
//! the only place that writes the republish cache — pairing the two
//! here is what stops a task from publishing a value it then forgets to
//! cache (or vice versa), which used to be a per-call-site obligation.
//!
//! Topic strings and payload encodings stay in `mqtt`; the
//! choice of *what* is worth reporting stays with the camera.

use std::sync::Arc;

use crate::mqtt::{SharedMqttClient, StatusPublisher};
use async_trait::async_trait;

use crate::camera_status::{CameraEvent, StatusError, StatusReporter};
use crate::status_cache::StatusCache;

/// Per-camera MQTT status reporter.
pub struct MqttStatusReporter {
	client: SharedMqttClient,
	topic_prefix: String,
	camera_name: String,
	cache: Arc<StatusCache>,
}

impl MqttStatusReporter {
	pub fn new(
		client: SharedMqttClient,
		topic_prefix: &str,
		camera_name: &str,
		cache: Arc<StatusCache>,
	) -> Self {
		Self {
			client,
			topic_prefix: topic_prefix.to_string(),
			camera_name: camera_name.to_string(),
			cache,
		}
	}

	/// Record the event in the republish cache so a broker that loses
	/// its retained store can be refilled on the next ConnAck.
	fn cache(&self, event: &CameraEvent) {
		match event {
			CameraEvent::Motion(detected) => self.cache.set_motion(*detected),
			CameraEvent::BatteryLevel(percent) => self.cache.set_battery_level(*percent),
			CameraEvent::Floodlight(on) => self.cache.set_floodlight(*on),
			CameraEvent::FloodlightTasks(enabled) => self.cache.set_floodlight_tasks(*enabled),
			CameraEvent::Pir(enabled) => self.cache.set_pir(*enabled),
			// Not cached — see `CameraEvent::is_cacheable`.
			CameraEvent::Connection(_) | CameraEvent::MotionUnknown | CameraEvent::Preview(_) => {}
		}
	}
}

#[async_trait]
impl StatusReporter for MqttStatusReporter {
	async fn report(&self, event: CameraEvent) -> Result<(), StatusError> {
		let publisher = StatusPublisher::new(&self.client, &self.topic_prefix, &self.camera_name);
		let result = match &event {
			CameraEvent::Connection(online) => publisher.publish_connection(*online).await,
			CameraEvent::Motion(detected) => publisher.publish_motion(*detected).await,
			CameraEvent::MotionUnknown => publisher.publish_motion_unknown().await,
			CameraEvent::BatteryLevel(percent) => publisher.publish_battery_level(*percent).await,
			CameraEvent::Floodlight(on) => publisher.publish_floodlight(*on).await,
			CameraEvent::FloodlightTasks(enabled) => {
				publisher.publish_floodlight_tasks_enabled(*enabled).await
			}
			CameraEvent::Pir(enabled) => publisher.publish_pir(*enabled).await,
			CameraEvent::Preview(jpeg) => publisher.publish_preview(jpeg).await,
		};
		result.map_err(|e| StatusError(e.to_string()))?;
		// Cache only what actually reached the broker, so a failed
		// publish can't leave the cache claiming a value HA never saw.
		self.cache(&event);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::Bytes;

	fn reporter(
		cache: Arc<StatusCache>,
	) -> (MqttStatusReporter, crate::mqtt::test_support::MockHandle) {
		let (client, mock) = crate::mqtt::test_support::mock_client();
		(
			MqttStatusReporter::new(client, "bairelay", "cam1", cache),
			mock,
		)
	}

	#[tokio::test]
	async fn each_event_lands_on_its_retained_topic() {
		let (reporter, mock) = reporter(Arc::new(StatusCache::default()));
		for event in [
			CameraEvent::Connection(true),
			CameraEvent::Motion(true),
			CameraEvent::MotionUnknown,
			CameraEvent::BatteryLevel(77),
			CameraEvent::Floodlight(true),
			CameraEvent::FloodlightTasks(false),
			CameraEvent::Pir(true),
			CameraEvent::Preview(Bytes::from_static(b"jpeg")),
		] {
			reporter.report(event).await.expect("mock publish succeeds");
		}

		let rows = mock.published();
		let find = |topic: &str| {
			rows.iter().find(|(t, _, _)| t == topic).unwrap_or_else(|| {
				panic!("no publish on {topic}; saw {:?}", mock.published_topics())
			})
		};
		assert_eq!(find("bairelay/cam1/status").1, b"connected");
		assert_eq!(find("bairelay/cam1/status/battery_level").1, b"77");
		assert_eq!(find("bairelay/cam1/status/pir").1, b"on");
		assert_eq!(
			find("bairelay/cam1/status/floodlight").1,
			br#"{"state":"on"}"#
		);
		assert_eq!(find("bairelay/cam1/status/floodlight_tasks").1, b"off");
		// Motion is published twice (unknown, then on); both retained.
		assert!(rows
			.iter()
			.any(|(t, p, r)| t == "bairelay/cam1/status/motion" && p == b"unknown" && *r));
		assert!(rows
			.iter()
			.any(|(t, p, r)| t == "bairelay/cam1/status/motion" && p == b"on" && *r));
		assert!(rows
			.iter()
			.any(|(t, _, r)| t == "bairelay/cam1/status/preview" && *r));
	}

	#[tokio::test]
	async fn publishing_fills_the_republish_cache_without_the_caller_asking() {
		let cache = Arc::new(StatusCache::default());
		let (reporter, _mock) = reporter(Arc::clone(&cache));

		reporter.report(CameraEvent::Motion(true)).await.unwrap();
		reporter
			.report(CameraEvent::BatteryLevel(42))
			.await
			.unwrap();
		reporter
			.report(CameraEvent::Floodlight(false))
			.await
			.unwrap();
		reporter
			.report(CameraEvent::FloodlightTasks(true))
			.await
			.unwrap();
		reporter.report(CameraEvent::Pir(false)).await.unwrap();

		assert_eq!(cache.motion(), Some(true));
		assert_eq!(cache.battery_level(), Some(42));
		assert_eq!(cache.floodlight(), Some(false));
		assert_eq!(cache.floodlight_tasks(), Some(true));
		assert_eq!(cache.pir(), Some(false));
	}

	#[tokio::test]
	async fn non_cacheable_events_leave_the_cache_empty() {
		let cache = Arc::new(StatusCache::default());
		let (reporter, _mock) = reporter(Arc::clone(&cache));

		reporter
			.report(CameraEvent::Connection(true))
			.await
			.unwrap();
		reporter.report(CameraEvent::MotionUnknown).await.unwrap();
		reporter
			.report(CameraEvent::Preview(Bytes::from_static(b"x")))
			.await
			.unwrap();

		assert_eq!(cache.motion(), None);
		assert_eq!(cache.battery_level(), None);
	}
}
