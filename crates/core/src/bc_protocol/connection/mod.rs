//! This module handles connections and subscribers
//!
//! This includes a tcp and udp connections. As well
//! as subscribers to binary streams that are encoded
//! in the bc packets.
//!
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;

mod bcconn;
mod bcsub;
#[cfg(test)]
mod chain;
pub(crate) mod discovery;
#[cfg(any(test, feature = "test-util"))]
pub mod mock;
mod tcpsource;
#[cfg(not(feature = "fuzz-api"))]
mod udpsource;
/// UDP send/recv flow primitives. Public only under the `fuzz-api`
/// feature so the out-of-tree fuzz harness can reach `UdpFlowState`
/// and `REORDER_CAP`; the production build keeps the module private.
#[cfg(feature = "fuzz-api")]
pub mod udpsource;

pub(crate) use self::{
	bcconn::BcConnection, bcconn::*, bcsub::BcSubscription, discovery::CameraDiscoverer,
	discovery::Discovery, tcpsource::TcpSource, udpsource::UdpSource,
};

#[derive(Debug)]
pub(crate) struct DiscoveryResult {
	socket: Arc<UdpSocket>,
	addr: SocketAddr,
	client_id: i32,
	camera_id: i32,
}

impl DiscoveryResult {
	/// Get the address discovered
	pub(crate) fn get_addr(&self) -> &SocketAddr {
		&self.addr
	}
}
