use crate::camera::DeviceAdmin;
use anyhow::{Context, Result};

use crate::local_time;

use super::output::Outcome;

pub async fn run(cam: &dyn DeviceAdmin) -> Result<Outcome> {
	// `now_local()` carries the captured UTC offset. The Baichuan
	// `SystemGeneral` packet encodes both the wall-clock components
	// and `time_zone = -offset.whole_seconds()`, so the camera's OSD
	// renders the operator's local time rather than UTC.
	let now = local_time::now_local();
	cam.set_time(now).await.context("set_time failed")?;
	Ok(Outcome::SetTime {
		utc: now.to_string(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::fake_camera::FakeCameraBuilder;

	#[tokio::test]
	async fn set_time_records_call_and_returns_utc_string() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = run(&*fake).await.unwrap();
		let Outcome::SetTime { utc } = outcome else {
			panic!("wrong variant");
		};
		assert!(!utc.is_empty());
		let calls = fake.calls().set_time.lock().unwrap();
		assert_eq!(calls.len(), 1);
		// Can't assert exact time but sanity-check that the outcome
		// string roughly matches the recorded timestamp by year prefix.
		let year = calls[0].year();
		assert!(utc.starts_with(&year.to_string()));
	}
}
