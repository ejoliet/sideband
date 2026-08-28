//! `sideband doctor`: is this machine able to send and receive?

use sideband_core::{
    config::{self, Config},
    slack,
    ticket::{human_bytes, Ticket},
    transfer::{self, Relays},
    SidebandError,
};

/// Runs every check, printing as it goes, and fails with the first real
/// problem's exit code so scripts can branch on it.
pub async fn run(cfg: &Config, ticket: Option<&str>) -> anyhow::Result<()> {
    let mut problem: Option<SidebandError> = None;

    let path = config::config_path();
    println!(
        "config      {} {}",
        if path.exists() { "ok" } else { "--" },
        path.display()
    );
    println!("store       ok {}", config::store_dir().display());

    match check_iroh(cfg).await {
        Ok(summary) => println!("iroh        ok {summary}"),
        Err(err) => {
            println!("iroh        FAIL {err}");
            problem = Some(err);
        }
    }

    match check_slack(cfg).await {
        Ok(summary) => println!("slack       {summary}"),
        Err(err) => {
            println!("slack       FAIL {err}");
            problem = problem.or(Some(err));
        }
    }

    if let Some(raw) = ticket {
        match check_ticket(raw) {
            Ok(summary) => println!("ticket      ok {summary}"),
            Err(err) => {
                println!("ticket      FAIL {err}");
                problem = problem.or(Some(err));
            }
        }
    }

    match problem {
        None => {
            println!("\nall good — `sideband send <file>` should work");
            Ok(())
        }
        Some(err) => Err(err.into()),
    }
}

/// Binds an endpoint and reports the addresses a peer could dial.
///
/// A home relay plus at least one direct address is the healthy case: direct
/// addresses are what hole punching upgrades to, the relay is the fallback.
async fn check_iroh(cfg: &Config) -> Result<String, SidebandError> {
    let relays = Relays::from_config(cfg.relay_url.as_deref());
    let addr = transfer::probe_endpoint(&relays).await.map_err(|err| {
        match err.downcast::<SidebandError>() {
            Ok(known) => known,
            Err(other) => SidebandError::Network(other.to_string()),
        }
    })?;

    let direct = addr.ip_addrs().count();
    let relay = addr
        .relay_urls()
        .next()
        .map_or_else(|| "no relay".to_string(), |url| url.to_string());
    if direct == 0 {
        return Err(SidebandError::Network(
            "no direct addresses; hole punching cannot work from this network".into(),
        ));
    }
    Ok(format!(
        "{} direct address{}, relay {relay}",
        direct,
        if direct == 1 { "" } else { "es" }
    ))
}

/// Slack is optional, so a missing token is reported but not fatal.
async fn check_slack(cfg: &Config) -> Result<String, SidebandError> {
    let Ok(token) = cfg.bot_token() else {
        return Ok(
            "-- SLACK_BOT_TOKEN not set (tickets still work; cards will not be posted)".into(),
        );
    };
    let client = slack::client()
        .map_err(|err| SidebandError::Config(format!("could not build a Slack client: {err}")))?;
    let who = slack::auth_test(&client, token).await?;
    let app = if cfg.app_token().is_ok() {
        "app token set"
    } else {
        "no app token, `sideband bot` unavailable"
    };
    Ok(format!("ok {who} ({app})"))
}

fn check_ticket(raw: &str) -> Result<String, SidebandError> {
    let ticket: Ticket = raw
        .parse()
        .map_err(|err| SidebandError::Config(format!("{err:#}")))?;
    ticket.check_fresh()?;
    Ok(format!(
        "{} ({}), {}",
        ticket.meta.name,
        human_bytes(ticket.meta.size),
        match ticket.remaining_secs() {
            Some(secs) => format!("{secs}s left"),
            None => "no expiry".to_string(),
        }
    ))
}
