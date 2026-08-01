use super::{BcCamera, Error, Result};
use crate::baichuan::bc::model::*;

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{
	reply_200_empty, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Reboot the camera
	pub async fn reboot(&self) -> Result<()> {
		self.has_ability_rw("reboot").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub = connection.subscribe(MSG_ID_REBOOT, msg_num).await?;

		let msg = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_REBOOT,
				channel_id: self.channel_id,
				msg_num,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				..Default::default()
			}),
		};

		sub.send(msg).await?;
		let msg = sub.recv().await?;

		if let BcMeta {
			response_code: 200, ..
		} = msg.meta
		{
			Ok(())
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "The camera did not accept the reboot command",
			})
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn reboot_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_REBOOT)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("reboot", true).await;
		cam.reboot().await.expect("reboot should succeed");
	}

	#[tokio::test]
	async fn reboot_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_REBOOT)
			.reply_with(|req| reply_err_code(req, 400))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("reboot", true).await;
		let err = cam.reboot().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}
}
