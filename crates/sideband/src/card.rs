//! Best-effort Slack card publishing, shared by `sideband send` and the bot.
//!
//! Every Slack call happens on a detached task. Callers hand updates to a
//! channel and never await a Slack response, so a rate-limited or unreachable
//! Slack cannot slow down or fail the transfer.

use std::time::Duration;

use sideband_core::{
    slack::{self, CardState},
    transfer::{SeedEvent, SeedStop},
    Config, Ticket,
};
use tokio::{sync::mpsc, task::JoinHandle};

/// Where a card should go and who it says it is from.
#[derive(Debug, Clone)]
pub struct Target {
    /// Bot token with `chat:write`.
    pub token: String,
    /// Channel name or ID.
    pub channel: String,
    /// Display name for the card's `From` field.
    pub sender: String,
}

impl Target {
    /// Builds a target from config plus a channel, or `None` when Slack is not
    /// configured. A missing token is not an error: the ticket already works.
    pub fn from_config(cfg: &Config, channel: Option<&str>) -> Option<Self> {
        Some(Self {
            token: cfg.bot_token().ok()?.to_string(),
            channel: channel?.to_string(),
            sender: whoami(),
        })
    }
}

/// Handle onto the card-updating task.
pub struct Publisher {
    updates: Option<mpsc::UnboundedSender<CardState>>,
    task: Option<JoinHandle<()>>,
}

impl Publisher {
    /// Starts posting a card, or returns an inert publisher when `target` is
    /// `None`.
    pub fn spawn(target: Option<Target>, ticket: Ticket, quiet: bool) -> Self {
        let Some(target) = target else {
            if !quiet {
                eprintln!(
                    "not posting to Slack (no channel or SLACK_BOT_TOKEN); share the command above"
                );
            }
            return Self {
                updates: None,
                task: None,
            };
        };

        let (updates, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move { run(target, ticket, quiet, rx).await });
        Self {
            updates: Some(updates),
            task: Some(task),
        }
    }

    /// Queues a card update. Never blocks and never fails the caller.
    pub fn notify(&self, event: SeedEvent) {
        if let (Some(updates), SeedEvent::DownloadCompleted { total }) = (&self.updates, event) {
            let _ = updates.send(CardState::Downloaded(total));
        }
    }

    /// Marks the card expired and waits briefly for the last Slack call.
    pub async fn finish(mut self, stop: SeedStop) {
        let Some(updates) = self.updates.take() else {
            return;
        };
        if stop != SeedStop::Interrupted {
            let _ = updates.send(CardState::Expired);
        }
        drop(updates);
        if let Some(task) = self.task.take() {
            // Bounded so Ctrl-C stays responsive if Slack is hanging.
            if tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .is_err()
            {
                tracing::debug!("gave up waiting for the final Slack card update");
            }
        }
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn run(
    target: Target,
    ticket: Ticket,
    quiet: bool,
    mut updates: mpsc::UnboundedReceiver<CardState>,
) {
    let client = match slack::client() {
        Ok(client) => client,
        Err(err) => {
            warn(quiet, format!("could not build a Slack client: {err:#}"));
            return;
        }
    };
    let card = match slack::post_card(
        &client,
        &target.token,
        &target.channel,
        &ticket,
        &target.sender,
    )
    .await
    {
        Ok(card) => {
            if !quiet {
                eprintln!("posted to #{}", target.channel.trim_start_matches('#'));
            }
            card
        }
        Err(err) => {
            warn(quiet, format!("could not post the Slack card: {err:#}"));
            return;
        }
    };

    while let Some(state) = updates.recv().await {
        if let Err(err) = slack::update_card(
            &client,
            &target.token,
            &card,
            &ticket,
            &target.sender,
            state,
        )
        .await
        {
            tracing::debug!(%err, "card update failed");
        }
    }
}

/// Slack failures are advisory: the transfer is already running.
fn warn(quiet: bool, message: String) {
    if quiet {
        tracing::warn!("{message}");
    } else {
        eprintln!("warning: {message} — the transfer continues");
    }
}

/// Best guess at who is sharing, for the card's `From` field.
fn whoami() -> String {
    std::env::var("SIDEBAND_SENDER")
        .or_else(|_| std::env::var("USER"))
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "someone".to_string())
}
