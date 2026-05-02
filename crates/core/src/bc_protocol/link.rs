use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

#[cfg(test)]
use crate::bc_protocol::{
	camera_driver::CameraDriver,
	connection::mock::{reply_200_xml, reply_err_code, MockConnection},
};

impl BcCamera {
	/// Get the [LinkType] xml which contains the connection status of the camera
	///
	/// This is the same as `ping()` but with the return type
	pub async fn get_linktype(&self) -> Result<LinkType> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_PING, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_PING,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
				payload: None,
			}),
		};

		sub_get.send(get).await?;
		let msg = sub_get.recv().await?;
		if msg.meta.response_code != 200 {
			return Err(Error::CameraServiceUnavailable {
				id: msg.meta.msg_id,
				code: msg.meta.response_code,
			});
		}

		if let BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(BcXml {
				link_type: Some(link_type),
				..
			})),
			..
		}) = msg.body
		{
			Ok(link_type)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Expected LinkType xml but it was not received",
			})
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_linktype_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						link_type: Some(LinkType {
							link_type: "LAN".to_string(),
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let lt = cam.get_linktype().await.expect("ok");
		assert_eq!(lt.link_type, "LAN");
	}

	#[tokio::test]
	async fn get_linktype_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| reply_err_code(req, 400))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_linktype().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 400, .. }
		));
	}

	#[tokio::test]
	async fn get_linktype_missing_link_type_returns_unintelligible() {
		// 200 reply but no link_type xml → UnintelligibleReply path.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_linktype().await.expect_err("no link_type → err");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn keepalive_probe_treats_unintelligible_as_ok() {
		// Older firmware path: link-type query returns 200 with a
		// payload missing `link_type`. `get_linktype` would surface
		// `UnintelligibleReply`; `keepalive_probe` must swallow that
		// and report the camera alive.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.keepalive_probe().await.expect("unintelligible → Ok");
	}

	#[tokio::test]
	async fn keepalive_probe_happy_path_is_ok() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						link_type: Some(LinkType {
							link_type: "LAN".to_string(),
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.keepalive_probe().await.expect("ok");
	}

	#[tokio::test]
	async fn keepalive_probe_forwards_other_errors() {
		// Non-200 reply must not be swallowed by the lenient wrapper —
		// only `UnintelligibleReply` gets the special treatment.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| reply_err_code(req, 400))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.keepalive_probe().await.expect_err("400 → err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 400, .. }
		));
	}
}
