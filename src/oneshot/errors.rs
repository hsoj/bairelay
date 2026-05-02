use thiserror::Error;

#[derive(Debug, Error)]
#[error("config error: {0}")]
pub struct ConfigError(String);

impl ConfigError {
	pub fn new(msg: impl Into<String>) -> Self {
		Self(msg.into())
	}
}

#[derive(Debug, Error)]
#[error("usage error: {0}")]
pub struct UsageError(String);

impl UsageError {
	pub fn new(msg: impl Into<String>) -> Self {
		Self(msg.into())
	}
}

#[derive(Debug, Error)]
#[error("interrupted")]
pub struct InterruptedError;

impl InterruptedError {
	// Canonical constructor. We deliberately do not derive `Default`
	// to avoid two parallel constructors (`new()` vs `default()`)
	// for a unit struct.
	#[allow(clippy::new_without_default)]
	pub fn new() -> Self {
		Self
	}
}
