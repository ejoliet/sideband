//! Core of `sideband`: Slack carries the intent, iroh carries the bytes.
//!
//! Slack only ever sees a ticket. The payload moves directly between peers over
//! iroh QUIC connections, content-addressed and BLAKE3-verified.

pub mod config;
pub mod error;
pub mod slack;
pub mod ticket;
pub mod transfer;

pub use config::Config;
pub use error::SidebandError;
pub use ticket::Ticket;
