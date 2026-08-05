use crate::camera::Lighting;
use anyhow::{Context, Result};

use super::output::Outcome;

pub async fn run(cam: &dyn Lighting) -> Result<Outcome> {
	cam.siren().await.context("siren trigger failed")?;
	Ok(Outcome::Siren)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::fake_camera::FakeCameraBuilder;

	#[tokio::test]
	async fn siren_returns_siren_variant_and_logs_call() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = run(&*fake).await.unwrap();
		assert_eq!(outcome, Outcome::Siren);
		assert_eq!(*fake.calls().siren.lock().unwrap(), 1);
	}
}
