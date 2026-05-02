//! Server runtime: TCP listener, per-connection tasks, TCP-interleaved
//! and UDP transports, session registry, RTCP sender reports.

pub mod connection;
pub mod listener;
pub mod packetizer;
pub mod registry;
pub mod rtcp;
pub mod session_task;
pub mod tls;
pub mod transport;
pub mod udp_pool;

pub use registry::SessionRegistry;

pub use listener::{RtspServer, ServerConfig};
pub use tls::{install_crypto_provider, ClientAuthMode, TlsConfig, TlsConfigError};
