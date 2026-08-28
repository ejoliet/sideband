//! The sideband ticket: an iroh blob ticket plus the file metadata and expiry
//! that the receiver needs before it dials anyone.

use std::{
    fmt,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context};
use data_encoding::BASE32_NOPAD;
use iroh_blobs::ticket::BlobTicket;
use serde::{Deserialize, Serialize};

use crate::error::SidebandError;

/// Human-visible prefix so a ticket is recognisable when pasted into Slack.
pub const TICKET_PREFIX: &str = "sideband";

/// What the receiver shows before downloading, and names the output file with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    /// File name only, never a path — the receiver chooses the directory.
    pub name: String,
    /// Size in bytes, for the Slack card and the progress display.
    pub size: u64,
}

/// A capability token: anyone holding it can fetch the blob until it expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// Endpoint address, hash and format of the blob.
    pub blob: BlobTicket,
    /// File name and size.
    pub meta: FileMeta,
    /// Unix seconds after which receivers must refuse. `None` never expires.
    pub expires_at: Option<u64>,
}

/// Single-variant enum so postcard writes a discriminator we can extend later.
#[derive(Serialize, Deserialize)]
enum Wire {
    V0 {
        blob: String,
        name: String,
        size: u64,
        expires_at: Option<u64>,
    },
}

impl Ticket {
    /// Builds a ticket, converting a TTL in hours into an absolute expiry.
    pub fn new(blob: BlobTicket, meta: FileMeta, ttl_hours: u32) -> Self {
        let expires_at = (ttl_hours > 0).then(|| now_unix() + u64::from(ttl_hours) * 3600);
        Self {
            blob,
            meta,
            expires_at,
        }
    }

    /// Short hash for the Slack card. Not enough to fetch with, only to eyeball.
    pub fn short_hash(&self) -> String {
        self.blob.hash().to_string().chars().take(12).collect()
    }

    /// Seconds until expiry, or `None` for a ticket that never expires.
    pub fn remaining_secs(&self) -> Option<u64> {
        self.expires_at.map(|at| at.saturating_sub(now_unix()))
    }

    /// `Err(TicketExpired)` once the TTL has elapsed.
    pub fn check_fresh(&self) -> Result<(), SidebandError> {
        match self.expires_at {
            Some(at) if now_unix() >= at => Err(SidebandError::TicketExpired(at)),
            _ => Ok(()),
        }
    }
}

impl fmt::Display for Ticket {
    /// `sideband` + base32 of the postcard encoding, lowercase and unpadded.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wire = Wire::V0 {
            blob: self.blob.to_string(),
            name: self.meta.name.clone(),
            size: self.meta.size,
            expires_at: self.expires_at,
        };
        let bytes = postcard::to_stdvec(&wire).map_err(|_| fmt::Error)?;
        write!(
            f,
            "{TICKET_PREFIX}{}",
            BASE32_NOPAD.encode(&bytes).to_lowercase()
        )
    }
}

impl FromStr for Ticket {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let Some(body) = s.strip_prefix(TICKET_PREFIX) else {
            bail!("not a sideband ticket: expected it to start with `{TICKET_PREFIX}`");
        };
        let bytes = BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .context("ticket is not valid base32")?;
        let Wire::V0 {
            blob,
            name,
            size,
            expires_at,
        } = postcard::from_bytes(&bytes).context("ticket payload is malformed")?;
        Ok(Self {
            blob: blob
                .parse()
                .context("ticket carries an invalid blob ticket")?,
            meta: FileMeta { name, size },
            expires_at,
        })
    }
}

/// Current time in unix seconds. Clamps to 0 for clocks set before 1970.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Formats bytes for humans, e.g. `1.4 GiB`.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use iroh::{EndpointAddr, SecretKey};
    use iroh_blobs::{BlobFormat, Hash};

    use super::*;

    fn sample_blob_ticket() -> BlobTicket {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4433".parse().unwrap());
        BlobTicket::new(addr, Hash::new(b"payload"), BlobFormat::Raw)
    }

    #[test]
    fn round_trips_through_compact_encoding() {
        let ticket = Ticket::new(
            sample_blob_ticket(),
            FileMeta {
                name: "roman-l2.tar".into(),
                size: 53_687_091_200,
            },
            24,
        );
        let encoded = ticket.to_string();
        assert!(encoded.starts_with(TICKET_PREFIX));
        assert_eq!(encoded.parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn rejects_foreign_and_corrupt_tickets() {
        assert!("blobaaaa".parse::<Ticket>().is_err());
        assert!(format!("{TICKET_PREFIX}not-base32!")
            .parse::<Ticket>()
            .is_err());
    }

    #[test]
    fn expiry_is_enforced_by_check_fresh() {
        let mut ticket = Ticket::new(
            sample_blob_ticket(),
            FileMeta {
                name: "a".into(),
                size: 1,
            },
            1,
        );
        assert!(ticket.check_fresh().is_ok());
        ticket.expires_at = Some(now_unix() - 1);
        assert!(matches!(
            ticket.check_fresh(),
            Err(SidebandError::TicketExpired(_))
        ));
        ticket.expires_at = None;
        assert!(ticket.check_fresh().is_ok());
    }

    #[test]
    fn human_bytes_picks_a_unit() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(53_687_091_200), "50.0 GiB");
    }
}
