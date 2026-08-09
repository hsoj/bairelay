//! Handles battery related messages
//!
//! There are primarily two messages:
//! - BatteryInfoList which the camera sends as part of its login info
//! - BatteryInfo which the client can request on demand
//!

use super::{BcCamera, Result};
use crate::baichuan::{
	bc::{model::*, xml::BatteryInfo},
	Error,
};

#[cfg(test)]
use crate::baichuan::bc::xml::BcXml;
#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{reply_200_xml, MockConnection};

impl BcCamera {
	/// Requests the current battery status of the camera
	pub async fn battery_info(&self) -> Result<BatteryInfo> {
		let connection = self.get_connection();

		let msg_num = self.new_message_num();
		let mut sub = connection.subscribe(MSG_ID_BATTERY_INFO, msg_num).await?;

		let msg = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_BATTERY_INFO,
				channel_id: self.channel_id,
				msg_num,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: Some(Extension {
					channel_id: Some(self.channel_id),
					..Default::default()
				}),
				payload: None,
			}),
		};

		sub.send(msg).await?;
		let mut msg = sub.recv().await?;

		if msg.meta.response_code == 200 {
			if let BcBody::ModernMsg(ModernMsg {
				payload: Some(BcPayloads::BcXml(xml)),
				..
			}) = &mut msg.body
			{
				if let Some(battery_info) = xml.battery_info.take() {
					return Ok(battery_info);
				}
			}
		}
		Err(Error::UnintelligibleReply {
			reply: std::sync::Arc::new(msg),
			why: "The camera did not accept the battery info (maybe no battery) command.",
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn battery_info_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_BATTERY_INFO)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						battery_info: Some(BatteryInfo {
							battery_percent: 73,
							temperature: 21,
							charge_status: "chargeComplete".to_string(),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;

		let cam = BcCamera::from_mock_connection(mock).await;
		let info = cam
			.battery_info()
			.await
			.expect("battery_info should succeed");
		assert_eq!(info.battery_percent, 73);
		assert_eq!(info.temperature, 21);
		assert_eq!(info.charge_status, "chargeComplete");
	}

	#[tokio::test]
	async fn battery_info_non_200_returns_err() {
		// Empty reply (no battery_info xml) triggers the
		// `UnintelligibleReply` branch even with response_code 200.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_BATTERY_INFO)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;

		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.battery_info().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}
}
