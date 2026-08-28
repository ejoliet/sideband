//! Slack as a control plane: post the file card, update it, and serve the
//! Socket Mode interactions.
//!
//! Nothing here is on the critical path of a transfer. Callers post
//! best-effort and keep transferring when Slack is down or rate-limited.

use std::sync::Arc;

use slack_morphism::prelude::*;

use crate::{
    error::SidebandError,
    ticket::{human_bytes, Ticket},
};

/// `action_id` of the Receive button. The bot answers it with the one-liner.
pub const RECEIVE_ACTION_ID: &str = "sideband_receive";

/// Hyper-backed Slack client, shared by `send` and `bot`.
pub type Client = SlackClient<SlackClientHyperHttpsConnector>;

/// A posted card, so the sender can update it later.
#[derive(Debug, Clone)]
pub struct PostedCard {
    /// Channel the card landed in, resolved to an ID by Slack.
    pub channel: SlackChannelId,
    /// Timestamp, which is also the message's edit handle.
    pub ts: SlackTs,
}

/// Builds a Slack client.
pub fn client() -> anyhow::Result<Arc<Client>> {
    Ok(Arc::new(
        SlackClient::new(SlackClientHyperConnector::new()?),
    ))
}

/// Verifies the bot token, mapping any failure to exit code 2.
pub async fn auth_test(client: &Client, token: &str) -> Result<String, SidebandError> {
    let token = SlackApiToken::new(token.into());
    client
        .open_session(&token)
        .auth_test()
        .await
        .map(|response| {
            format!(
                "{} as {}",
                response.team,
                response.user.unwrap_or_else(|| "bot".into())
            )
        })
        .map_err(|err| SidebandError::Config(format!("Slack auth failed: {err}")))
}

/// Posts the file card and returns its location so it can be updated.
pub async fn post_card(
    client: &Client,
    token: &str,
    channel: &str,
    ticket: &Ticket,
    sender: &str,
) -> anyhow::Result<PostedCard> {
    let token = SlackApiToken::new(token.into());
    let request = SlackApiChatPostMessageRequest::new(
        channel_id(channel),
        card_content(ticket, sender, CardState::Available),
    );
    let response = client
        .open_session(&token)
        .chat_post_message(&request)
        .await?;
    Ok(PostedCard {
        channel: response.channel,
        ts: response.ts,
    })
}

/// Rewrites an existing card, e.g. to mark it expired or count downloads.
pub async fn update_card(
    client: &Client,
    token: &str,
    card: &PostedCard,
    ticket: &Ticket,
    sender: &str,
    state: CardState,
) -> anyhow::Result<()> {
    let token = SlackApiToken::new(token.into());
    let request = SlackApiChatUpdateRequest::new(
        card.channel.clone(),
        card_content(ticket, sender, state),
        card.ts.clone(),
    );
    client.open_session(&token).chat_update(&request).await?;
    Ok(())
}

/// Posts a reply in the card's thread, used for receive confirmations.
pub async fn reply_in_thread(
    client: &Client,
    token: &str,
    card: &PostedCard,
    text: String,
) -> anyhow::Result<()> {
    let token = SlackApiToken::new(token.into());
    let request = SlackApiChatPostMessageRequest::new(
        card.channel.clone(),
        SlackMessageContent::new().with_text(text),
    )
    .with_thread_ts(card.ts.clone());
    client
        .open_session(&token)
        .chat_post_message(&request)
        .await?;
    Ok(())
}

/// Sends a message only the clicking user sees.
///
/// This is how the Receive button delivers the one-liner: Slack cannot put text
/// on a user's clipboard, and an ephemeral reply keeps the channel quiet.
pub async fn post_ephemeral(
    client: &Client,
    token: &str,
    channel: SlackChannelId,
    user: SlackUserId,
    text: String,
) -> anyhow::Result<()> {
    let token = SlackApiToken::new(token.into());
    let request = SlackApiChatPostEphemeralRequest::new(
        channel,
        user,
        SlackMessageContent::new().with_text(text),
    );
    client
        .open_session(&token)
        .chat_post_ephemeral(&request)
        .await?;
    Ok(())
}

/// What the card should say about the transfer right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    /// Seeding; the Receive button is live.
    Available,
    /// Seeding, and N peers have finished.
    Downloaded(u32),
    /// TTL elapsed or max downloads reached; no button.
    Expired,
}

/// The command a receiver runs. Also the Receive button's ephemeral payload.
pub fn receive_command(ticket: &Ticket) -> String {
    format!("sideband get {ticket}")
}

/// What to tell a receiver who does not have the CLI yet.
///
/// AIDEV-The README leaves the install route open (brew tap or `curl | sh`) and
/// defaults to "both, brew first". Receivers meet that decision here and nowhere
/// else, so the two channels are named in that order in one place.
pub const INSTALL_HINT: &str = concat!(
    "No `sideband` yet? `brew install ejoliet/tap/sideband`, or ",
    "`curl -fsSL https://raw.githubusercontent.com/ejoliet/sideband/main/scripts/install.sh | sh`."
);

/// Renders the Block Kit card.
///
/// The ticket itself is in the message body: the Receive button cannot write to
/// a user's clipboard, so the copyable one-liner has to be visible text.
pub fn card_content(ticket: &Ticket, sender: &str, state: CardState) -> SlackMessageContent {
    let status = match state {
        CardState::Available => match ticket.remaining_secs() {
            Some(secs) => format!("expires in {}", human_duration(secs)),
            None => "no expiry".to_string(),
        },
        CardState::Downloaded(n) => {
            format!("{n} download{} completed", if n == 1 { "" } else { "s" })
        }
        CardState::Expired => "expired — ask the sender to re-share".to_string(),
    };

    let fields = vec![
        md!("*Size*\n{}", human_bytes(ticket.meta.size)),
        md!("*BLAKE3*\n`{}…`", ticket.short_hash()),
        md!("*From*\n{}", sender),
        md!("*Status*\n{}", status),
    ];

    let blocks: Vec<SlackBlock> = slack_blocks![
        some_into(SlackHeaderBlock::new(pt!("📦 {}", ticket.meta.name))),
        some_into(SlackSectionBlock::new().with_fields(fields)),
        optionally_into(
            state != CardState::Expired
                => SlackSectionBlock::new().with_text(md!(
                    "Run this to receive it directly, no Slack storage involved:\n```{}```",
                    receive_command(ticket)
                ))
        ),
        optionally_into(
            state != CardState::Expired
                => SlackActionsBlock::new(slack_blocks![some_into(
                    SlackBlockButtonElement::new(
                        RECEIVE_ACTION_ID.into(),
                        pt!("Receive"),
                    )
                    .with_style(SlackBlockButtonStyle::Primary)
                    .with_value(ticket.to_string())
                )])
        ),
        some_into(SlackContextBlock::new(vec![
            SlackContextBlockElement::MarkDown(md!(
                "Anyone in this channel can fetch this file until it expires."
            )),
            SlackContextBlockElement::MarkDown(md!("{}", INSTALL_HINT)),
        ])),
    ];

    SlackMessageContent::new()
        .with_text(format!(
            "{} ({}) shared by {sender} — run `{}`",
            ticket.meta.name,
            human_bytes(ticket.meta.size),
            receive_command(ticket)
        ))
        .with_blocks(blocks)
}

/// Channel names and IDs both work with `chat.postMessage`.
fn channel_id(channel: &str) -> SlackChannelId {
    channel.trim_start_matches('#').into()
}

/// Rounds a duration to the largest useful unit, e.g. `23h`.
fn human_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use iroh::{EndpointAddr, SecretKey};
    use iroh_blobs::{ticket::BlobTicket, BlobFormat, Hash};

    use super::*;
    use crate::ticket::FileMeta;

    fn sample_ticket(ttl_hours: u32) -> Ticket {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4433".parse().unwrap());
        Ticket::new(
            BlobTicket::new(addr, Hash::new(b"payload"), BlobFormat::Raw),
            FileMeta {
                name: "roman-l2.tar".into(),
                size: 53_687_091_200,
            },
            ttl_hours,
        )
    }

    /// The card must carry the ticket as copyable text and as the button value,
    /// which is what makes the Receive flow work without clipboard access.
    #[test]
    fn available_card_carries_the_receive_command() {
        let ticket = sample_ticket(24);
        let json =
            serde_json::to_string(&card_content(&ticket, "ejoliet", CardState::Available)).unwrap();
        assert!(json.contains("roman-l2.tar"));
        assert!(json.contains("50.0 GiB"));
        assert!(json.contains(&receive_command(&ticket)));
        assert!(json.contains(RECEIVE_ACTION_ID));
        // A receiver without the CLI has to learn how to get it from the card.
        assert!(json.contains("brew install ejoliet/tap/sideband"));
        // A 24 h TTL renders as `1d`, or `23h` if a second ticks past first.
        assert!(json.contains("expires in 1d") || json.contains("expires in 23h"));
    }

    #[test]
    fn expired_card_drops_the_button() {
        let json = serde_json::to_string(&card_content(
            &sample_ticket(1),
            "ejoliet",
            CardState::Expired,
        ))
        .unwrap();
        assert!(!json.contains(RECEIVE_ACTION_ID));
        assert!(json.contains("expired"));
    }

    #[test]
    fn download_count_is_pluralised() {
        let ticket = sample_ticket(1);
        let one =
            serde_json::to_string(&card_content(&ticket, "e", CardState::Downloaded(1))).unwrap();
        assert!(one.contains("1 download completed"));
        let two =
            serde_json::to_string(&card_content(&ticket, "e", CardState::Downloaded(2))).unwrap();
        assert!(two.contains("2 downloads completed"));
    }

    #[test]
    fn channel_names_lose_the_hash() {
        assert_eq!(channel_id("#data").to_string(), "data");
        assert_eq!(channel_id("C0123").to_string(), "C0123");
    }

    #[test]
    fn durations_use_the_largest_unit() {
        assert_eq!(human_duration(30), "30s");
        assert_eq!(human_duration(600), "10m");
        assert_eq!(human_duration(7200), "2h");
        assert_eq!(human_duration(172_800), "2d");
    }
}
