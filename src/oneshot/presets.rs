use anyhow::{Context, Result};
use neolink_core::bc_protocol::CameraDriver;

use super::output::{Outcome, Preset};

pub async fn run(cam: &dyn CameraDriver) -> Result<Outcome> {
	let ptz = cam
		.get_ptz_preset()
		.await
		.context("get_ptz_preset failed")?;
	let presets = ptz
		.preset_list
		.preset
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
	use neolink_core::bc::xml::{Preset as XmlPreset, PresetList, PtzPreset};
	use neolink_core::bc_protocol::{Error, FakeCameraBuilder};

	#[tokio::test]
	async fn presets_maps_xml_list_to_outcome() {
		let fake = FakeCameraBuilder::new()
			.with_ptz_preset(|| {
				Ok(PtzPreset {
					preset_list: PresetList {
						preset: vec![
							XmlPreset {
								id: 0,
								name: Some("home".into()),
								..Default::default()
							},
							XmlPreset {
								id: 1,
								name: None,
								..Default::default()
							},
						],
					},
					..Default::default()
				})
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
			.with_ptz_preset(|| Ok(PtzPreset::default()))
			.build();
		let outcome = run(&*fake).await.unwrap();
		assert_eq!(outcome, Outcome::Presets { presets: vec![] });
	}

	#[tokio::test]
	async fn presets_error_propagates() {
		let fake = FakeCameraBuilder::new()
			.with_ptz_preset(|| Err(Error::Other("fail")))
			.build();
		let err = run(&*fake).await.unwrap_err();
		assert!(format!("{:#}", err).contains("get_ptz_preset failed"));
	}
}
