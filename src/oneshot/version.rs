use crate::camera::DeviceAdmin;
use anyhow::{Context, Result};

use super::output::Outcome;

pub async fn run(cam: &dyn DeviceAdmin) -> Result<Outcome> {
	let info = cam.version().await.context("camera version query failed")?;
	Ok(Outcome::Version {
		model: info.model.unwrap_or_else(|| "unknown".into()),
		firmware: info.firmwareVersion,
		hardware: info.hardwareVersion,
		build_day: info.buildDay,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc::xml::VersionInfo;
	use crate::baichuan::bc_protocol::Error;

	use crate::fake_camera::FakeCameraBuilder;

	#[tokio::test]
	async fn version_maps_all_fields() {
		let fake = FakeCameraBuilder::new()
			.with_version(|| {
				Ok(VersionInfo {
					model: Some("Argus 3 Pro".into()),
					firmwareVersion: "v3.1.0.1234_23112233".into(),
					hardwareVersion: "IPC_523SD8MP".into(),
					buildDay: "build 23112233".into(),
					..Default::default()
				})
			})
			.build();
		let outcome = run(&*fake).await.unwrap();
		assert_eq!(
			outcome,
			Outcome::Version {
				model: "Argus 3 Pro".into(),
				firmware: "v3.1.0.1234_23112233".into(),
				hardware: "IPC_523SD8MP".into(),
				build_day: "build 23112233".into(),
			}
		);
	}

	#[tokio::test]
	async fn version_missing_model_becomes_unknown() {
		let fake = FakeCameraBuilder::new()
			.with_version(|| {
				Ok(VersionInfo {
					model: None,
					firmwareVersion: "fw".into(),
					hardwareVersion: "hw".into(),
					buildDay: "bd".into(),
					..Default::default()
				})
			})
			.build();
		let outcome = run(&*fake).await.unwrap();
		let Outcome::Version { model, .. } = outcome else {
			panic!("wrong variant");
		};
		assert_eq!(model, "unknown");
	}

	#[tokio::test]
	async fn version_error_propagates() {
		let fake = FakeCameraBuilder::new()
			.with_version(|| Err(Error::Other("no")))
			.build();
		let err = run(&*fake).await.unwrap_err();
		assert!(format!("{:#}", err).contains("camera version query failed"));
	}
}
