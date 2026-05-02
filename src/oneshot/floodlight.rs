use std::time::Duration;

use anyhow::{Context, Result};
use neolink_core::bc_protocol::CameraDriver;

use super::output::Outcome;

// Matches the hold duration used by src/mqtt_dispatch.rs so the one-shot
// CLI behaves the same as an MQTT floodlight toggle.
const FLOODLIGHT_HOLD_SECS: u16 = 30;

// Bound the read path: `listen_on_floodlight` only returns events the
// camera pushes, so if nothing changes we'd wait forever. Argus cameras
// typically send an initial state on subscribe; if they don't, surface
// that as "state unknown" rather than hanging.
//
// 5 s (was 3 s) — a battery camera fresh from sleep takes longer than
// 3 s to push the first floodlight status, so the original timeout fired
// on roughly half of operator-triggered `bairelay floodlight <cam>`
// reads. The wider window keeps cold-start reads reliable; the case
// where the camera genuinely never pushes (broken firmware) still
// surfaces as the "state unknown" error, just two seconds later.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Toggle when `set` is `Some`, otherwise read the current state.
pub async fn run(cam: &dyn CameraDriver, set: Option<bool>) -> Result<Outcome> {
	if let Some(on) = set {
		cam.set_floodlight_manual(on, FLOODLIGHT_HOLD_SECS)
			.await
			.context("floodlight command failed")?;
	}
	let state = read_current_state(cam).await?;
	Ok(Outcome::Floodlight { state })
}

async fn read_current_state(cam: &dyn CameraDriver) -> Result<bool> {
	let mut rx = cam
		.listen_on_floodlight()
		.await
		.context("listen_on_floodlight failed")?;
	match tokio::time::timeout(READ_TIMEOUT, rx.recv()).await {
		Ok(Some(list)) => Ok(list
			.floodlight_status_list
			.first()
			.map(|f| f.status != 0)
			.unwrap_or(false)),
		Ok(None) => anyhow::bail!("camera closed the floodlight status channel before sending"),
		Err(_) => anyhow::bail!(
			"camera did not report a floodlight status within {:?} (subscribe-only stream; state may be idle)",
			READ_TIMEOUT
		),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use neolink_core::bc::xml::{FloodlightStatus, FloodlightStatusList};
	use neolink_core::bc_protocol::FakeCameraBuilder;
	use tokio::sync::mpsc;
	use tokio::time::Instant;

	fn single_status(on: bool) -> FloodlightStatusList {
		FloodlightStatusList {
			floodlight_status_list: vec![FloodlightStatus {
				channel_id: 0,
				status: if on { 1 } else { 0 },
			}],
			..Default::default()
		}
	}

	#[tokio::test]
	async fn floodlight_read_on() {
		let (tx, rx) = mpsc::channel(1);
		tx.send(single_status(true)).await.unwrap();
		let fake = FakeCameraBuilder::new().with_floodlight_stream(rx).build();
		let outcome = run(&*fake, None).await.unwrap();
		assert_eq!(outcome, Outcome::Floodlight { state: true });
	}

	#[tokio::test]
	async fn floodlight_read_off() {
		let (tx, rx) = mpsc::channel(1);
		tx.send(single_status(false)).await.unwrap();
		let fake = FakeCameraBuilder::new().with_floodlight_stream(rx).build();
		let outcome = run(&*fake, None).await.unwrap();
		assert_eq!(outcome, Outcome::Floodlight { state: false });
	}

	#[tokio::test]
	async fn floodlight_set_true_then_reads() {
		let (tx, rx) = mpsc::channel(1);
		tx.send(single_status(true)).await.unwrap();
		let fake = FakeCameraBuilder::new().with_floodlight_stream(rx).build();
		let outcome = run(&*fake, Some(true)).await.unwrap();
		assert_eq!(outcome, Outcome::Floodlight { state: true });
		assert_eq!(
			*fake.calls().set_floodlight_manual.lock().unwrap(),
			vec![(true, FLOODLIGHT_HOLD_SECS)]
		);
	}

	#[tokio::test]
	async fn floodlight_empty_list_returns_false() {
		let (tx, rx) = mpsc::channel(1);
		tx.send(FloodlightStatusList::default()).await.unwrap();
		let fake = FakeCameraBuilder::new().with_floodlight_stream(rx).build();
		let outcome = run(&*fake, None).await.unwrap();
		assert_eq!(outcome, Outcome::Floodlight { state: false });
	}

	#[tokio::test]
	async fn floodlight_channel_closed_errors() {
		let (tx, rx) = mpsc::channel::<FloodlightStatusList>(1);
		drop(tx);
		let fake = FakeCameraBuilder::new().with_floodlight_stream(rx).build();
		let err = run(&*fake, None).await.unwrap_err();
		assert!(format!("{:#}", err).contains("closed the floodlight status channel"));
	}

	#[tokio::test(start_paused = true)]
	async fn floodlight_timeout_surfaces_message() {
		// Leave the sender alive but never send — relies on paused clock
		// to fast-forward past READ_TIMEOUT without waiting real time.
		let (_tx, rx) = mpsc::channel::<FloodlightStatusList>(1);
		let fake = FakeCameraBuilder::new().with_floodlight_stream(rx).build();
		let start = Instant::now();
		let err = run(&*fake, None).await.unwrap_err();
		assert!(start.elapsed() >= READ_TIMEOUT);
		assert!(format!("{:#}", err).contains("did not report a floodlight status"));
	}
}
