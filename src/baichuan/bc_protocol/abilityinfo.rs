use super::{BcCamera, Error, Result};
use crate::baichuan::bc::{model::*, xml::*};
use tracing::*;

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{
	reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Get the ability info xml for the current user
	pub async fn get_abilityinfo(&self) -> Result<AbilityInfo> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_ABILITY_INFO, msg_num).await?;
		let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_ABILITY_INFO,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    user_name: Some(self.get_credentials().username.clone()),
                    token: Some("system, streaming, PTZ, IO, security, replay, disk,  network, alarm, record, video, image".to_string()),
                    ..Default::default()
                }),
                payload: None,
            }),
        };

		sub_get.send(get).await?;
		let mut msg = sub_get.recv().await?;
		if msg.meta.response_code != 200 {
			return Err(Error::CameraServiceUnavailable {
				id: msg.meta.msg_id,
				code: msg.meta.response_code,
			});
		}

		if let BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(xml)),
			..
		}) = &mut msg.body
		{
			if let Some(ability_info) = xml.ability_info.take() {
				return Ok(ability_info);
			}
		}
		Err(Error::UnintelligibleReply {
			reply: std::sync::Arc::new(msg),
			why: "Expected AbilityInfo xml but it was not received",
		})
	}

	/// Populate ability list of the camera
	pub async fn populate_abilities(&self) -> Result<()> {
		let info = self.get_abilityinfo().await?;
		let mut ser_buf = bytes::BytesMut::new();
		let info_res = quick_xml::se::to_writer(&mut ser_buf, &info).map(|_| ser_buf);
		if let Ok(Ok(info_str)) = info_res.map(|b| std::str::from_utf8(&b).map(|a| a.to_owned())) {
			debug!("Abilities: {}", info_str);
		}

		let mut abilities: Vec<String> = vec![];

		let mut tokens: Vec<Option<&AbilityInfoToken>> = vec![
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

		for token in tokens.drain(..).flatten() {
			for sub_module in token.sub_module.iter() {
				abilities.extend(
					sub_module
						.ability_value
						.replace(' ', "")
						.split(',')
						.map(|s| s.to_string()),
				);
			}
		}

		let mut locked_abilities = self.abilities.write().await;
		for ability in abilities.iter() {
			// `rsplit_once` so the suffix is the kind and the prefix
			// (with any embedded underscores intact) is the ability
			// name. A naive `split('_').next().next()` silently drops
			// any future ability whose name contains an underscore
			// (e.g. `two_way_audio_rw`).
			let Some((ability_name, ability_kind)) = ability.rsplit_once('_') else {
				continue;
			};
			match ability_kind {
				"rw" => {
					locked_abilities.insert(ability_name.to_string(), super::ReadKind::ReadWrite);
				}
				"ro" => {
					locked_abilities.insert(ability_name.to_string(), super::ReadKind::ReadOnly);
				}
				_ => {
					continue;
				}
			}
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_abilityinfo_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_ABILITY_INFO)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						ability_info: Some(AbilityInfo {
							username: "admin".to_string(),
							system: Some(AbilityInfoToken {
								sub_module: vec![AbilityInfoSubModule {
									channel_id: Some(0),
									ability_value: "reboot_rw, general_rw".to_string(),
								}],
							}),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let info = cam.get_abilityinfo().await.expect("ok");
		assert_eq!(info.username, "admin");
		assert!(info.system.is_some());
	}

	#[tokio::test]
	async fn get_abilityinfo_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_ABILITY_INFO)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_abilityinfo().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_abilityinfo_missing_payload_returns_unintelligible() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_ABILITY_INFO)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_abilityinfo().await.expect_err("err");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn populate_abilities_parses_ro_rw_and_skips_garbage() {
		// One system module with three abilities: reboot is rw, general
		// is ro, and a third without an _rw/_ro suffix is skipped.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_ABILITY_INFO)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						ability_info: Some(AbilityInfo {
							username: "admin".into(),
							system: Some(AbilityInfoToken {
								sub_module: vec![AbilityInfoSubModule {
									channel_id: Some(0),
									ability_value: "reboot_rw, general_ro, nothing".into(),
								}],
							}),
							network: Some(AbilityInfoToken {
								sub_module: vec![AbilityInfoSubModule {
									channel_id: Some(0),
									ability_value: "ping_rw".into(),
								}],
							}),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.populate_abilities().await.expect("ok");
		// Sanity: an rw ability satisfies the ro and rw has_ability checks.
		cam.has_ability_ro("reboot").await.expect("ro ok");
		cam.has_ability_rw("reboot").await.expect("rw ok");
		cam.has_ability_ro("general").await.expect("ro ok");
		// Garbage/unparseable arm didn't crash.
	}

	#[tokio::test]
	async fn populate_abilities_preserves_underscores_in_ability_name() {
		// Forward-compat: an ability whose name contains underscores
		// (e.g. `two_way_audio_rw`) used to be silently dropped because
		// `split('_').next().next()` truncated to `("two", "way")`.
		// `rsplit_once('_')` now correctly yields name=`two_way_audio`,
		// kind=`rw`.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_ABILITY_INFO)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						ability_info: Some(AbilityInfo {
							username: "admin".into(),
							system: Some(AbilityInfoToken {
								sub_module: vec![AbilityInfoSubModule {
									channel_id: Some(0),
									ability_value: "two_way_audio_rw".into(),
								}],
							}),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.populate_abilities().await.expect("ok");
		cam.has_ability_rw("two_way_audio")
			.await
			.expect("name with underscores survives split");
	}

	#[tokio::test]
	async fn populate_abilities_propagates_err_from_get_abilityinfo() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_ABILITY_INFO)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.populate_abilities().await.expect_err("err");
		assert!(matches!(err, Error::CameraServiceUnavailable { .. }));
	}
}
