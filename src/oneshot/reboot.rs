use crate::camera::DeviceAdmin;
use anyhow::{Context, Result};

use super::output::Outcome;

pub async fn run(cam: &dyn DeviceAdmin) -> Result<Outcome> {
	cam.reboot().await.context("reboot command failed")?;
	Ok(Outcome::Reboot)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::fake_camera::FakeCameraBuilder;

	#[tokio::test]
	async fn reboot_returns_reboot_variant_and_logs_call() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = run(&*fake).await.unwrap();
		assert_eq!(outcome, Outcome::Reboot);
		assert_eq!(*fake.calls().reboot.lock().unwrap(), 1);
	}

	#[tokio::test]
	async fn reboot_error_propagates_with_context() {
		use crate::baichuan::bc_protocol::Error;
		use crate::fake_camera::FakeDeviceAdmin;
		let fake = FakeDeviceAdmin::new()
			.with_reboot_error(|| Error::Other("reboot refused"))
			.build();
		let err = run(&*fake).await.unwrap_err();
		assert!(format!("{:#}", err).contains("reboot command failed"));
		assert_eq!(*fake.calls().reboot.lock().unwrap(), 1);
	}
}
