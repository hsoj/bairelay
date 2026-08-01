use crate::camera::Camera;
use anyhow::{Context, Result};

use super::output::Outcome;

pub async fn run(cam: &dyn Camera, set: Option<bool>) -> Result<Outcome> {
	if let Some(on) = set {
		cam.pir_set(on).await.context("pir_set failed")?;
	}
	// Always read back so both paths return the current state.
	let cfg = cam.pir_config().await.context("get_pirstate failed")?;
	Ok(Outcome::Pir {
		enable: cfg.enable != 0,
		sensitivity: cfg.sensiValue.or(cfg.sensitivity),
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc::xml::RfAlarmCfg;
	use crate::baichuan::bc_protocol::Error;

	use crate::fake_camera::FakeCameraBuilder;

	fn cfg_with(enable: u8, sensi: Option<u8>, old: Option<u8>) -> RfAlarmCfg {
		RfAlarmCfg {
			enable,
			sensiValue: sensi,
			sensitivity: old,
			..Default::default()
		}
	}

	#[tokio::test]
	async fn pir_read_only_reports_current_state() {
		let fake = FakeCameraBuilder::new()
			.with_pir_config(|| Ok(cfg_with(1, Some(50), None)))
			.build();
		let outcome = run(&*fake, None).await.unwrap();
		assert_eq!(
			outcome,
			Outcome::Pir {
				enable: true,
				sensitivity: Some(50),
			}
		);
		assert!(fake.calls().pir_set.lock().unwrap().is_empty());
	}

	#[tokio::test]
	async fn pir_set_true_then_reads_back() {
		let fake = FakeCameraBuilder::new()
			.with_pir_config(|| Ok(cfg_with(1, None, Some(80))))
			.build();
		let outcome = run(&*fake, Some(true)).await.unwrap();
		assert_eq!(
			outcome,
			Outcome::Pir {
				enable: true,
				sensitivity: Some(80),
			}
		);
		assert_eq!(*fake.calls().pir_set.lock().unwrap(), vec![true]);
	}

	#[tokio::test]
	async fn pir_set_false_recorded() {
		let fake = FakeCameraBuilder::new()
			.with_pir_config(|| Ok(cfg_with(0, None, None)))
			.build();
		let outcome = run(&*fake, Some(false)).await.unwrap();
		assert_eq!(
			outcome,
			Outcome::Pir {
				enable: false,
				sensitivity: None,
			}
		);
		assert_eq!(*fake.calls().pir_set.lock().unwrap(), vec![false]);
	}

	#[tokio::test]
	async fn pir_read_error_propagates_with_context() {
		let fake = FakeCameraBuilder::new()
			.with_pir_config(|| Err(Error::Other("nope")))
			.build();
		let err = run(&*fake, None).await.unwrap_err();
		assert!(format!("{:#}", err).contains("get_pirstate failed"));
	}
}
