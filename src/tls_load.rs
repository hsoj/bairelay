//! Load and parse the TLS PEMs referenced by [`Config`].
//!
//! All file I/O happens here so `main.rs` only sees a parsed
//! [`crate::rtsp::server::TlsConfig`]. Errors carry the offending path
//! so operators do not have to grep their config to find what bairelay
//! tried to open.

use std::fs;
use std::io::Cursor;
use std::sync::Arc;

use crate::rtsp::server::{ClientAuthMode, TlsConfig};
use anyhow::{anyhow, Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::RootCertStore;

use crate::config::TlsClientAuth;

/// Parsed TLS material ready to hand to `crate::rtsp::server::ServerConfig`.
#[derive(Debug)]
pub struct LoadedTls {
	pub tls_config: TlsConfig,
}

/// Read and parse the server PEM, optionally read+parse the client-CA
/// PEM, then build the rustls server config.
pub fn load_server_tls(
	cert_path: &str,
	tls_client_auth: TlsClientAuth,
	client_ca_path: Option<&str>,
) -> Result<LoadedTls> {
	let pem =
		fs::read(cert_path).with_context(|| format!("failed to read TLS cert PEM: {cert_path}"))?;
	let mut chain: Vec<CertificateDer<'static>> = Vec::new();
	let mut key: Option<PrivateKeyDer<'static>> = None;
	for item in rustls_pemfile::read_all(&mut Cursor::new(&pem)) {
		let parsed = item.with_context(|| format!("PEM parse error in {cert_path}"))?;
		match parsed {
			rustls_pemfile::Item::X509Certificate(c) => chain.push(c),
			rustls_pemfile::Item::Pkcs1Key(k) => {
				set_key_or_reject(&mut key, PrivateKeyDer::Pkcs1(k), cert_path)?
			}
			rustls_pemfile::Item::Pkcs8Key(k) => {
				set_key_or_reject(&mut key, PrivateKeyDer::Pkcs8(k), cert_path)?
			}
			rustls_pemfile::Item::Sec1Key(k) => {
				set_key_or_reject(&mut key, PrivateKeyDer::Sec1(k), cert_path)?
			}
			_ => {}
		}
	}
	if chain.is_empty() {
		return Err(anyhow!("TLS cert PEM has no certificate: {cert_path}"));
	}
	let key = key.ok_or_else(|| anyhow!("TLS cert PEM has no private key: {cert_path}"))?;

	let client_auth = match tls_client_auth {
		TlsClientAuth::None => ClientAuthMode::None,
		other => {
			let ca_path = client_ca_path.ok_or_else(|| {
				anyhow!(
					"internal: tls_client_auth requires tls_client_ca; \
					 should have been caught by config validation"
				)
			})?;
			let roots = load_ca_roots(ca_path)?;
			match other {
				TlsClientAuth::Request => ClientAuthMode::Request { roots },
				TlsClientAuth::Require => ClientAuthMode::Require { roots },
				TlsClientAuth::None => unreachable!(),
			}
		}
	};

	let tls_config = TlsConfig::build(chain, key, client_auth)
		.with_context(|| format!("invalid TLS config (cert {cert_path})"))?;
	Ok(LoadedTls { tls_config })
}

/// Reject a PEM bundle that carries more than one private key.
///
/// `cat key1.pem key2.pem cert.pem` would otherwise silently let the
/// last key parsed become the server key — likely mismatched against
/// the cert chain, surfacing as an opaque rustls "key does not match
/// certificate" error at startup. Fail fast with the operator's path.
fn set_key_or_reject(
	slot: &mut Option<PrivateKeyDer<'static>>,
	new_key: PrivateKeyDer<'static>,
	cert_path: &str,
) -> Result<()> {
	if slot.is_some() {
		return Err(anyhow!(
			"TLS cert PEM contains more than one private key: {cert_path}"
		));
	}
	*slot = Some(new_key);
	Ok(())
}

fn load_ca_roots(ca_path: &str) -> Result<Arc<RootCertStore>> {
	let pem =
		fs::read(ca_path).with_context(|| format!("failed to read tls_client_ca: {ca_path}"))?;
	let mut roots = RootCertStore::empty();
	let mut found = 0usize;
	for item in rustls_pemfile::certs(&mut Cursor::new(&pem)) {
		let der: CertificateDer<'static> =
			item.with_context(|| format!("PEM parse error in {ca_path}"))?;
		roots
			.add(der)
			.with_context(|| format!("rustls rejected CA cert in {ca_path}"))?;
		found += 1;
	}
	if found == 0 {
		return Err(anyhow!("tls_client_ca PEM has no certificates: {ca_path}"));
	}
	Ok(Arc::new(roots))
}

#[cfg(test)]
mod tests {
	use super::*;
	use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
	use std::io::Write;
	use tempfile::NamedTempFile;

	fn install_crypto() {
		// Forward to the shared `crate::rtsp::server::install_crypto_provider`
		// helper so the OnceLock + Err-swallow semantics live in exactly one
		// place across the workspace.
		crate::rtsp::server::install_crypto_provider();
	}

	fn write_temp_pem(contents: &[u8]) -> NamedTempFile {
		let mut f = NamedTempFile::new().unwrap();
		f.write_all(contents).unwrap();
		f.flush().unwrap();
		f
	}

	fn ca_and_server_bundle() -> (String, String) {
		// (server-bundle.pem, ca.pem)
		let ca_kp = KeyPair::generate().unwrap();
		let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
		ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
		ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
		let ca = ca_params.self_signed(&ca_kp).unwrap();

		let server_kp = KeyPair::generate().unwrap();
		let server_params = CertificateParams::new(vec!["localhost".into()]).unwrap();
		let server = server_params.signed_by(&server_kp, &ca, &ca_kp).unwrap();

		let server_bundle = format!("{}{}", server.pem(), server_kp.serialize_pem());
		let ca_pem = ca.pem();
		(server_bundle, ca_pem)
	}

	#[test]
	fn load_server_tls_happy_path_no_client_auth() {
		install_crypto();
		let (bundle, _ca) = ca_and_server_bundle();
		let bundle_file = write_temp_pem(bundle.as_bytes());
		let res = load_server_tls(
			bundle_file.path().to_str().unwrap(),
			TlsClientAuth::None,
			None,
		);
		assert!(res.is_ok(), "load failed: {res:?}");
	}

	#[test]
	fn load_server_tls_happy_path_require_client_auth() {
		install_crypto();
		let (bundle, ca) = ca_and_server_bundle();
		let bundle_file = write_temp_pem(bundle.as_bytes());
		let ca_file = write_temp_pem(ca.as_bytes());
		let res = load_server_tls(
			bundle_file.path().to_str().unwrap(),
			TlsClientAuth::Require,
			Some(ca_file.path().to_str().unwrap()),
		);
		assert!(res.is_ok(), "load failed: {res:?}");
	}

	#[test]
	fn load_server_tls_missing_cert_file() {
		install_crypto();
		let res = load_server_tls("/nonexistent/path.pem", TlsClientAuth::None, None);
		let err = res.expect_err("must fail").to_string();
		assert!(err.contains("failed to read TLS cert PEM"), "got: {err}");
	}

	#[test]
	fn load_server_tls_pem_without_certificate() {
		install_crypto();
		let server_kp = KeyPair::generate().unwrap();
		let key_only = server_kp.serialize_pem();
		let f = write_temp_pem(key_only.as_bytes());
		let res = load_server_tls(f.path().to_str().unwrap(), TlsClientAuth::None, None);
		let err = res.expect_err("must fail").to_string();
		assert!(err.contains("no certificate"), "got: {err}");
	}

	#[test]
	fn load_server_tls_pem_without_private_key() {
		install_crypto();
		// Mint a fresh server cert and write ONLY the cert (no key) so
		// the failure mode under test is the operator's "I forgot to
		// concatenate the key into the bundle" mistake, not "I fed the
		// CA cert by accident."
		let ca_kp = KeyPair::generate().unwrap();
		let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
		ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
		ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
		let ca = ca_params.self_signed(&ca_kp).unwrap();
		let server_kp = KeyPair::generate().unwrap();
		let server_params = CertificateParams::new(vec!["localhost".into()]).unwrap();
		let server = server_params.signed_by(&server_kp, &ca, &ca_kp).unwrap();

		let cert_only = server.pem();
		let f = write_temp_pem(cert_only.as_bytes());
		let res = load_server_tls(f.path().to_str().unwrap(), TlsClientAuth::None, None);
		let err = res.expect_err("must fail").to_string();
		assert!(err.contains("no private key"), "got: {err}");
	}

	#[test]
	fn load_server_tls_rejects_multiple_keys_in_bundle() {
		install_crypto();
		// `cat key1.pem key2.pem cert.pem` → previously last-key-wins,
		// silently mismatched against the cert. Now an explicit error.
		let (mut bundle, _ca) = ca_and_server_bundle();
		let stray_kp = KeyPair::generate().unwrap();
		bundle.push_str(&stray_kp.serialize_pem());
		let f = write_temp_pem(bundle.as_bytes());
		let res = load_server_tls(f.path().to_str().unwrap(), TlsClientAuth::None, None);
		let err = res.expect_err("must fail").to_string();
		assert!(err.contains("more than one private key"), "got: {err}");
	}

	#[test]
	fn load_server_tls_client_ca_empty() {
		install_crypto();
		let (bundle, _ca) = ca_and_server_bundle();
		let bundle_file = write_temp_pem(bundle.as_bytes());
		// Write a non-PEM file so certs() yields nothing.
		let empty = write_temp_pem(b"# nothing here\n");
		let res = load_server_tls(
			bundle_file.path().to_str().unwrap(),
			TlsClientAuth::Request,
			Some(empty.path().to_str().unwrap()),
		);
		let err = res.expect_err("must fail").to_string();
		assert!(err.contains("no certificates"), "got: {err}");
	}
}
