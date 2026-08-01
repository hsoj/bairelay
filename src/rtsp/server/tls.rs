//! TLS configuration for the RTSP server.
//!
//! Wraps an `Arc<rustls::ServerConfig>` so it builds once at config-load
//! time and clones cheaply into the listener task. The listener wraps each
//! accepted TCP socket via `tokio_rustls::TlsAcceptor`.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::RootCertStore;

#[derive(thiserror::Error, Debug)]
pub enum TlsConfigError {
	#[error("certificate chain is empty")]
	EmptyCertChain,
	#[error("client_auth roots store is empty")]
	EmptyClientAuthRoots,
	#[error("rustls rejected the cert/key pair: {0}")]
	Rustls(#[from] rustls::Error),
	#[error("client cert verifier build failed: {0}")]
	VerifierBuild(#[from] rustls::server::VerifierBuilderError),
}

/// Client-certificate authentication mode.
///
/// `None` accepts any TLS-capable client; `Request` lets unauthenticated
/// clients in but verifies any cert that is presented; `Require` rejects
/// the handshake unless the client presents a cert chain that validates
/// against `roots`.
#[derive(Clone)]
pub enum ClientAuthMode {
	None,
	Request { roots: Arc<RootCertStore> },
	Require { roots: Arc<RootCertStore> },
}

#[derive(Clone)]
pub struct TlsConfig {
	pub server_config: Arc<rustls::ServerConfig>,
}

impl std::fmt::Debug for TlsConfig {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("TlsConfig")
			.field("alpn_protocols", &self.server_config.alpn_protocols.len())
			.finish_non_exhaustive()
	}
}

impl TlsConfig {
	/// Build a `TlsConfig` from already-parsed cert chain + key.
	///
	/// Performs the full rustls build at call time so misconfiguration
	/// surfaces at config-load rather than on the first connection.
	pub fn build(
		cert_chain: Vec<CertificateDer<'static>>,
		key: PrivateKeyDer<'static>,
		client_auth: ClientAuthMode,
	) -> Result<Self, TlsConfigError> {
		if cert_chain.is_empty() {
			return Err(TlsConfigError::EmptyCertChain);
		}

		let builder = rustls::ServerConfig::builder();

		let server_config = match client_auth {
			ClientAuthMode::None => builder
				.with_no_client_auth()
				.with_single_cert(cert_chain, key)?,
			ClientAuthMode::Request { roots } => {
				if roots.is_empty() {
					return Err(TlsConfigError::EmptyClientAuthRoots);
				}
				let verifier = WebPkiClientVerifier::builder(roots)
					.allow_unauthenticated()
					.build()?;
				builder
					.with_client_cert_verifier(verifier)
					.with_single_cert(cert_chain, key)?
			}
			ClientAuthMode::Require { roots } => {
				if roots.is_empty() {
					return Err(TlsConfigError::EmptyClientAuthRoots);
				}
				let verifier = WebPkiClientVerifier::builder(roots).build()?;
				builder
					.with_client_cert_verifier(verifier)
					.with_single_cert(cert_chain, key)?
			}
		};

		Ok(Self {
			server_config: Arc::new(server_config),
		})
	}
}

/// Install the rustls aws-lc-rs default crypto provider once per
/// process. Idempotent: a second call is a no-op (rustls returns
/// `Err(CryptoProvider)` if a provider is already installed; the
/// helper swallows it). Safe to call from production code (the
/// `OnceLock` makes multi-bring-up paths a no-op) AND from every
/// integration test sharing the cargo test binary.
///
/// Single canonical entry point — replaces three near-identical
/// copies in `tls.rs` / `tls_load.rs` / `tls_handshake.rs`.
pub fn install_crypto_provider() {
	use std::sync::OnceLock;
	static INIT: OnceLock<()> = OnceLock::new();
	INIT.get_or_init(|| {
		// install_default returns Err if a provider is already
		// installed — fine in repeated test runs sharing a process,
		// or if some upstream initialiser ran first.
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	});
}

#[cfg(test)]
fn init_crypto_for_tests() {
	install_crypto_provider();
}

#[cfg(test)]
mod tests {
	use super::*;
	use rcgen::{CertificateParams, KeyPair};

	fn self_signed() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
		let kp = KeyPair::generate().unwrap();
		let params = CertificateParams::new(vec!["localhost".into()]).unwrap();
		let cert = params.self_signed(&kp).unwrap();
		let cert_der: CertificateDer<'static> = cert.der().clone();
		let key_der: PrivateKeyDer<'static> = PrivateKeyDer::Pkcs8(kp.serialize_der().into());
		(vec![cert_der], key_der)
	}

	#[test]
	fn build_accepts_self_signed_no_client_auth() {
		init_crypto_for_tests();
		let (chain, key) = self_signed();
		let cfg = TlsConfig::build(chain, key, ClientAuthMode::None).expect("build must succeed");
		assert!(!cfg.server_config.crypto_provider().cipher_suites.is_empty());
	}

	#[test]
	fn build_rejects_empty_cert_chain() {
		init_crypto_for_tests();
		let (_, key) = self_signed();
		let err = TlsConfig::build(vec![], key, ClientAuthMode::None)
			.expect_err("empty chain must reject");
		assert!(matches!(err, TlsConfigError::EmptyCertChain));
	}

	#[test]
	fn build_rejects_empty_request_roots() {
		init_crypto_for_tests();
		let (chain, key) = self_signed();
		let roots = Arc::new(RootCertStore::empty());
		let err = TlsConfig::build(chain, key, ClientAuthMode::Request { roots })
			.expect_err("empty roots must reject");
		assert!(matches!(err, TlsConfigError::EmptyClientAuthRoots));
	}

	#[test]
	fn build_rejects_empty_require_roots() {
		init_crypto_for_tests();
		let (chain, key) = self_signed();
		let roots = Arc::new(RootCertStore::empty());
		let err = TlsConfig::build(chain, key, ClientAuthMode::Require { roots })
			.expect_err("empty roots must reject");
		assert!(matches!(err, TlsConfigError::EmptyClientAuthRoots));
	}

	#[test]
	fn build_accepts_require_with_one_root() {
		init_crypto_for_tests();
		let (chain, key) = self_signed();
		let mut roots = RootCertStore::empty();
		roots.add(chain[0].clone()).unwrap();
		let roots = Arc::new(roots);
		let _cfg = TlsConfig::build(chain, key, ClientAuthMode::Require { roots })
			.expect("require with roots must succeed");
	}

	#[test]
	fn build_accepts_request_with_one_root() {
		init_crypto_for_tests();
		let (chain, key) = self_signed();
		let mut roots = RootCertStore::empty();
		roots.add(chain[0].clone()).unwrap();
		let roots = Arc::new(roots);
		let _cfg = TlsConfig::build(chain, key, ClientAuthMode::Request { roots })
			.expect("request with roots must succeed");
	}
}
