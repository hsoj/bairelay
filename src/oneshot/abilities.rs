//! `abilities` one-shot — print the camera's `abilityInfo` XML reply
//! plus a flat parsed view. Read-only diagnostic; primary use is
//! capturing ground-truth ability strings for the `MissingAbility`
//! gate decisions that `populate_abilities` consumes (see
//! `docs/implementation.md` § XML parsing brittleness).

use anyhow::{Context, Result};
use neolink_core::bc::xml::{AbilityInfo, AbilityInfoToken};
use neolink_core::bc_protocol::CameraDriver;

use super::output::{AbilityEntry, Outcome};

pub async fn run(cam: &dyn CameraDriver) -> Result<Outcome> {
	let info = cam
		.get_abilityinfo()
		.await
		.context("camera abilityInfo query failed")?;
	let xml = serialise_xml(&info)?;
	let entries = flatten(&info);
	Ok(Outcome::Abilities {
		username: info.username,
		xml,
		entries,
	})
}

/// Round-trip the parsed `AbilityInfo` back through `quick_xml::se` so
/// the operator gets a stable on-disk form. The deserialiser tolerates
/// missing fields (`#[serde(default)]` everywhere); the serialiser
/// emits whatever we received, modulo field-order normalisation.
fn serialise_xml(info: &AbilityInfo) -> Result<String> {
	let mut buf = bytes::BytesMut::new();
	quick_xml::se::to_writer(&mut buf, info).context("re-serialise abilityInfo as XML")?;
	let s = std::str::from_utf8(&buf)
		.context("abilityInfo XML was not valid utf-8 — protocol corruption?")?;
	Ok(s.to_owned())
}

/// Walk the per-module tokens collecting every `(module, name, kind)`
/// triple. `kind` is `"rw"` / `"ro"` per the abilityValue suffix —
/// matches the gate `populate_abilities` builds against. Suffix-less
/// entries are skipped silently (same as `populate_abilities`).
fn flatten(info: &AbilityInfo) -> Vec<AbilityEntry> {
	const MODULES: &[&str] = &[
		"system",
		"network",
		"alarm",
		"image",
		"video",
		"security",
		"replay",
		"ptz",
		"io",
		"streaming",
	];
	let tokens: [Option<&AbilityInfoToken>; 10] = [
		info.system.as_ref(),
		info.network.as_ref(),
		info.alarm.as_ref(),
		info.image.as_ref(),
		info.video.as_ref(),
		info.security.as_ref(),
		info.replay.as_ref(),
		info.ptz.as_ref(),
		info.io.as_ref(),
		info.streaming.as_ref(),
	];
	let mut out = Vec::new();
	for (module, token) in MODULES.iter().zip(tokens) {
		let Some(token) = token else {
			continue;
		};
		for sub in &token.sub_module {
			for raw in sub.ability_value.replace(' ', "").split(',') {
				if raw.is_empty() {
					continue;
				}
				let Some((name, kind)) = raw.rsplit_once('_') else {
					continue;
				};
				if kind != "rw" && kind != "ro" {
					continue;
				}
				out.push(AbilityEntry {
					module: (*module).to_string(),
					name: name.to_string(),
					kind: kind.to_string(),
				});
			}
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use neolink_core::bc::xml::{AbilityInfoSubModule, AbilityInfoToken};
	use neolink_core::bc_protocol::{Error, FakeCameraBuilder};

	fn token(values: &[&str]) -> AbilityInfoToken {
		AbilityInfoToken {
			sub_module: vec![AbilityInfoSubModule {
				channel_id: Some(0),
				ability_value: values.join(", "),
			}],
		}
	}

	#[tokio::test]
	async fn run_collects_entries_and_xml_for_typical_response() {
		let fake = FakeCameraBuilder::new()
			.with_abilityinfo(|| {
				Ok(AbilityInfo {
					username: "admin".into(),
					system: Some(token(&["reboot_rw", "general_ro"])),
					network: Some(token(&["ping_rw"])),
					ptz: Some(token(&["two_way_audio_rw"])),
					..Default::default()
				})
			})
			.build();
		let out = run(&*fake).await.unwrap();
		let Outcome::Abilities {
			username,
			xml,
			entries,
		} = out
		else {
			panic!("wrong variant");
		};
		assert_eq!(username, "admin");
		assert!(xml.contains("admin"));
		assert!(xml.contains("reboot_rw"));
		assert_eq!(entries.len(), 4);
		assert!(entries
			.iter()
			.any(|e| e.module == "system" && e.name == "reboot" && e.kind == "rw"));
		assert!(entries
			.iter()
			.any(|e| e.module == "system" && e.name == "general" && e.kind == "ro"));
		assert!(entries
			.iter()
			.any(|e| e.module == "network" && e.name == "ping" && e.kind == "rw"));
		// Underscore-bearing ability names survive: rsplit_once keeps
		// `two_way_audio` as the name and `rw` as the kind.
		assert!(entries
			.iter()
			.any(|e| e.module == "ptz" && e.name == "two_way_audio" && e.kind == "rw"));
	}

	#[tokio::test]
	async fn run_skips_entries_with_unknown_or_missing_kind_suffix() {
		let fake = FakeCameraBuilder::new()
			.with_abilityinfo(|| {
				Ok(AbilityInfo {
					username: "x".into(),
					// `naked` has no `_rw`/`_ro` suffix; `weird_xx` has a
					// suffix but not one we recognise. Both must be
					// skipped, matching `populate_abilities`.
					system: Some(token(&["ok_rw", "naked", "weird_xx", ""])),
					..Default::default()
				})
			})
			.build();
		let out = run(&*fake).await.unwrap();
		let Outcome::Abilities { entries, .. } = out else {
			panic!("wrong variant");
		};
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0].name, "ok");
		assert_eq!(entries[0].kind, "rw");
	}

	#[tokio::test]
	async fn run_handles_camera_with_no_modules_populated() {
		let fake = FakeCameraBuilder::new()
			.with_abilityinfo(|| {
				Ok(AbilityInfo {
					username: "minimal".into(),
					..Default::default()
				})
			})
			.build();
		let out = run(&*fake).await.unwrap();
		let Outcome::Abilities {
			username, entries, ..
		} = out
		else {
			panic!("wrong variant");
		};
		assert_eq!(username, "minimal");
		assert!(entries.is_empty());
	}

	#[tokio::test]
	async fn run_propagates_camera_error_with_context() {
		let fake = FakeCameraBuilder::new()
			.with_abilityinfo(|| Err(Error::Other("nope")))
			.build();
		let err = run(&*fake).await.unwrap_err();
		assert!(format!("{:#}", err).contains("camera abilityInfo query failed"));
	}
}
