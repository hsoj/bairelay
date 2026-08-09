use super::{BcCamera, Error, Result};
use crate::baichuan::bc::{model::*, xml::*};

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{reply_200_xml, MockConnection};

impl BcCamera {
	/// Request the [VersionInfo] xml
	pub async fn version(&self) -> Result<VersionInfo> {
		self.has_ability_ro("version").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_version = connection.subscribe(MSG_ID_VERSION, msg_num).await?;

		let version = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_VERSION,
				channel_id: self.channel_id,
				msg_num,
				stream_type: 0,
				response_code: 0,
				class: 0x6414, // IDK why
			},
			body: BcBody::ModernMsg(ModernMsg {
				..Default::default()
			}),
		};

		sub_version.send(version).await?;

		let modern_reply = sub_version.recv().await?;
		if modern_reply.meta.response_code != 200 {
			return Err(Error::CameraServiceUnavailable {
				id: modern_reply.meta.msg_id,
				code: modern_reply.meta.response_code,
			});
		}
		let mut modern_reply = modern_reply;
		if let BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(xml)),
			..
		}) = &mut modern_reply.body
		{
			if let Some(info) = xml.version_info.take() {
				return Ok(info);
			}
		}
		Err(Error::UnintelligibleReply {
			reply: std::sync::Arc::new(modern_reply),
			why: "Expected a VersionInfo message",
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn version_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_VERSION)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						version_info: Some(VersionInfo {
							name: "Argus".to_string(),
							firmwareVersion: "v3.0.0.5649_25111355".to_string(),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("version", false).await;
		let info = cam.version().await.expect("ok");
		assert_eq!(info.name, "Argus");
		assert_eq!(info.firmwareVersion, "v3.0.0.5649_25111355");
	}

	#[tokio::test]
	async fn version_missing_ability_returns_err() {
		// No ability granted; MissingAbility fires before a request
		// is ever sent, so we build an empty-script mock.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.version().await.expect_err("should fail");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	#[tokio::test]
	async fn version_non_200_returns_service_unavailable() {
		use crate::baichuan::bc_protocol::connection::mock::reply_err_code;
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_VERSION)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("version", false).await;
		let err = cam.version().await.expect_err("non-200 should error");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn version_missing_version_info_payload_returns_unintelligible() {
		// 200 reply but no VersionInfo → UnintelligibleReply path.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_VERSION)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("version", false).await;
		let err = cam.version().await.expect_err("missing info → err");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}
}
