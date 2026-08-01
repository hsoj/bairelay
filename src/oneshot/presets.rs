use anyhow::{Context, Result};

use super::output::{Outcome, Preset};
use crate::camera::Camera;

pub async fn run(cam: &dyn Camera) -> Result<Outcome> {
	let slots = cam.ptz_presets().await.context("ptz_presets failed")?;
	let presets = slots
		.into_iter()
		.map(|p| Preset {
			id: p.id,
			name: p.name,
		})
		.collect();
	Ok(Outcome::Presets { presets })
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc_protocol::Error;

	use crate::fake_camera::FakeCameraBuilder;
	use crate::ptz::PresetSlot;

	#[tokio::test]
	async fn presets_maps_slot_list_to_outcome() {
		let fake = FakeCameraBuilder::new()
			.with_ptz_presets(|| {
				Ok(vec![
					PresetSlot {
						id: 0,
						name: Some("home".into()),
					},
					PresetSlot { id: 1, name: None },
				])
			})
			.build();
		let outcome = run(&*fake).await.unwrap();
		let Outcome::Presets { presets } = outcome else {
			panic!("wrong variant");
		};
		assert_eq!(presets.len(), 2);
		assert_eq!(presets[0].id, 0);
		assert_eq!(presets[0].name.as_deref(), Some("home"));
		assert_eq!(presets[1].id, 1);
		assert!(presets[1].name.is_none());
	}

	#[tokio::test]
	async fn presets_empty_list() {
		let fake = FakeCameraBuilder::new()
			.with_ptz_presets(|| Ok(Vec::new()))
			.build();
		let outcome = run(&*fake).await.unwrap();
		assert_eq!(outcome, Outcome::Presets { presets: vec![] });
	}

	#[tokio::test]
	async fn presets_error_propagates() {
		let fake = FakeCameraBuilder::new()
			.with_ptz_presets(|| Err(Error::Other("fail")))
			.build();
		let err = run(&*fake).await.unwrap_err();
		assert!(format!("{:#}", err).contains("ptz_presets failed"));
	}
}
