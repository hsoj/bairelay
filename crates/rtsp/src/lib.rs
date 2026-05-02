//! Bairelay RTSP primitives.
//!
//! Pure-logic library: codec packetizers, transcoders, RTSP message
//! handling, SDP generation, authentication, session state, last-frame
//! buffer, and the `StreamProvider` trait. No I/O or server runtime —
//! that lives in the server runtime layer.

pub mod buffer;
pub mod codec;
pub mod provider;
pub mod rtp;
pub mod rtsp;
pub mod sdp;
pub mod server;
pub mod transcode;
pub mod url;
