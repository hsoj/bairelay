use super::BcConnection;
use crate::baichuan::bcmedia::codex::BcMediaCodex;
use crate::baichuan::{bc::model::*, bcmedia::model::*, Error, Result};
use futures::stream::{Stream, TryStreamExt};
use std::io::{Error as IoError, Result as IoResult};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Receiver;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tokio_util::codec::FramedRead;
use tokio_util::compat::FuturesAsyncReadCompatExt;

pub struct BcSubscription<'a> {
	rx: ReceiverStream<Result<Bc>>,
	msg_num: Option<u32>,
	conn: &'a BcConnection,
}

pub struct BcStream<'a> {
	rx: &'a mut ReceiverStream<Result<Bc>>,
}

impl Unpin for BcStream<'_> {}

impl Stream for BcStream<'_> {
	type Item = Result<Bc>;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Bc>>> {
		let mut this = self.as_mut();
		match Pin::new(&mut this.rx).poll_next(cx) {
			Poll::Ready(Some(bc)) => Poll::Ready(Some(bc)),
			Poll::Ready(None) => Poll::Ready(None),
			Poll::Pending => Poll::Pending,
		}
	}
}

impl<'a> BcSubscription<'a> {
	pub fn new(
		rx: Receiver<Result<Bc>>,
		msg_num: Option<u32>,
		conn: &'a BcConnection,
	) -> BcSubscription<'a> {
		BcSubscription {
			rx: ReceiverStream::new(rx),
			msg_num,
			conn,
		}
	}

	pub async fn send(&self, bc: Bc) -> Result<()> {
		if let Some(msg_num) = self.msg_num {
			// `debug_assert!` rather than `assert!`: caller-supplied `bc`
			// is normally constructed within the same crate, but the type
			// is `pub` and a future caller could pass a mismatched
			// `meta.msg_num`. Hard-panicking on the connection task is
			// the wrong response — return the contract error instead.
			if bc.meta.msg_num as u32 != msg_num {
				return Err(Error::Other("BcSub::send msg_num mismatch"));
			}
		} else {
			tracing::debug!("Sending message before msg_num has been aquired");
		}
		self.conn.send(bc).await?;
		Ok(())
	}

	pub async fn recv(&mut self) -> Result<Bc> {
		let bc = self.rx.next().await.ok_or(Error::DroppedSubscriber)?;
		if let Ok(bc) = &bc {
			if let Some(msg_num) = self.msg_num {
				// `bc.meta.msg_num` is wire-derived. The `Poller`
				// upstream filters by `msg_num` before delivery, so a
				// mismatch here is a Poller invariant violation rather
				// than a hostile-peer vector — but `assert!`-aborting
				// the connection task on a Poller bug is still the wrong
				// tool. Drop the message with a warn and let `recv`
				// return the next one.
				if bc.meta.msg_num as u32 != msg_num {
					tracing::warn!(
						"BcSub::recv msg_num mismatch (got {}, want {}); dropping",
						bc.meta.msg_num,
						msg_num,
					);
					return Err(Error::Other("BcSub::recv msg_num mismatch"));
				}
			} else {
				// Leaning number now
				self.msg_num = Some(bc.meta.msg_num as u32);
			}
		}
		bc
	}

	#[allow(unused)]
	pub fn bc_stream(&'_ mut self) -> BcStream<'_> {
		BcStream { rx: &mut self.rx }
	}

	pub fn payload_stream(&'_ mut self) -> impl Stream<Item = IoResult<Vec<u8>>> + '_ {
		(&mut self.rx).filter_map(|x| match x {
			Ok(Bc {
				meta: BcMeta { .. },
				body:
					BcBody::ModernMsg(ModernMsg {
						payload: Some(BcPayloads::Binary(data)),
						..
					}),
			}) => Some(Ok(data)),
			Ok(_) => None,
			Err(e) => Some(Err(IoError::other(e))),
		})
	}

	pub fn bcmedia_stream(&'_ mut self, strict: bool) -> impl Stream<Item = Result<BcMedia>> + '_ {
		let async_read = self
			.payload_stream()
			.map(|frame| frame)
			.into_async_read()
			.compat();
		FramedRead::new(async_read, BcMediaCodex::new(strict)).map(|frame| frame)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc_protocol::connection::mock::MockConnection;
	use crate::baichuan::bc_protocol::BcCamera;

	/// Drive the `send` debug-path branch: `msg_num = None` means the
	/// subscription hasn't yet committed to a specific request number,
	/// so `send` logs the "Sending message before msg_num has been
	/// aquired" debug line. Exercise it via a `subscribe_to_id` handle
	/// which hands back a `BcSubscription` with `msg_num: None`.
	#[tokio::test]
	async fn bcsub_send_without_msg_num_logs_and_forwards() {
		let mock = MockConnection::new()
			.expect_msg(123)
			.reply_with(|req| {
				// Echo reply with response_code 200 for the recv() path
				// below. This also tests the `None -> learn-the-number`
				// arm inside `recv`.
				Bc {
					meta: BcMeta {
						msg_id: 123,
						channel_id: 0,
						msg_num: req.meta.msg_num,
						stream_type: 0,
						response_code: 200,
						class: 0x6414,
					},
					body: BcBody::ModernMsg(ModernMsg::default()),
				}
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let conn = cam.get_connection();
		let mut sub = conn.subscribe_to_id(123).await.expect("ok");

		let req = Bc {
			meta: BcMeta {
				msg_id: 123,
				channel_id: 0,
				msg_num: 77,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg::default()),
		};
		sub.send(req).await.expect("ok");
		let reply = sub.recv().await.expect("ok");
		assert_eq!(reply.meta.msg_num, 77);
	}

	/// Drive the `payload_stream` filter_map to emit a binary payload.
	#[tokio::test]
	async fn bcsub_payload_stream_forwards_binary_bodies() {
		let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bc>>(4);
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let conn = cam.get_connection();
		let mut sub = BcSubscription::new(rx, None, &conn);

		let bc = Bc {
			meta: BcMeta {
				msg_id: 1,
				channel_id: 0,
				msg_num: 0,
				stream_type: 0,
				response_code: 200,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
				payload: Some(BcPayloads::Binary(vec![1, 2, 3])),
			}),
		};
		tx.send(Ok(bc)).await.unwrap();
		drop(tx);

		let mut stream = sub.payload_stream();
		let first = futures::StreamExt::next(&mut stream)
			.await
			.expect("got one")
			.expect("ok");
		assert_eq!(first, vec![1, 2, 3]);
	}

	#[tokio::test]
	async fn bcsub_payload_stream_filters_non_binary_and_propagates_err() {
		let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bc>>(4);
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let conn = cam.get_connection();
		let mut sub = BcSubscription::new(rx, None, &conn);

		// First: a non-binary ModernMsg → dropped by filter_map.
		tx.send(Ok(Bc {
			meta: BcMeta {
				msg_id: 1,
				channel_id: 0,
				msg_num: 0,
				stream_type: 0,
				response_code: 200,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
				payload: None,
			}),
		}))
		.await
		.unwrap();
		// Second: an error → passed through as Err.
		tx.send(Err(Error::DroppedSubscriber)).await.unwrap();
		drop(tx);

		let mut stream = sub.payload_stream();
		let first = futures::StreamExt::next(&mut stream)
			.await
			.expect("got one");
		assert!(first.is_err());
	}

	#[tokio::test]
	async fn bcsub_bc_stream_forwards_messages() {
		let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bc>>(4);
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let conn = cam.get_connection();
		let mut sub = BcSubscription::new(rx, None, &conn);

		let bc = Bc {
			meta: BcMeta {
				msg_id: 5,
				channel_id: 0,
				msg_num: 7,
				stream_type: 0,
				response_code: 200,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg::default()),
		};
		tx.send(Ok(bc)).await.unwrap();
		drop(tx);

		let mut stream = sub.bc_stream();
		let first = futures::StreamExt::next(&mut stream)
			.await
			.expect("got one")
			.expect("ok");
		assert_eq!(first.meta.msg_num, 7);
	}
}
