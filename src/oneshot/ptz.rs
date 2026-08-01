use crate::baichuan::bc_protocol::Direction;
use anyhow::{Context, Result};

use crate::camera::Camera;

use super::output::{Outcome, Preset};

/// Move to a preset or, if `preset_id` is `None`, list the camera's presets.
pub async fn preset(cam: &dyn Camera, preset_id: Option<u8>) -> Result<Outcome> {
	match preset_id {
		Some(id) => {
			cam.moveto_ptz_preset(id)
				.await
				.context("moveto_ptz_preset failed")?;
			Ok(Outcome::PtzMoveTo { preset_id: id })
		}
		None => {
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
	}
}

pub async fn assign(cam: &dyn Camera, preset_id: u8, name: String) -> Result<Outcome> {
	cam.set_ptz_preset(preset_id, name.clone())
		.await
		.context("set_ptz_preset failed")?;
	Ok(Outcome::PtzAssign { preset_id, name })
}

pub async fn control(
	cam: &dyn Camera,
	direction: Direction,
	amount: u32,
	speed: Option<u32>,
) -> Result<Outcome> {
	// `BcCamera::send_ptz` takes an f32 "amount"; speed in neolink's CLI
	// is an optional hint that scales the motion. We use `amount` as the
	// primary argument and leave `speed` informational on the wire —
	// matches what neolink does.
	cam.send_ptz(direction, amount as f32)
		.await
		.context("send_ptz failed")?;
	Ok(Outcome::PtzControl {
		direction: direction_label(direction).into(),
		amount,
		speed,
	})
}

pub async fn zoom(cam: &dyn Camera, amount: f32) -> Result<Outcome> {
	// The CLI accepts the user-facing zoom factor to match neolink's
	// interface; `ZoomLevel`'s constructor owns the wire scaling.
	cam.zoom_to(crate::ptz::ZoomLevel::from_factor(amount))
		.await
		.context("zoom_to failed")?;
	Ok(Outcome::PtzZoom { amount })
}

fn direction_label(dir: Direction) -> &'static str {
	match dir {
		Direction::Up => "up",
		Direction::Down => "down",
		Direction::Left => "left",
		Direction::Right => "right",
		Direction::Stop => "stop",
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::fake_camera::FakeCameraBuilder;
	use crate::ptz::{PresetSlot, ZoomLevel};

	#[tokio::test]
	async fn preset_move_to_logs_call_and_returns_moveto() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = preset(&*fake, Some(3)).await.unwrap();
		assert_eq!(outcome, Outcome::PtzMoveTo { preset_id: 3 });
		assert_eq!(*fake.calls().moveto_ptz_preset.lock().unwrap(), vec![3]);
	}

	#[tokio::test]
	async fn preset_list_returns_presets() {
		let fake = FakeCameraBuilder::new()
			.with_ptz_presets(|| {
				Ok(vec![PresetSlot {
					id: 5,
					name: Some("deck".into()),
				}])
			})
			.build();
		let outcome = preset(&*fake, None).await.unwrap();
		let Outcome::Presets { presets } = outcome else {
			panic!("wrong variant");
		};
		assert_eq!(presets.len(), 1);
		assert_eq!(presets[0].id, 5);
		assert_eq!(presets[0].name.as_deref(), Some("deck"));
	}

	#[tokio::test]
	async fn assign_stores_preset_and_records_call() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = assign(&*fake, 7, "porch".into()).await.unwrap();
		assert_eq!(
			outcome,
			Outcome::PtzAssign {
				preset_id: 7,
				name: "porch".into()
			}
		);
		assert_eq!(
			*fake.calls().set_ptz_preset.lock().unwrap(),
			vec![(7u8, "porch".to_string())]
		);
	}

	#[tokio::test]
	async fn control_formats_direction_label() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = control(&*fake, Direction::Up, 2, Some(5)).await.unwrap();
		assert_eq!(
			outcome,
			Outcome::PtzControl {
				direction: "up".into(),
				amount: 2,
				speed: Some(5),
			}
		);
		let calls = fake.calls().send_ptz.lock().unwrap();
		assert_eq!(calls.len(), 1);
		assert_eq!(calls[0].0, Direction::Up);
	}

	#[tokio::test]
	async fn control_all_directions_label() {
		let fake = FakeCameraBuilder::new().build();
		for (dir, label) in [
			(Direction::Up, "up"),
			(Direction::Down, "down"),
			(Direction::Left, "left"),
			(Direction::Right, "right"),
			(Direction::Stop, "stop"),
		] {
			let outcome = control(&*fake, dir, 1, None).await.unwrap();
			let Outcome::PtzControl { direction, .. } = outcome else {
				panic!("wrong variant");
			};
			assert_eq!(direction, label);
		}
	}

	#[tokio::test]
	async fn zoom_scales_amount_through_zoom_level() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = zoom(&*fake, 1.5).await.unwrap();
		assert_eq!(outcome, Outcome::PtzZoom { amount: 1.5 });
		assert_eq!(
			*fake.calls().zoom_to.lock().unwrap(),
			vec![ZoomLevel::from_factor(1.5)]
		);
		assert_eq!(ZoomLevel::from_factor(1.5).camera_units(), 1500);
	}

	#[tokio::test]
	async fn zoom_negative_clamps_to_zero() {
		let fake = FakeCameraBuilder::new().build();
		let _ = zoom(&*fake, -4.0).await.unwrap();
		assert_eq!(
			*fake.calls().zoom_to.lock().unwrap(),
			vec![ZoomLevel::from_factor(0.0)]
		);
	}
}
