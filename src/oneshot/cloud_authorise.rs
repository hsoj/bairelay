//! `cloud-authorise` — one-time interactive MFA bootstrap for cloud cameras.
//!
//! A host whose outbound IP Reolink doesn't recognise gets `8208 mfa_required`
//! on the cloud token mint. This command clears that once: it triggers the
//! chosen verification method, reads the code, completes the verified login,
//! and writes the ~30-day `mfa_trust_token` to `config-cloud-auth.json` beside
//! the config. The connect path then replays it (see
//! [`crate::config::apply_cloud_auth`] + `cloud::mint_bundle`) until it lapses,
//! at which point this is re-run.

use crate::config::{Config, CLOUD_AUTH_FILE};
use crate::oneshot::classify::{EXIT_CONNECTION, EXIT_OK, EXIT_USAGE};
use bairelay_neolink_core::cloud::{self, CloudAuth};
use std::io::Write;
use std::path::Path;

/// Verification methods the bootstrap accepts.
pub const METHODS: &[&str] = &["email", "totp", "backup_code"];

/// Current unix time in seconds.
fn now_unix() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_secs() as i64)
		.unwrap_or(0)
}

/// Serialise `auth` to `path` as JSON, owner-read/write only (`0600` on Unix —
/// the trust token is a ~30-day account credential).
pub fn write_auth(path: &Path, auth: &CloudAuth) -> std::io::Result<()> {
	let json = serde_json::to_string_pretty(auth).map_err(std::io::Error::other)?;
	std::fs::write(path, json)?;
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
	}
	Ok(())
}

/// Read one trimmed line from stdin after printing `label`.
fn prompt(label: &str) -> String {
	print!("{label}");
	let _ = std::io::stdout().flush();
	let mut line = String::new();
	let _ = std::io::stdin().read_line(&mut line);
	line.trim().to_string()
}

/// Run the interactive bootstrap, returning a process exit code. Account
/// credentials come from the config root; `method` is one of [`METHODS`].
pub async fn run(config_path: &Path, config: &Config, method: &str) -> i32 {
	if !METHODS.contains(&method) {
		eprintln!("cloud-authorise: unknown --method '{method}' (use one of {METHODS:?})");
		return EXIT_USAGE;
	}
	let (account, password) = match (
		config.cloud_account.as_deref(),
		config.cloud_password.as_deref(),
	) {
		(Some(a), Some(p)) if !a.is_empty() && !p.is_empty() => (a, p),
		_ => {
			eprintln!("cloud-authorise: set cloud_account and cloud_password in the config first");
			return EXIT_USAGE;
		}
	};

	// Drive the bootstrap; the closure prints the method-specific prompt
	// (after the code request, since `email` mails it) and reads the code.
	let auth = match cloud::mfa_bootstrap(account, password, method, |m| {
		match m {
			"email" => println!("A verification code was emailed to {account}."),
			"totp" => println!("Open your authenticator app for the current code."),
			_ => println!("Use one of your saved backup codes."),
		}
		prompt("Enter the code: ")
	})
	.await
	{
		Ok(a) => a,
		Err(e) => {
			eprintln!("cloud-authorise: {e}");
			return EXIT_CONNECTION;
		}
	};
	let path = config_path.with_file_name(CLOUD_AUTH_FILE);
	if let Err(e) = write_auth(&path, &auth) {
		eprintln!("cloud-authorise: could not write {}: {e}", path.display());
		return EXIT_CONNECTION;
	}
	let days = ((auth.expiry_unix_s - now_unix()) / 86_400).max(0);
	println!(
		"Authorised. Trust token stored at {} (valid ~{days} days). Cloud cameras \
		 on this host will now connect without prompting.",
		path.display()
	);
	EXIT_OK
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn now_unix_is_a_recent_timestamp() {
		// Sanity-pins the helper used to report the trust token's lifetime.
		assert!(now_unix() > 1_700_000_000);
	}

	#[test]
	fn write_auth_round_trips_and_is_owner_only() {
		let dir = std::env::temp_dir().join(format!("bairelay-ca-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join(CLOUD_AUTH_FILE);
		let auth = CloudAuth {
			account: "you@example.com".into(),
			mfa_trust_token: Some("TRUST".into()),
			refresh_token: Some("REFRESH".into()),
			expiry_unix_s: 4_102_444_800,
		};
		write_auth(&path, &auth).unwrap();
		let back: CloudAuth =
			serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
		assert_eq!(back.account, "you@example.com");
		assert_eq!(back.mfa_trust_token.as_deref(), Some("TRUST"));
		assert_eq!(back.refresh_token.as_deref(), Some("REFRESH"));
		assert_eq!(back.expiry_unix_s, 4_102_444_800);
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			let mode = std::fs::metadata(&path).unwrap().permissions().mode();
			assert_eq!(mode & 0o777, 0o600, "trust-token file must be owner-only");
		}
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[tokio::test]
	async fn run_rejects_unknown_method_without_network() {
		let cfg = Config::default();
		let code = run(
			Path::new("/nonexistent/config.toml"),
			&cfg,
			"carrier-pigeon",
		)
		.await;
		assert_eq!(code, EXIT_USAGE);
	}

	#[tokio::test]
	async fn run_rejects_missing_account_without_network() {
		// Valid method, but no cloud_account/password -> usage error before
		// any network call.
		let cfg = Config::default();
		let code = run(Path::new("/nonexistent/config.toml"), &cfg, "email").await;
		assert_eq!(code, EXIT_USAGE);
	}
}
