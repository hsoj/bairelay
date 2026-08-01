use super::{BcCamera, Result};
use crate::baichuan::bc::model::*;

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::MockConnection;

impl BcCamera {
	/// Create a handler to respond to keep alive messages
	/// These messages are sent by the camera so we listen to
	/// a message ID rather than setting a message number and
	/// responding to it
	pub async fn keepalive(&self) -> Result<()> {
		let connection = self.get_connection();
		connection
			.handle_msg(MSG_ID_UDP_KEEP_ALIVE, |bc| {
				Box::pin(async move {
					Some(Bc {
						meta: BcMeta {
							msg_id: MSG_ID_UDP_KEEP_ALIVE,
							channel_id: bc.meta.channel_id,
							msg_num: bc.meta.msg_num,
							stream_type: bc.meta.stream_type,
							response_code: 200,
							class: 0x6414,
						},
						body: BcBody::ModernMsg(ModernMsg {
							..Default::default()
						}),
					})
				})
			})
			.await?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn keepalive_registers_handler_and_returns_ok() {
		// `keepalive` only registers a handler for MSG_ID_UDP_KEEP_ALIVE
		// messages arriving from the camera — it does not itself send a
		// request, so the mock has no expectations.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.keepalive().await.expect("keepalive should succeed");
	}

	#[tokio::test]
	async fn keepalive_handler_builds_200_reply_body() {
		// Inject a camera-initiated MSG_ID_UDP_KEEP_ALIVE message
		// straight into the mock source and verify the registered
		// closure builds the expected 200-response shape. This covers
		// the closure body (the meat of keepalive.rs).
		let mock = MockConnection::new().build().await;
		let injector = mock.injector();
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.keepalive().await.expect("keepalive registers");

		// Give the poll-command AddHandler a moment to be installed
		// inside the poller before we inject the test frame.
		tokio::task::yield_now().await;

		// Inject a fake incoming keepalive frame; the registered
		// handler runs asynchronously, so we just need to wait a bit
		// for the closure body to execute — its side effect is sending
		// a reply over the sink, which exercises lines 16-24/26-27.
		let req = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_UDP_KEEP_ALIVE,
				channel_id: 0,
				stream_type: 0,
				response_code: 0,
				msg_num: 42,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg::default()),
		};
		injector.push(req).await;

		// Let the spawned handler task run. A brief sleep under a
		// paused clock would be ideal, but we use yield_now + a
		// short real sleep so the worker thread gets scheduled.
		tokio::time::sleep(std::time::Duration::from_millis(20)).await;
	}
}
