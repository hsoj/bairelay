use crate::camera::Camera;
use anyhow::{Context, Result};

use super::output::Outcome;

pub async fn run(cam: &dyn Camera, set: Option<bool>) -> Result<Outcome> {
	if let Some(on) = set {
		cam.led_light_set(on)
			.await
			.context("led_light_set failed")?;
	}
	let led = cam.led_state().await.context("get_ledstate failed")?;
	// light_state values from the camera are "open" / "close".
	let state = matches!(led.light_state.as_str(), "open");
	Ok(Outcome::StatusLight { state })
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc::xml::LedState;
	use crate::baichuan::bc_protocol::Error;

	use crate::fake_camera::FakeCameraBuilder;

	fn led_with(light_state: &str) -> LedState {
		LedState {
			light_state: light_state.into(),
			..Default::default()
		}
	}

	#[tokio::test]
	async fn status_light_read_open_is_on() {
		let fake = FakeCameraBuilder::new()
			.with_led_state(|| Ok(led_with("open")))
			.build();
		let outcome = run(&*fake, None).await.unwrap();
		assert_eq!(outcome, Outcome::StatusLight { state: true });
	}

	#[tokio::test]
	async fn status_light_read_close_is_off() {
		let fake = FakeCameraBuilder::new()
			.with_led_state(|| Ok(led_with("close")))
			.build();
		let outcome = run(&*fake, None).await.unwrap();
		assert_eq!(outcome, Outcome::StatusLight { state: false });
	}

	#[tokio::test]
	async fn status_light_set_true_then_reads_back() {
		let fake = FakeCameraBuilder::new()
			.with_led_state(|| Ok(led_with("open")))
			.build();
		let outcome = run(&*fake, Some(true)).await.unwrap();
		assert_eq!(outcome, Outcome::StatusLight { state: true });
		assert_eq!(*fake.calls().led_light_set.lock().unwrap(), vec![true]);
	}

	#[tokio::test]
	async fn status_light_read_error_propagates() {
		let fake = FakeCameraBuilder::new()
			.with_led_state(|| Err(Error::Other("boom")))
			.build();
		let err = run(&*fake, None).await.unwrap_err();
		assert!(format!("{:#}", err).contains("get_ledstate failed"));
	}
}
