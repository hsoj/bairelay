use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};
use std::convert::{TryFrom, TryInto};
use time::{
	macros::date, parsing::Parsed, Date, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset,
};

#[cfg(test)]
use crate::bc_protocol::connection::mock::{
	reply_200_empty, reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	///
	/// Get the time from the camera
	///
	/// # Returns
	///
	/// returns either an error or an option with the offsetted date time
	///
	pub async fn get_time(&self) -> Result<Option<OffsetDateTime>> {
		self.has_ability_ro("general").await?;
		let general = self.get_system_general().await?;

		let (
			Some(time_zone),
			Some(year),
			Some(month),
			Some(day),
			Some(hour),
			Some(minute),
			Some(second),
		) = (
			general.time_zone,
			general.year,
			general.month,
			general.day,
			general.hour,
			general.minute,
			general.second,
		)
		else {
			return Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(BcXml {
					system_general: Some(general),
					..Default::default()
				}),
				why: "SystemGeneral reply missing one or more time fields",
			});
		};

		let datetime = try_build_timestamp(time_zone, year, month, day, hour, minute, second)
			.map_err(|_| Error::UnintelligibleXml {
				reply: std::sync::Arc::new(BcXml {
					system_general: Some(general),
					..Default::default()
				}),
				why: "Could not parse date",
			})?;

		// Pre-2019 dates indicate the camera's clock was never set
		// (factory firmware boots to e.g. 1999-01-01 or 2000-01-01).
		// Treat as `Ok(None)` so callers can re-set rather than treat
		// an obvious factory date as a real reading.
		const BOUNDARY: Date = date!(2019 - 01 - 01);
		if datetime.date() < BOUNDARY {
			Ok(None)
		} else {
			Ok(Some(datetime))
		}
	}

	/// Read the full `SystemGeneral` block from the camera. Used
	/// internally by `get_time` (for the time fields) and by
	/// `set_time` (for the read-modify-write that preserves every
	/// non-time field on the wire — see the comment in `set_time`).
	async fn get_system_general(&self) -> Result<SystemGeneral> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get_general = connection.subscribe(MSG_ID_GET_GENERAL, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_GET_GENERAL,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg::default()),
		};

		sub_get_general.send(get).await?;
		let msg = sub_get_general.recv().await?;
		if msg.meta.response_code != 200 {
			return Err(Error::CameraServiceUnavailable {
				id: msg.meta.msg_id,
				code: msg.meta.response_code,
			});
		}

		if let BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(BcXml {
				system_general: Some(general),
				..
			})),
			..
		}) = msg.body
		{
			Ok(general)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Reply did not contain a SystemGeneral block",
			})
		}
	}

	///
	/// Sets the time of the camera
	///
	/// # Parameters
	///
	/// * `timestamp` - The time to set the camera to
	///
	/// # Returns
	///
	/// returns Ok(()) or error
	///
	pub async fn set_time(&self, timestamp: OffsetDateTime) -> Result<()> {
		self.has_ability_rw("general").await?;

		// Read-modify-write: we GET the camera's current SystemGeneral,
		// mutate ONLY the time fields, and SET the full struct back.
		// Writing a freshly-defaulted SystemGeneral (the previous
		// behaviour) silently dropped every field we don't model
		// (osdFormat, language, deviceName) plus any firmware-internal
		// fields not surfaced in the struct, leaving the camera in a
		// half-set state that produced subtle drift on subsequent
		// firmware-driven time syncs. Round-tripping the rest of the
		// struct keeps every other knob bytewise unchanged.
		let mut general = self.get_system_general().await?;
		general.version = xml_ver();
		// Reolink uses positive seconds to indicate a negative UTC offset:
		general.time_zone = Some(-timestamp.offset().whole_seconds());
		general.year = Some(timestamp.year());
		general.month = Some(timestamp.month().into());
		general.day = Some(timestamp.day());
		general.hour = Some(timestamp.hour());
		general.minute = Some(timestamp.minute());
		general.second = Some(timestamp.second());

		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_set_general = connection.subscribe(MSG_ID_SET_GENERAL, msg_num).await?;
		let set = Bc::new_from_xml(
			BcMeta {
				msg_id: MSG_ID_SET_GENERAL,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			BcXml {
				system_general: Some(general),
				..Default::default()
			},
		);

		sub_set_general.send(set).await?;
		let msg = sub_set_general.recv().await?;
		if let BcMeta {
			response_code: 200, ..
		} = msg.meta
		{
		} else {
			return Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "The camera did not accept the set time command.",
			});
		}

		Ok(())
	}
}

/// Decode a Reolink `SystemGeneral` payload into an `OffsetDateTime`.
///
/// The wire `timezone` field uses Reolink's inverted convention
/// (positive seconds = west of UTC, negative = east) as documented in
/// `set_time` above. We negate here so the round-trip
/// `set_time(t)` → `get_time()` preserves the original
/// `OffsetDateTime`. Without the negation the two functions disagree
/// by `2 × offset` whenever the operator runs anything other than UTC.
///
/// `get_time` is currently only called by the Neolink CLI; bairelay's
/// CLI has no `get-time` subcommand, so the asymmetry was invisible in
/// production. Closing it here keeps the contract clean for any future
/// caller (TZ-aware MQTT publish, etc.) and makes the unit-level
/// symmetry test below load-bearing.
fn try_build_timestamp(
	timezone: i32,
	year: i32,
	month: u8,
	day: u8,
	hour: u8,
	minute: u8,
	second: u8,
) -> std::result::Result<OffsetDateTime, crate::Error> {
	let date = Date::try_from(
		Parsed::new()
			.with_year(year)
			.ok_or(Error::TimeParse)?
			.with_month(month.try_into()?)
			.ok_or(Error::TimeParse)?
			.with_day(day.try_into()?)
			.ok_or(Error::TimeParse)?,
	)?;
	let time = Time::try_from(
		Parsed::new()
			.with_hour_24(hour)
			.ok_or(Error::TimeParse)?
			.with_minute(minute)
			.ok_or(Error::TimeParse)?
			.with_second(second)
			.ok_or(Error::TimeParse)?,
	)?;
	// Negate to match `set_time`'s sign convention (Reolink wire:
	// positive = west of UTC, negative = east).
	let offset = UtcOffset::from_whole_seconds(-timezone)?;

	Ok(PrimitiveDateTime::new(date, time).assume_offset(offset))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_time_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_GENERAL)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						system_general: Some(SystemGeneral {
							version: "1.1".to_string(),
							time_zone: Some(0),
							year: Some(2026),
							month: Some(4),
							day: Some(23),
							hour: Some(12),
							minute: Some(30),
							second: Some(45),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("general", false).await;
		let dt = cam
			.get_time()
			.await
			.expect("get_time should succeed")
			.expect("datetime should be Some");
		assert_eq!(dt.year(), 2026);
		assert_eq!(dt.hour(), 12);
	}

	#[tokio::test]
	async fn get_time_missing_ability_returns_err() {
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_time().await.expect_err("should fail");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	#[tokio::test]
	async fn get_time_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_GENERAL)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("general", false).await;
		let err = cam.get_time().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_time_pre_boundary_returns_ok_none() {
		// Any date before 2019-01-01 → treated as "time not set".
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_GENERAL)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						system_general: Some(SystemGeneral {
							version: "1.1".to_string(),
							time_zone: Some(0),
							year: Some(1999),
							month: Some(1),
							day: Some(1),
							hour: Some(0),
							minute: Some(0),
							second: Some(0),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("general", false).await;
		let dt = cam.get_time().await.expect("ok");
		assert!(dt.is_none());
	}

	#[tokio::test]
	async fn get_time_invalid_date_returns_err() {
		// Month 13 is invalid → try_build_timestamp fails →
		// UnintelligibleReply.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_GENERAL)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						system_general: Some(SystemGeneral {
							version: "1.1".to_string(),
							time_zone: Some(0),
							year: Some(2026),
							month: Some(13),
							day: Some(1),
							hour: Some(0),
							minute: Some(0),
							second: Some(0),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("general", false).await;
		let err = cam.get_time().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleXml { .. }));
	}

	#[tokio::test]
	async fn get_time_missing_fields_returns_err() {
		// 200 with SystemGeneral but year=None → pattern doesn't match
		// → UnintelligibleReply.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_GENERAL)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						system_general: Some(SystemGeneral {
							version: "1.1".to_string(),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("general", false).await;
		let err = cam.get_time().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleXml { .. }));
	}

	#[tokio::test]
	async fn set_time_happy_path() {
		// `set_time` is read-modify-write: GET the camera's current
		// SystemGeneral, mutate ONLY the time fields, SET the full
		// struct back. The GET stub seeds non-time fields (osdFormat,
		// language, deviceName, timeFormat) that the SET assertion
		// then verifies are preserved bytewise — that's the
		// regression guard for "freshly-defaulted SystemGeneral
		// silently dropping camera state on every set-time".
		// Pin the inverted timezone convention here too: UTC+2
		// (+7200 UtcOffset seconds) serialises as -7200 on the wire.
		let dt = PrimitiveDateTime::new(
			Date::from_calendar_date(2026, time::Month::April, 23).unwrap(),
			Time::from_hms(12, 30, 45).unwrap(),
		)
		.assume_offset(UtcOffset::from_whole_seconds(7200).unwrap());

		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_GENERAL)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						system_general: Some(SystemGeneral {
							version: "1.1".to_string(),
							time_zone: Some(0),
							year: Some(2024),
							month: Some(1),
							day: Some(1),
							hour: Some(0),
							minute: Some(0),
							second: Some(0),
							osd_format: Some("DMY".to_string()),
							time_format: Some(1),
							language: Some("English".to_string()),
							device_name: Some("Front gate".to_string()),
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_SET_GENERAL)
			.reply_with_xml(|req, xml| {
				let g = xml.system_general.as_ref().expect("system_general on SET");
				// Mutated time fields:
				assert_eq!(g.year, Some(2026));
				assert_eq!(g.month, Some(4));
				assert_eq!(g.day, Some(23));
				assert_eq!(g.hour, Some(12));
				assert_eq!(g.minute, Some(30));
				assert_eq!(g.second, Some(45));
				// UtcOffset +7200 (UTC+2) → wire -7200 per Reolink.
				assert_eq!(g.time_zone, Some(-7200));
				// Preserved non-time fields (the regression guard):
				assert_eq!(g.osd_format.as_deref(), Some("DMY"));
				assert_eq!(g.time_format, Some(1));
				assert_eq!(g.language.as_deref(), Some("English"));
				assert_eq!(g.device_name.as_deref(), Some("Front gate"));
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("general", true).await;
		cam.set_time(dt).await.expect("ok");
	}

	#[tokio::test]
	async fn set_time_get_failure_propagates() {
		// If the GET roundtrip fails, set_time must surface the error
		// without sending a partial SET — otherwise we'd be back to
		// the bug this refactor fixed.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_GENERAL)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("general", true).await;
		let err = cam
			.set_time(OffsetDateTime::now_utc())
			.await
			.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn set_time_missing_ability_returns_err() {
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.set_time(OffsetDateTime::now_utc())
			.await
			.expect_err("should fail");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	#[tokio::test]
	async fn set_time_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_GENERAL)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						system_general: Some(SystemGeneral {
							version: "1.1".to_string(),
							time_zone: Some(0),
							year: Some(2026),
							month: Some(1),
							day: Some(1),
							hour: Some(0),
							minute: Some(0),
							second: Some(0),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_SET_GENERAL)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("general", true).await;
		let err = cam
			.set_time(OffsetDateTime::now_utc())
			.await
			.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[test]
	fn try_build_timestamp_valid_ok_inverts_timezone_sign() {
		// Wire timezone uses Reolink's inverted convention: positive =
		// west of UTC, negative = east. Wire `+3600` therefore decodes
		// to `UtcOffset` of `-3600` (UTC-1, "one hour west").
		let dt = try_build_timestamp(3600, 2026, 4, 23, 12, 30, 45).expect("ok");
		assert_eq!(dt.year(), 2026);
		assert_eq!(dt.offset().whole_seconds(), -3600);
	}

	#[test]
	fn try_build_timestamp_bad_month_is_err() {
		assert!(try_build_timestamp(0, 2026, 13, 1, 0, 0, 0).is_err());
	}

	#[test]
	fn try_build_timestamp_bad_day_is_err() {
		assert!(try_build_timestamp(0, 2026, 2, 31, 0, 0, 0).is_err());
	}

	#[test]
	fn try_build_timestamp_bad_hour_is_err() {
		assert!(try_build_timestamp(0, 2026, 4, 23, 25, 0, 0).is_err());
	}

	/// Roundtrip pin: `set_time` sends `time_zone =
	/// -offset.whole_seconds()`; `try_build_timestamp` must decode that
	/// wire value back into the same `OffsetDateTime` the operator
	/// passed in. Tests every offset that bairelay's actual deployments
	/// span (UTC-12 to UTC+14 covers the full real-world range).
	#[test]
	fn set_time_get_time_roundtrip_preserves_offset_datetime() {
		// Pick a deterministic wallclock so we don't depend on
		// `OffsetDateTime::now_*` flakiness.
		let local_date = Date::from_calendar_date(2026, time::Month::April, 23).unwrap();
		let local_time = Time::from_hms(12, 30, 45).unwrap();
		for offset_seconds in [-12 * 3600, -5 * 3600, -3600, 0, 3600, 5 * 3600, 14 * 3600] {
			let original_offset = UtcOffset::from_whole_seconds(offset_seconds).unwrap();
			let original =
				PrimitiveDateTime::new(local_date, local_time).assume_offset(original_offset);

			// `set_time` would put this on the wire:
			let wire_time_zone = -original.offset().whole_seconds();

			// `get_time` (via `try_build_timestamp`) would decode the
			// wire value back to:
			let decoded = try_build_timestamp(
				wire_time_zone,
				original.year(),
				original.month().into(),
				original.day(),
				original.hour(),
				original.minute(),
				original.second(),
			)
			.expect("decode ok");

			// Same instant in UTC AND same display-local components.
			assert_eq!(
				decoded, original,
				"roundtrip drifted at offset {offset_seconds}s: wire={wire_time_zone}"
			);
			assert_eq!(decoded.offset(), original_offset);
		}
	}
}
