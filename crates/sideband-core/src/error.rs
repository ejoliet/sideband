//! Error taxonomy with the exit codes from the CLI contract.

/// Errors that map to a documented process exit code.
///
/// Anything not classified here surfaces as exit code 1.
#[derive(Debug, thiserror::Error)]
pub enum SidebandError {
    /// Missing token, unreadable config, bad Slack auth.
    #[error("config/auth: {0}")]
    Config(String),

    /// Relay unreachable, hole punching failed, endpoint could not bind.
    #[error("network/relay: {0}")]
    Network(String),

    /// BLAKE3 verification failed. Partial data is deleted before this is raised.
    #[error("hash mismatch for {hash}: unverified bytes discarded")]
    HashMismatch { hash: String },

    /// Ticket TTL elapsed.
    #[error("ticket expired at unix {0}")]
    TicketExpired(u64),
}

impl SidebandError {
    /// Exit code from the CLI contract: 2 config/auth, 3 network, 4 hash, 5 expiry.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Config(_) => 2,
            Self::Network(_) => 3,
            Self::HashMismatch { .. } => 4,
            Self::TicketExpired(_) => 5,
        }
    }
}

/// Exit code for any error, walking the `anyhow` chain for a classified cause.
pub fn exit_code_of(err: &anyhow::Error) -> i32 {
    err.chain()
        .find_map(|e| e.downcast_ref::<SidebandError>())
        .map_or(1, SidebandError::exit_code)
}
