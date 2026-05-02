use anyhow::{Context, Result};
use neolink_core::bc_protocol::CameraDriver;

use super::output::Outcome;

pub async fn run(cam: &dyn CameraDriver) -> Result<Outcome> {
	let info = cam.battery_info().await.context("battery_info failed")?;
	// Voltage is reported in millivolts (confirmed against captured
	// samples). Clamp percent to 100 — some Argus firmwares briefly
	// report 101 on warm boot. We surface mV as the integer the wire
	// uses, not as a converted float — see Outcome::Battery doc.
	Ok(Outcome::Battery {
		percent: info.battery_percent.min(100),
		voltage_mv: info.voltage.max(0) as u32,
		charge_status: info.charge_status,
		low_power: info.low_power != 0,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use neolink_core::bc::xml::BatteryInfo;
	use neolink_core::bc_protocol::{Error, FakeCameraBuilder};

	#[tokio::test]
	async fn battery_happy_path_maps_fields() {
		let fake = FakeCameraBuilder::new()
			.with_battery_info(|| {
				Ok(BatteryInfo {
					battery_percent: 87,
					voltage: 3942,
					charge_status: "charging".into(),
					low_power: 0,
					..Default::default()
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
	async fn battery_clamps_percent_to_100() {
		let fake = FakeCameraBuilder::new()
			.with_battery_info(|| {
				Ok(BatteryInfo {
					battery_percent: 101,
					voltage: 4200,
					charge_status: "none".into(),
					low_power: 1,
					..Default::default()
				})
			})
			.build();
		let outcome = run(&*fake).await.unwrap();
		let Outcome::Battery {
			percent, low_power, ..
		} = outcome
		else {
			panic!("wrong variant");
		};
		assert_eq!(percent, 100);
		assert!(low_power);
	}

	#[tokio::test]
	async fn battery_driver_error_propagates_with_context() {
		let fake = FakeCameraBuilder::new()
			.with_battery_info(|| Err(Error::Other("simulated")))
			.build();
		let err = run(&*fake).await.unwrap_err();
		assert!(format!("{:#}", err).contains("battery_info failed"));
	}
}
