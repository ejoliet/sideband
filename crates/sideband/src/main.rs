//! `sideband` CLI: send, get, bot, doctor.

mod bot;
mod card;
mod doctor;

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use sideband_core::{
    config::{self, Config},
    error::exit_code_of,
    slack,
    ticket::{human_bytes, Ticket},
    transfer::{self, GetOptions, Relays, SeedEvent, SeedStop, Seeder, SendOptions},
};

/// Unlimited, end-to-end encrypted file transfer between teammates,
/// brokered by Slack.
#[derive(Debug, Parser)]
#[command(name = "sideband", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Share a file: hash it, post a card to Slack, stay resident and seed.
    Send {
        /// File to share.
        path: PathBuf,
        /// Slack channel, with or without the leading `#`.
        #[arg(long)]
        channel: Option<String>,
        /// Hours the ticket stays valid. 0 disables expiry.
        #[arg(long, value_name = "H")]
        ttl: Option<u32>,
        /// Stop seeding after N completed downloads. 0 is unlimited.
        #[arg(long, value_name = "N")]
        max_downloads: Option<u32>,
        /// Print only the ticket.
        #[arg(long)]
        quiet: bool,
    },
    /// Receive a file. Re-run with the same ticket to resume.
    Get {
        /// Ticket from the Slack card.
        ticket: String,
        /// Destination directory.
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Socket Mode daemon: `/drop`, Receive buttons, receive confirmations.
    Bot,
    /// Check connectivity: relay reach, hole punching, Slack auth, ticket state.
    Doctor {
        /// Also report whether this ticket is still fresh.
        #[arg(long)]
        ticket: Option<String>,
    },
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sideband=info,warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: could not start the async runtime: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(Cli::parse())) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::ExitCode::from(exit_code_of(&err) as u8)
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let cfg = Config::load()?;
    match cli.command {
        Command::Send {
            path,
            channel,
            ttl,
            max_downloads,
            quiet,
        } => {
            send(
                &cfg,
                &path,
                channel.or_else(|| cfg.channel.clone()),
                ttl.unwrap_or(cfg.ttl_hours),
                max_downloads.unwrap_or(cfg.max_downloads),
                quiet,
            )
            .await
        }
        Command::Get { ticket, out } => get(&cfg, &ticket, &out).await,
        Command::Bot => bot::run(cfg).await,
        Command::Doctor { ticket } => doctor::run(&cfg, ticket.as_deref()).await,
    }
}

async fn send(
    cfg: &Config,
    path: &std::path::Path,
    channel: Option<String>,
    ttl_hours: u32,
    max_downloads: u32,
    quiet: bool,
) -> anyhow::Result<()> {
    let seeder = Seeder::prepare(
        path,
        SendOptions {
            ttl_hours,
            max_downloads,
            relays: Relays::from_config(cfg.relay_url.as_deref()),
            store_dir: config::store_dir(),
        },
    )
    .await?;

    let ticket = seeder.ticket().clone();
    if quiet {
        println!("{ticket}");
    } else {
        println!("{} ({})", ticket.meta.name, human_bytes(ticket.meta.size));
        println!("BLAKE3 {}…", ticket.short_hash());
        println!();
        println!("{}", slack::receive_command(&ticket));
        println!();
    }

    // Posting runs on its own task so a slow, rate-limited or unreachable
    // Slack never delays or aborts the transfer.
    let card = card::Publisher::spawn(
        card::Target::from_config(cfg, channel.as_deref()),
        ticket.clone(),
        quiet,
    );

    if !quiet {
        eprintln!("seeding — Ctrl-C to stop");
    }
    let stop = seeder
        .seed(|event| {
            if !quiet {
                match event {
                    SeedEvent::DownloadStarted => eprintln!("peer started a download"),
                    SeedEvent::DownloadCompleted { total } => {
                        eprintln!("download {total} completed")
                    }
                }
            }
            card.notify(event);
        })
        .await?;

    if !quiet {
        eprintln!(
            "{}",
            match stop {
                SeedStop::MaxDownloads(n) => format!("stopped: {n} downloads completed"),
                SeedStop::Expired => "stopped: ticket expired".to_string(),
                SeedStop::Interrupted => "stopped: interrupted".to_string(),
            }
        );
    }
    card.finish(stop).await;
    Ok(())
}

async fn get(cfg: &Config, ticket: &str, out: &std::path::Path) -> anyhow::Result<()> {
    let ticket: Ticket = ticket.parse().context("reading the ticket")?;
    eprintln!(
        "receiving {} ({})",
        ticket.meta.name,
        human_bytes(ticket.meta.size)
    );

    let mut last_percent = u64::MAX;
    let dest = transfer::fetch(
        &ticket,
        out,
        GetOptions {
            relays: Relays::from_config(cfg.relay_url.as_deref()),
            store_dir: config::store_dir(),
        },
        |progress| {
            let percent = progress
                .done
                .saturating_mul(100)
                .checked_div(progress.total)
                .unwrap_or(0);
            if percent != last_percent {
                last_percent = percent;
                eprint!("\r{percent:>3}% {}", human_bytes(progress.done));
            }
        },
    )
    .await?;

    // Close the progress line rather than overwriting it: piped output has no
    // carriage return to honour, and the final percentage is worth keeping.
    eprintln!();
    eprintln!("verified BLAKE3 {}…", ticket.short_hash());
    println!("{}", dest.display());
    Ok(())
}
