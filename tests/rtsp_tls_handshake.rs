//! TLS handshake integration tests.
//!
//! Spins up `RtspServer::serve` with `tls = Some(...)` and drives a real
//! `tokio_rustls::TlsConnector` from the same process. Each test mints
//! its own CA + server leaf + (optionally) client leaf via `rcgen`, so
//! `cargo test` is fully hermetic — no openssl on the host required.
//!
//! These tests stop at the TLS handshake. Verifying the RTSP layer over
//! TLS is left to the existing rtsp_integration_test.rs / fixture_replay.rs
//! shapes once more thorough live coverage exists; this file provides
//! handshake correctness for all three `ClientAuthMode` modes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

use bairelay::rtsp::protocol::auth::UserCred;
use bairelay::rtsp::provider::{StreamError, StreamProvider, SubscriptionHandle};
use bairelay::rtsp::server::{ClientAuthMode, RtspServer, ServerConfig, TlsConfig};
use bairelay::rtsp::url::StreamKind;

fn install_crypto() {
	// Forward to the shared `bairelay::rtsp::server::install_crypto_provider`
	// helper so the OnceLock + Err-swallow semantics live in exactly one
	// place. Wrapper kept (vs inlining the call) only because every test
	// site already names the function `install_crypto`; renaming each call
	// site would be churn for no behaviour change.
	bairelay::rtsp::server::install_crypto_provider();
}

/// Empty provider — handshake tests stop before any RTSP request is sent.
struct EmptyProvider;

#[async_trait]
impl StreamProvider for EmptyProvider {
	async fn subscribe(
		&self,
		_camera: &str,
		_kind: StreamKind,
		_user: Option<&str>,
	) -> Result<SubscriptionHandle, StreamError> {
		Err(StreamError::UnknownCamera)
	}
}

struct Pki {
	server_chain: Vec<CertificateDer<'static>>,
	server_key: PrivateKeyDer<'static>,
	ca_root_store: Arc<RootCertStore>,
	client_chain: Vec<CertificateDer<'static>>,
	client_key: PrivateKeyDer<'static>,
}

fn make_pki() -> Pki {
	use rcgen::{BasicConstraints, IsCa, KeyUsagePurpose};

	let ca_kp = KeyPair::generate().unwrap();
	let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
	ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
	ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
	ca_params.distinguished_name = {
		let mut dn = rcgen::DistinguishedName::new();
		dn.push(rcgen::DnType::CommonName, "bairelay-test-ca");
		dn
	};
	let ca = ca_params.self_signed(&ca_kp).unwrap();

	let server_kp = KeyPair::generate().unwrap();
	let server_params =
		CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
	let server_cert = server_params.signed_by(&server_kp, &ca, &ca_kp).unwrap();

	let client_kp = KeyPair::generate().unwrap();
	let client_params = CertificateParams::new(vec!["test-client".into()]).unwrap();
	let client_cert = client_params.signed_by(&client_kp, &ca, &ca_kp).unwrap();

	let mut roots = RootCertStore::empty();
	roots.add(ca.der().clone()).unwrap();

	Pki {
		server_chain: vec![server_cert.der().clone()],
		server_key: PrivateKeyDer::Pkcs8(server_kp.serialize_der().into()),
		ca_root_store: Arc::new(roots),
		client_chain: vec![client_cert.der().clone()],
		client_key: PrivateKeyDer::Pkcs8(client_kp.serialize_der().into()),
	}
}

async fn spawn_tls_server(tls: TlsConfig) -> (SocketAddr, CancellationToken) {
	spawn_tls_server_with_users(tls, vec![]).await
}

async fn spawn_tls_server_with_users(
	tls: TlsConfig,
	users: Vec<UserCred>,
) -> (SocketAddr, CancellationToken) {
	// Pick a free loopback port via probe, drop, rebind. The window is
	// small enough that on practical CI it works first try; fixture-replay
	// uses the same pattern.
	let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = probe.local_addr().unwrap();
	drop(probe);

	let cancel = CancellationToken::new();
	let server_cancel = cancel.clone();
	let provider: Arc<dyn StreamProvider> = Arc::new(EmptyProvider);
	let cfg = ServerConfig {
		bind: addr,
		realm: "tls-test".to_string(),
		users,
		tls: Some(tls),
		max_connections: None,
	};
	tokio::spawn(async move {
		let _ = RtspServer::serve(cfg, provider, server_cancel).await;
	});

	// Poll until the listener accepts a TCP connection.
	for _ in 0..20 {
		tokio::time::sleep(Duration::from_millis(50)).await;
		if TcpStream::connect(addr).await.is_ok() {
			break;
		}
	}
	(addr, cancel)
}

fn make_client_config(roots: &Arc<RootCertStore>) -> Arc<RustlsClientConfig> {
	let mut store = RootCertStore::empty();
	for c in roots.roots.iter() {
		store.roots.push(c.clone());
	}
	let cfg = RustlsClientConfig::builder()
		.with_root_certificates(store)
		.with_no_client_auth();
	Arc::new(cfg)
}

fn make_client_config_with_cert(
	roots: &Arc<RootCertStore>,
	chain: Vec<CertificateDer<'static>>,
	key: PrivateKeyDer<'static>,
) -> Arc<RustlsClientConfig> {
	let mut store = RootCertStore::empty();
	for c in roots.roots.iter() {
		store.roots.push(c.clone());
	}
	let cfg = RustlsClientConfig::builder()
		.with_root_certificates(store)
		.with_client_auth_cert(chain, key)
		.expect("with_client_auth_cert");
	Arc::new(cfg)
}

#[tokio::test]
async fn tls_handshake_completes_for_no_client_auth() {
	install_crypto();
	let pki = make_pki();
	let tls = TlsConfig::build(pki.server_chain, pki.server_key, ClientAuthMode::None)
		.expect("build TlsConfig");
	let (addr, cancel) = spawn_tls_server(tls).await;

	let connector = TlsConnector::from(make_client_config(&pki.ca_root_store));
	let tcp = TcpStream::connect(addr).await.unwrap();
	let server_name = ServerName::try_from("localhost").unwrap();
	let res = connector.connect(server_name, tcp).await;
	assert!(res.is_ok(), "handshake must succeed: {res:?}");

	cancel.cancel();
}

#[tokio::test]
async fn require_mode_rejects_client_without_cert() {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	install_crypto();
	let pki = make_pki();
	let tls = TlsConfig::build(
		pki.server_chain,
		pki.server_key,
		ClientAuthMode::Require {
			roots: Arc::clone(&pki.ca_root_store),
		},
	)
	.expect("build TlsConfig");
	let (addr, cancel) = spawn_tls_server(tls).await;

	let connector = TlsConnector::from(make_client_config(&pki.ca_root_store));
	let tcp = TcpStream::connect(addr).await.unwrap();
	let server_name = ServerName::try_from("localhost").unwrap();
	// Under TLS 1.3 the server's "client cert required" alert can arrive
	// either during the handshake or on the first write/read, depending
	// on rustls internals. Accept either: handshake fails OR a write+read
	// roundtrip can't move bytes.
	match connector.connect(server_name, tcp).await {
		Err(_) => {} // Handshake-time rejection — expected.
		Ok(mut tls_stream) => {
			let _ = tls_stream
				.write_all(b"OPTIONS rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n\r\n")
				.await;
			let mut buf = [0u8; 1024];
			let res = tokio::time::timeout(Duration::from_secs(2), tls_stream.read(&mut buf))
				.await
				.expect("read deadline");
			assert!(
				matches!(res, Ok(0) | Err(_)),
				"server must drop connection: got {res:?}"
			);
		}
	}

	cancel.cancel();
}

#[tokio::test]
async fn request_mode_accepts_client_without_cert() {
	install_crypto();
	let pki = make_pki();
	let tls = TlsConfig::build(
		pki.server_chain,
		pki.server_key,
		ClientAuthMode::Request {
			roots: Arc::clone(&pki.ca_root_store),
		},
	)
	.expect("build TlsConfig");
	let (addr, cancel) = spawn_tls_server(tls).await;

	let connector = TlsConnector::from(make_client_config(&pki.ca_root_store));
	let tcp = TcpStream::connect(addr).await.unwrap();
	let server_name = ServerName::try_from("localhost").unwrap();
	let res = connector.connect(server_name, tcp).await;
	assert!(
		res.is_ok(),
		"handshake must succeed without client cert in Request mode: {res:?}"
	);

	cancel.cancel();
}

/// Mint an independent CA + client cert so we can drive a Require-mode
/// handshake with a client cert signed by the *wrong* CA.
fn make_wrong_ca_client_chain() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
	use rcgen::{BasicConstraints, IsCa, KeyUsagePurpose};

	let ca_kp = KeyPair::generate().unwrap();
	let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
	ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
	ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
	ca_params.distinguished_name = {
		let mut dn = rcgen::DistinguishedName::new();
		dn.push(rcgen::DnType::CommonName, "wrong-ca");
		dn
	};
	let ca = ca_params.self_signed(&ca_kp).unwrap();

	let client_kp = KeyPair::generate().unwrap();
	let client_params = CertificateParams::new(vec!["wrong-client".into()]).unwrap();
	let client_cert = client_params.signed_by(&client_kp, &ca, &ca_kp).unwrap();

	(
		vec![client_cert.der().clone()],
		PrivateKeyDer::Pkcs8(client_kp.serialize_der().into()),
	)
}

#[tokio::test]
async fn require_mode_rejects_client_cert_signed_by_wrong_ca() {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	install_crypto();
	let pki = make_pki();
	let tls = TlsConfig::build(
		pki.server_chain,
		pki.server_key,
		ClientAuthMode::Require {
			roots: Arc::clone(&pki.ca_root_store),
		},
	)
	.expect("build TlsConfig");
	let (addr, cancel) = spawn_tls_server(tls).await;

	// Client cert signed by an unrelated CA (not in the server's roots).
	// The realistic deployment scenario this defends: a stolen cert from a
	// sibling deployment whose CA we don't trust.
	let (wrong_chain, wrong_key) = make_wrong_ca_client_chain();
	let connector = TlsConnector::from(make_client_config_with_cert(
		&pki.ca_root_store,
		wrong_chain,
		wrong_key,
	));
	let tcp = TcpStream::connect(addr).await.unwrap();
	let server_name = ServerName::try_from("localhost").unwrap();
	// Same TLS-1.3 alert-timing tolerance as the no-cert case.
	match connector.connect(server_name, tcp).await {
		Err(_) => {} // Handshake-time rejection.
		Ok(mut tls_stream) => {
			let _ = tls_stream
				.write_all(b"OPTIONS rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n\r\n")
				.await;
			let mut buf = [0u8; 1024];
			let res = tokio::time::timeout(Duration::from_secs(2), tls_stream.read(&mut buf))
				.await
				.expect("read deadline");
			assert!(
				matches!(res, Ok(0) | Err(_)),
				"server must drop wrong-CA client: got {res:?}"
			);
		}
	}

	cancel.cancel();
}

#[tokio::test]
async fn request_mode_rejects_client_cert_signed_by_wrong_ca() {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	install_crypto();
	let pki = make_pki();
	let tls = TlsConfig::build(
		pki.server_chain,
		pki.server_key,
		ClientAuthMode::Request {
			roots: Arc::clone(&pki.ca_root_store),
		},
	)
	.expect("build TlsConfig");
	let (addr, cancel) = spawn_tls_server(tls).await;

	// Mirror of the Require-mode wrong-CA test: Request mode must
	// also reject a presented cert that fails to verify. The
	// difference between Request and Require is "is the cert
	// optional?" — neither mode is supposed to accept an INVALID
	// presented cert. Without this regression test, a future
	// `WebPkiClientVerifier` switch that silently swallows verify
	// failures in Request mode would go undetected.
	let (wrong_chain, wrong_key) = make_wrong_ca_client_chain();
	let connector = TlsConnector::from(make_client_config_with_cert(
		&pki.ca_root_store,
		wrong_chain,
		wrong_key,
	));
	let tcp = TcpStream::connect(addr).await.unwrap();
	let server_name = ServerName::try_from("localhost").unwrap();
	match connector.connect(server_name, tcp).await {
		Err(_) => {} // Handshake-time rejection.
		Ok(mut tls_stream) => {
			let _ = tls_stream
				.write_all(b"OPTIONS rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n\r\n")
				.await;
			let mut buf = [0u8; 1024];
			let res = tokio::time::timeout(Duration::from_secs(2), tls_stream.read(&mut buf))
				.await
				.expect("read deadline");
			assert!(
				matches!(res, Ok(0) | Err(_)),
				"server must drop wrong-CA client even in Request mode: got {res:?}"
			);
		}
	}

	cancel.cancel();
}

#[tokio::test]
async fn require_mode_accepts_valid_client_cert() {
	install_crypto();
	let pki = make_pki();
	let tls = TlsConfig::build(
		pki.server_chain,
		pki.server_key,
		ClientAuthMode::Require {
			roots: Arc::clone(&pki.ca_root_store),
		},
	)
	.expect("build TlsConfig");
	let (addr, cancel) = spawn_tls_server(tls).await;

	let connector = TlsConnector::from(make_client_config_with_cert(
		&pki.ca_root_store,
		pki.client_chain,
		pki.client_key,
	));
	let tcp = TcpStream::connect(addr).await.unwrap();
	let server_name = ServerName::try_from("localhost").unwrap();
	let res = connector.connect(server_name, tcp).await;
	assert!(
		res.is_ok(),
		"handshake with valid client cert must succeed: {res:?}"
	);

	cancel.cancel();
}

/// Send one RTSP request over the TLS stream and read the response head
/// (through the blank line). Panics on timeout or EOF before the head
/// completes — good enough for single-request assertions.
async fn tls_roundtrip<S>(stream: &mut S, request: &str) -> String
where
	S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	stream.write_all(request.as_bytes()).await.unwrap();
	let mut buf = Vec::new();
	let mut tmp = [0u8; 1024];
	loop {
		let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut tmp))
			.await
			.expect("timeout reading RTSP response over TLS")
			.expect("read error");
		assert!(n > 0, "EOF before response head completed");
		buf.extend_from_slice(&tmp[..n]);
		if buf.windows(4).any(|w| w == b"\r\n\r\n") {
			return String::from_utf8_lossy(&buf).into_owned();
		}
	}
}

#[tokio::test]
async fn tls_connection_offers_and_accepts_basic_auth() {
	// The counterpart to the plaintext test in rtsp_integration_test.rs:
	// on a TLS connection the password is protected in transit, so the
	// 401 challenge must offer Basic and a Basic header must be verified.
	install_crypto();
	let pki = make_pki();
	let tls = TlsConfig::build(pki.server_chain, pki.server_key, ClientAuthMode::None)
		.expect("build TlsConfig");
	let users = vec![UserCred {
		name: "alice".into(),
		password: "wonderland".into(),
	}];
	let (addr, cancel) = spawn_tls_server_with_users(tls, users).await;

	let connector = TlsConnector::from(make_client_config(&pki.ca_root_store));
	let tcp = TcpStream::connect(addr).await.unwrap();
	let server_name = ServerName::try_from("localhost").unwrap();
	let mut stream = connector.connect(server_name, tcp).await.unwrap();

	// 1. DESCRIBE without credentials → 401 offering Basic (and Digest).
	let head = tls_roundtrip(
		&mut stream,
		"DESCRIBE rtsps://localhost/cam1 RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n",
	)
	.await;
	assert!(head.contains("401"), "expected 401 challenge: {head}");
	let lower = head.to_ascii_lowercase();
	assert!(
		lower.contains("basic realm="),
		"TLS 401 must offer Basic: {head}"
	);
	assert!(
		lower.contains("digest realm="),
		"TLS 401 must still offer Digest: {head}"
	);

	// 2. DESCRIBE with valid Basic creds → authenticated. EmptyProvider
	// then reports the camera as unknown, so 404 (not 401/403) proves
	// verify_basic accepted the credentials. base64("alice:wonderland").
	let head = tls_roundtrip(
		&mut stream,
		"DESCRIBE rtsps://localhost/cam1 RTSP/1.0\r\nCSeq: 2\r\nAuthorization: Basic YWxpY2U6d29uZGVybGFuZA==\r\nAccept: application/sdp\r\n\r\n",
	)
	.await;
	assert!(
		!head.contains("401") && !head.contains("403"),
		"valid Basic over TLS must authenticate: {head}"
	);

	cancel.cancel();
}
