use anyhow::{Context, Result};
use bairelay_neolink_core::bc_protocol::CameraDriver;

use super::output::Outcome;

pub async fn run(cam: &dyn CameraDriver) -> Result<Outcome> {
	cam.siren().await.context("siren trigger failed")?;
	Ok(Outcome::Siren)
}

#[cfg(test)]
mod tests {
	use super::*;
	use bairelay_neolink_core::bc_protocol::FakeCameraBuilder;

	#[tokio::test]
	async fn siren_returns_siren_variant_and_logs_call() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = run(&*fake).await.unwrap();
		assert_eq!(outcome, Outcome::Siren);
		assert_eq!(*fake.calls().siren.lock().unwrap(), 1);
	}
}
