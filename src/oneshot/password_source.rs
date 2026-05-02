//! Resolve the password value for `bairelay users add` /
//! `bairelay users password`.
//!
//! Operators can pass the password as a positional argv (legacy /
//! drop-in compat) or omit it, in which case bairelay prompts on a
//! TTY (no-echo, like `passwd`) or reads a single line from stdin
//! when stdin is piped (CI / vault integrations).
//!
//! Keeping the secret out of `ps auxww`, shell history, and
//! supervisor argv-capture logs is the only goal — this file
//! intentionally has no other policy (no double-confirm, no
//! complexity rules, no env-var fallback). Add those if a real
//! deployment needs them; today bairelay is single-operator.

use anyhow::{Context, Result};
use std::io::IsTerminal;

use crate::oneshot::errors::UsageError;

/// Default prompt for `users add`.
pub const PROMPT_ADD: &str = "New user password: ";
/// Default prompt for `users password`.
pub const PROMPT_PASSWORD: &str = "New password: ";

/// Resolve the operator's password. If `provided` is `Some`, validate
/// non-empty and return it as-is (legacy positional path). If `None`,
/// prompt on a TTY or read one line from stdin.
///
/// Errors are typed as [`UsageError`] so the exit-code classifier
/// maps them to `EXIT_USAGE = 2` — empty / blank input is operator
/// pilot error, not a programmer bug.
pub fn resolve(provided: Option<String>, prompt: &str) -> Result<String> {
	if let Some(p) = provided {
		if p.is_empty() {
			return Err(UsageError::new("password must not be empty").into());
		}
		return Ok(p);
	}
	let raw = if std::io::stdin().is_terminal() {
		rpassword::prompt_password(prompt).context("read password from terminal")?
	} else {
		let mut line = String::new();
		std::io::stdin()
			.read_line(&mut line)
			.context("read password from stdin")?;
		line
	};
	let trimmed = raw.trim_end_matches(['\r', '\n']).to_string();
	if trimmed.is_empty() {
		return Err(UsageError::new("password must not be empty").into());
	}
	Ok(trimmed)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn provided_some_passes_through() {
		assert_eq!(
			resolve(Some("hunter2".into()), "ignored").unwrap(),
			"hunter2"
		);
	}

	#[test]
	fn provided_some_empty_rejected_as_usage() {
		let err = resolve(Some(String::new()), "ignored").unwrap_err();
		let chain: Vec<_> = err.chain().collect();
		assert!(
			chain.iter().any(|c| c.is::<UsageError>()),
			"empty positional should classify as usage error"
		);
	}

	// `provided = None` triggers TTY/stdin reads which are environment-
	// dependent (cargo test typically pipes stdin from `/dev/null`).
	// Direct integration coverage of the prompt path lives in the
	// oneshot CLI subprocess tests in tests/cli_oneshot_test.rs, where
	// stdin can be controlled.
}
