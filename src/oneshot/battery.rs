use anyhow::{Context, Result};

use super::output::Outcome;
use crate::camera::Camera;

pub async fn run(cam: &dyn Camera) -> Result<Outcome> {
	let status = cam.battery_status().await.context("battery query failed")?;
	// The port already clamps percent to 0–100 and types voltage as
	// millivolts; negative mV readings (seen from some firmwares
	// mid-boot) clamp at this display edge.
	Ok(Outcome::Battery {
		percent: u32::from(status.percent),
		voltage_mv: status.voltage.get().max(0) as u32,
		charge_status: status.charge_status,
		low_power: status.low_power,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc_protocol::Error;

	use crate::battery::{BatteryStatus, Millivolts};
	use crate::fake_camera::FakeCameraBuilder;

	#[tokio::test]
	async fn battery_happy_path_maps_fields() {
		let fake = FakeCameraBuilder::new()
			.with_battery_status(|| {
				Ok(BatteryStatus {
					percent: 87,
					voltage: Millivolts(3942),
					charge_status: "charging".into(),
					low_power: false,
				})
			})
			.build();
		let outcome = run(&*fake).await.unwrap();
		assert_eq!(
			outcome,
			Outcome::Battery {
				percent: 87,
				voltage_mv: 3942,
				charge_status: "charging".into(),
				low_power: false,
			}
		);
	}

	#[tokio::test]
	async fn battery_clamps_negative_voltage_at_display_edge() {
		let fake = FakeCameraBuilder::new()
			.with_battery_status(|| {
				Ok(BatteryStatus {
					percent: 100,
					voltage: Millivolts(-1),
					charge_status: "none".into(),
					low_power: true,
				})
			})
			.build();
		let outcome = run(&*fake).await.unwrap();
		let Outcome::Battery {
			voltage_mv,
			low_power,
			..
		} = outcome
		else {
			panic!("wrong variant");
		};
		assert_eq!(voltage_mv, 0);
		assert!(low_power);
	}

	#[tokio::test]
	async fn battery_driver_error_propagates_with_context() {
		let fake = FakeCameraBuilder::new()
			.with_battery_status(|| Err(Error::Other("simulated")))
			.build();
		let err = run(&*fake).await.unwrap_err();
		assert!(format!("{:#}", err).contains("battery query failed"));
	}
}
