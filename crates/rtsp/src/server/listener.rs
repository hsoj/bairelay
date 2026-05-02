//! RTSP server listener: accepts TCP connections and spawns per-connection tasks.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::provider::StreamProvider;
use crate::rtsp::auth::UserCred;
use crate::server::tls::TlsConfig;
use crate::server::udp_pool::UdpPortPool;

/// Maximum time we wait for a TLS handshake before dropping the
/// connection. A slow or malicious client otherwise pins an accept
/// task indefinitely.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Static configuration for the RTSP server.
#[derive(Clone)]
pub struct ServerConfig {
	/// TCP bind address (e.g. `0.0.0.0:8554` or `0.0.0.0:8555`).
	pub bind: SocketAddr,
	/// Auth realm advertised in WWW-Authenticate headers.
	pub realm: String,
	/// User credentials. Empty list means no authentication.
	pub users: Vec<UserCred>,
	/// When `Some`, accepted TCP sockets are wrapped in a TLS handshake
	/// before any RTSP bytes are read. `None` keeps the listener plain.
	pub tls: Option<TlsConfig>,
	/// Maximum number of concurrent client connections. `None` is
	/// unlimited; the binary sets a finite default so a fork-bomb client
	/// can't exhaust file descriptors. Connections beyond the cap wait
	/// in the OS listen backlog until a slot frees.
	pub max_connections: Option<usize>,
}

/// A running RTSP server.
pub struct RtspServer;

impl RtspServer {
	/// Bind the listener and accept connections until `cancel` fires.
	///
	/// Returns when the listener exits (on cancel or unrecoverable error).
	/// Convenience wrapper around [`Self::serve_with_listener`] for callers
	/// that don't need to pre-bind. The binary uses the pre-bind path so
	/// listener errors surface synchronously at startup.
	pub async fn serve(
		config: ServerConfig,
		provider: Arc<dyn StreamProvider>,
		cancel: CancellationToken,
	) -> std::io::Result<()> {
		let listener = TcpListener::bind(config.bind).await?;
		Self::serve_with_listener(listener, config, provider, cancel).await
	}

	/// Run the accept loop against an already-bound `TcpListener`. The
	/// binary binds synchronously in `main.rs` so a bind failure (port in
	/// use, permission denied) fails the daemon at startup instead of
	/// being logged inside the spawned task.
	pub async fn serve_with_listener(
		listener: TcpListener,
		config: ServerConfig,
		provider: Arc<dyn StreamProvider>,
		cancel: CancellationToken,
	) -> std::io::Result<()> {
		let acceptor: Option<TlsAcceptor> = config
			.tls
			.as_ref()
			.map(|t| TlsAcceptor::from(Arc::clone(&t.server_config)));
		let scheme = if acceptor.is_some() { "rtsps" } else { "rtsp" };
		tracing::info!(
			bind = %config.bind,
			scheme,
			max_connections = config.max_connections.map(|n| n as i64).unwrap_or(-1),
			"RTSP server listening"
		);

		let udp_pool = Arc::new(UdpPortPool::new());
		let server_bind_ip = config.bind.ip();

		// Concurrent-connection cap. A finite cap blocks the accept loop
		// on `acquire_owned()` until an in-flight handler drops its
		// permit; the OS listen backlog absorbs the SYN overflow during
		// the wait — standard backpressure. `None` uses an effectively-
		// unbounded semaphore (capacity > 2^62) so the same code path
		// handles both modes.
		let conn_capacity = config
			.max_connections
			.unwrap_or(tokio::sync::Semaphore::MAX_PERMITS);
		let conn_sem = Arc::new(tokio::sync::Semaphore::new(conn_capacity));

		loop {
			// Acquire a connection slot before calling accept(). When the
			// cap is hit we deliberately stop accepting; new clients sit
			// in the kernel's listen backlog until a slot frees.
			let permit = tokio::select! {
				_ = cancel.cancelled() => {
					tracing::info!(scheme, "RTSP server shutting down");
					return Ok(());
				}
				p = Arc::clone(&conn_sem).acquire_owned() => match p {
					Ok(p) => p,
					Err(_) => return Ok(()), // semaphore closed
				}
			};
			tokio::select! {
				_ = cancel.cancelled() => {
					tracing::info!(scheme, "RTSP server shutting down");
					return Ok(());
				}
				result = listener.accept() => {
					let (stream, peer) = match result {
						Ok(pair) => pair,
						Err(e) => {
							tracing::warn!(error = %e, "accept failed");
							continue;
						}
					};
					let _ = stream.set_nodelay(true);
					// Capture socket-level facts on the bare TcpStream
					// before any TLS wrap. Multi-homed hosts behind strict
					// firewalls otherwise drop UDP RTP whose source IP
					// doesn't match the RTSP TCP 5-tuple's local IP.
					let peer_ip = peer.ip();
					let local_ip = stream
						.local_addr()
						.map(|a| a.ip())
						.unwrap_or_else(|e| {
							tracing::warn!(error = %e, "local_addr() failed; falling back to loopback");
							std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
						});
					let provider = Arc::clone(&provider);
					let users = config.users.clone();
					let realm = config.realm.clone();
					let pool = Arc::clone(&udp_pool);
					let conn_cancel = cancel.clone();
					match acceptor.as_ref() {
						None => {
							tracing::debug!(peer = %peer, "new RTSP connection");
							tokio::spawn(async move {
								crate::server::connection::handle_connection(
									stream,
									provider,
									users,
									realm,
									pool,
									server_bind_ip,
									peer_ip,
									local_ip,
									false,
									conn_cancel,
								)
								.await;
								// Hold the permit until the connection
								// task ends; dropping releases the slot.
								drop(permit);
							});
						}
						Some(acc) => {
							let acc = acc.clone();
							tracing::debug!(peer = %peer, "new RTSPS connection (TLS handshake pending)");
							tokio::spawn(async move {
								match tokio::time::timeout(
									TLS_HANDSHAKE_TIMEOUT,
									acc.accept(stream),
								)
								.await
								{
									Ok(Ok(tls)) => {
										crate::server::connection::handle_connection(
											tls,
											provider,
											users,
											realm,
											pool,
											server_bind_ip,
											peer_ip,
											local_ip,
											true,
											conn_cancel,
										)
										.await;
									}
									Ok(Err(e)) => {
										tracing::warn!(peer = %peer, error = %e, "TLS handshake failed");
									}
									Err(_) => {
										tracing::warn!(peer = %peer, "TLS handshake timed out");
									}
								}
								drop(permit);
							});
						}
					}
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::provider::{Frame, StreamError, StreamProvider, SubscriptionHandle};
	use crate::sdp::{SdpParams, VideoParams};
	use crate::url::StreamKind;

	struct NoopProvider;

	#[async_trait::async_trait]
	impl StreamProvider for NoopProvider {
		async fn subscribe(
			&self,
			camera: &str,
			_kind: StreamKind,
			_user: Option<&str>,
		) -> Result<SubscriptionHandle, StreamError> {
			let (_tx, rx) = tokio::sync::broadcast::channel::<Frame>(1);
			Ok(SubscriptionHandle {
				frames: rx,
				sdp_params: SdpParams {
					server_ip: "0.0.0.0".to_string(),
					session_id: "0".to_string(),
					session_name: camera.to_string(),
					video: Some(VideoParams {
						codec: crate::codec::VideoCodec::H264,
						payload_type: 96,
						sps: vec![],
						pps: vec![],
						vps: None,
						profile_level_id: [0x42, 0x00, 0x1f],
					}),
					audio: None,
				},
				last_frame: Arc::new(crate::buffer::LastFrameBuffer::new()),
				guard: Box::new(()),
			})
		}
	}

	#[tokio::test]
	async fn serve_with_listener_accepts_prebound_socket() {
		// Pre-bind on an ephemeral port — exactly the pattern the binary
		// uses to surface bind errors synchronously at startup.
		let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
		let addr = listener.local_addr().unwrap();
		let cfg = ServerConfig {
			bind: addr,
			realm: "bairelay-test".into(),
			users: vec![],
			tls: None,
			max_connections: None,
		};
		let cancel = CancellationToken::new();
		let cancel_for_server = cancel.clone();
		let task = tokio::spawn(async move {
			RtspServer::serve_with_listener(
				listener,
				cfg,
				Arc::new(NoopProvider) as Arc<dyn StreamProvider>,
				cancel_for_server,
			)
			.await
		});
		// Confirm the listener is actually accepting on the given port.
		tokio::time::sleep(std::time::Duration::from_millis(20)).await;
		let _conn = tokio::net::TcpStream::connect(addr)
			.await
			.expect("server must accept after pre-bind");
		cancel.cancel();
		let res = tokio::time::timeout(std::time::Duration::from_secs(2), task)
			.await
			.expect("task joined within deadline")
			.expect("task panic-free");
		assert!(res.is_ok(), "graceful cancel must return Ok, got {res:?}");
	}

	#[tokio::test]
	async fn max_connections_caps_concurrent_handlers() {
		// max_connections = 1: open two connections; the first
		// successfully exchanges OPTIONS; the second should sit on the
		// semaphore and not get a response until the first closes.
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let cfg = ServerConfig {
			bind: addr,
			realm: "test".into(),
			users: vec![],
			tls: None,
			max_connections: Some(1),
		};
		let cancel = CancellationToken::new();
		let cancel_for_server = cancel.clone();
		let server = tokio::spawn(async move {
			let _ = RtspServer::serve_with_listener(
				listener,
				cfg,
				Arc::new(NoopProvider) as Arc<dyn StreamProvider>,
				cancel_for_server,
			)
			.await;
		});
		tokio::time::sleep(std::time::Duration::from_millis(20)).await;

		// Client 1: full request → response.
		let mut c1 = tokio::net::TcpStream::connect(addr).await.unwrap();
		c1.write_all(b"OPTIONS rtsp://x/cam1 RTSP/1.0\r\nCSeq: 1\r\n\r\n")
			.await
			.unwrap();
		let mut buf1 = [0u8; 1024];
		let n1 = tokio::time::timeout(std::time::Duration::from_secs(1), c1.read(&mut buf1))
			.await
			.expect("c1 read must complete")
			.unwrap();
		assert!(n1 > 0, "c1 must receive a response");

		// Client 2: connect + send while c1 holds the only slot — must
		// not get a server response.
		let mut c2 = tokio::net::TcpStream::connect(addr).await.unwrap();
		c2.write_all(b"OPTIONS rtsp://x/cam1 RTSP/1.0\r\nCSeq: 1\r\n\r\n")
			.await
			.unwrap();
		let mut buf2 = [0u8; 1024];
		let res =
			tokio::time::timeout(std::time::Duration::from_millis(200), c2.read(&mut buf2)).await;
		assert!(
			res.is_err(),
			"c2 must wait — semaphore at capacity should defer accept"
		);

		// Drop c1 → server-side handler exits, permit drops, c2 unblocks.
		drop(c1);
		let n2 = tokio::time::timeout(std::time::Duration::from_secs(2), c2.read(&mut buf2))
			.await
			.expect("c2 read must complete after slot frees")
			.unwrap();
		assert!(n2 > 0, "c2 must receive a response once c1 closes");

		cancel.cancel();
		let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server).await;
	}
}
