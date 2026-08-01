use super::{BcCamera, Result};
use crate::baichuan::bc::model::*;

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{reply_200_empty, MockConnection};

impl BcCamera {
	/// Ping the camera will either return Ok(()) which means a sucess reply
	/// or error
	pub async fn ping(&self) -> Result<()> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_ping = connection.subscribe(MSG_ID_PING, msg_num).await?;

		let ping = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_PING,
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

		sub_ping.send(ping).await?;

		let reply = sub_ping.recv().await?;
		if reply.meta.response_code != 200 {
			return Err(crate::baichuan::Error::CameraServiceUnavailable {
				id: reply.meta.msg_id,
				code: reply.meta.response_code,
			});
		}

		tracing::trace!("Ping complete");
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc_protocol::connection::mock::reply_err_code;

	#[tokio::test]
	async fn ping_non_200_reply_returns_camera_service_unavailable() {
		// Regression: pre-fix, `ping` discarded `recv()` without
		// inspecting `response_code`, so a 500 reply silently
		// returned `Ok(())`. Now it must surface
		// `CameraServiceUnavailable`.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.ping().await.expect_err("non-200 must be err");
		assert!(matches!(
			err,
			crate::baichuan::Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn ping_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.ping().await.expect("ok");
	}

	#[tokio::test]
	async fn ping_connection_drop_returns_err() {
		// Scripted: first exchange MATCHES on msg_id but returns None
		// (simulates a camera that dropped the request). The mux
		// never sends a reply — `sub.recv()` would wait forever, so
		// we cap with a short wall-clock timeout.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_none()
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let r = tokio::time::timeout(std::time::Duration::from_millis(200), cam.ping()).await;
		assert!(
			r.is_err(),
			"ping should not complete when camera never replies"
		);
	}
}
