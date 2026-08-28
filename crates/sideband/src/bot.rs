//! `sideband bot`: a Socket Mode daemon on the sender's own machine.
//!
//! The bot is personal, not workspace-hosted: `/drop` seeds a file that lives on
//! whichever laptop is running this process. That is what lets a share happen
//! without the bytes touching any server.
//!
//! Slack requires an acknowledgement within three seconds, so every callback
//! returns immediately and does the real work on a spawned task.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use sideband_core::{
    config::{self, Config},
    slack::{self, PostedCard, RECEIVE_ACTION_ID},
    ticket::{human_bytes, Ticket},
    transfer::{Relays, Seeder, SendOptions},
};
use slack_morphism::prelude::*;

use crate::card;

/// Config the callbacks need. Callbacks are plain `fn` pointers, so this travels
/// through the listener's user-state map rather than a closure capture.
#[derive(Debug, Clone)]
struct BotState {
    cfg: Config,
    bot_token: String,
}

/// Connects to Socket Mode and serves until SIGINT/SIGTERM.
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let bot_token = cfg.bot_token()?.to_string();
    let app_token = cfg.app_token()?.to_string();
    let client = slack::client()?;

    // Fail fast with exit code 2 on a bad token instead of letting Socket Mode
    // reconnect forever.
    let who = slack::auth_test(&client, &bot_token).await?;
    println!("sideband bot connected to {who}");
    println!("use `/drop <path>` in any channel this app is in");

    let environment = Arc::new(
        SlackClientEventsListenerEnvironment::new(client)
            .with_user_state(BotState { cfg, bot_token }),
    );
    let callbacks = SlackSocketModeListenerCallbacks::new()
        .with_command_events(on_command)
        .with_interaction_events(on_interaction);
    let listener = SlackClientSocketModeListener::new(
        &SlackClientSocketModeConfig::new(),
        environment,
        callbacks,
    );

    listener
        .listen_for(&SlackApiToken::new(app_token.into()))
        .await
        .context("opening a Socket Mode connection")?;
    listener.serve().await;
    Ok(())
}

/// Answers `/drop <path>` at once and seeds on a spawned task.
async fn on_command(
    event: SlackCommandEvent,
    client: Arc<slack::Client>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<SlackCommandEventResponse> {
    let Some(state) = user_state(&states).await else {
        return Ok(reply("sideband bot is misconfigured; check its logs"));
    };

    let raw = event.text.as_deref().unwrap_or("").trim().to_string();
    if raw.is_empty() {
        return Ok(reply(
            "Usage: `/drop <path>` — a path on the machine running the sideband bot.",
        ));
    }

    let path = expand_home(&raw);
    // Checked before acknowledging so a typo gets an immediate, useful answer.
    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(reply(&format!("`{raw}` is not a regular file."))),
        Err(err) => return Ok(reply(&format!("Cannot read `{raw}`: {err}"))),
    }

    let channel = event.channel_id.clone();
    let user = event.user_id.clone();
    tokio::spawn(async move { seed_and_post(state, client, channel, user, path).await });

    Ok(reply(&format!(
        "Hashing `{raw}` — the card will appear shortly."
    )))
}

/// Answers a Receive button press with the one-liner, then notes it in-thread.
async fn on_interaction(
    event: SlackInteractionEvent,
    client: Arc<slack::Client>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    let SlackInteractionEvent::BlockActions(actions) = event else {
        return Ok(());
    };
    let Some(state) = user_state(&states).await else {
        return Ok(());
    };

    let Some(action) = actions
        .actions
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|a| a.action_id.0 == RECEIVE_ACTION_ID)
    else {
        return Ok(());
    };
    let Some(raw) = action.value.clone() else {
        return Ok(());
    };
    let Some(user) = actions.user.as_ref().map(|u| u.id.clone()) else {
        return Ok(());
    };
    let channel =
        actions
            .channel
            .as_ref()
            .map(|c| c.id.clone())
            .or_else(|| match &actions.container {
                SlackInteractionActionContainer::Message(m) => m.channel_id.clone(),
                _ => None,
            });
    let Some(channel) = channel else {
        return Ok(());
    };
    let card_ts = match &actions.container {
        SlackInteractionActionContainer::Message(m) => Some(m.message_ts.clone()),
        _ => None,
    };

    tokio::spawn(async move {
        answer_receive(state, client, channel, user, card_ts, raw).await;
    });
    Ok(())
}

/// Hashes the file, publishes the card, and seeds until it expires.
async fn seed_and_post(
    state: BotState,
    client: Arc<slack::Client>,
    channel: SlackChannelId,
    user: SlackUserId,
    path: PathBuf,
) {
    let opts = SendOptions {
        ttl_hours: state.cfg.ttl_hours,
        max_downloads: state.cfg.max_downloads,
        relays: Relays::from_config(state.cfg.relay_url.as_deref()),
        store_dir: config::store_dir(),
    };
    let seeder = match Seeder::prepare(&path, opts).await {
        Ok(seeder) => seeder,
        Err(err) => {
            let text = format!("Could not share `{}`: {err:#}", path.display());
            let _ = slack::post_ephemeral(&client, &state.bot_token, channel, user, text).await;
            return;
        }
    };

    let ticket = seeder.ticket().clone();
    let target = card::Target {
        token: state.bot_token.clone(),
        channel: channel.to_string(),
        sender: format!("<@{user}>"),
    };
    let publisher = card::Publisher::spawn(Some(target), ticket, true);

    match seeder.seed(|event| publisher.notify(event)).await {
        Ok(stop) => publisher.finish(stop).await,
        Err(err) => tracing::warn!(%err, "seeding failed"),
    }
}

/// Sends the receive one-liner privately and leaves a public breadcrumb.
async fn answer_receive(
    state: BotState,
    client: Arc<slack::Client>,
    channel: SlackChannelId,
    user: SlackUserId,
    card_ts: Option<SlackTs>,
    raw: String,
) {
    let text = match raw.parse::<Ticket>() {
        Err(err) => format!("That ticket is unreadable: {err:#}"),
        Ok(ticket) => match ticket.check_fresh() {
            Err(err) => format!("{err}. Ask the sender to re-share."),
            Ok(()) => format!(
                "Run this to fetch {} ({}):\n```{}```\nIt resumes if interrupted — just run it again.\n\n{}",
                ticket.meta.name,
                human_bytes(ticket.meta.size),
                slack::receive_command(&ticket),
                slack::INSTALL_HINT
            ),
        },
    };

    if let Err(err) = slack::post_ephemeral(
        &client,
        &state.bot_token,
        channel.clone(),
        user.clone(),
        text,
    )
    .await
    {
        tracing::warn!(%err, "could not send the receive instructions");
        return;
    }

    if let Some(ts) = card_ts {
        let card = PostedCard { channel, ts };
        let _ = slack::reply_in_thread(
            &client,
            &state.bot_token,
            &card,
            format!("<@{user}> is fetching this file."),
        )
        .await;
    }
}

async fn user_state(states: &SlackClientEventsUserState) -> Option<BotState> {
    states.read().await.get_user_state::<BotState>().cloned()
}

/// Ephemeral slash-command reply, visible only to the caller.
fn reply(text: &str) -> SlackCommandEventResponse {
    SlackCommandEventResponse::new(SlackMessageContent::new().with_text(text.to_string()))
        .with_response_type(SlackMessageResponseType::Ephemeral)
}

/// `/drop ~/x` is typed by humans, so a leading `~` has to work.
fn expand_home(raw: &str) -> PathBuf {
    let trimmed = raw.trim_matches(|c| c == '`' || c == '"' || c == '\'');
    match trimmed.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(trimmed),
        },
        None => PathBuf::from(trimmed),
    }
}
