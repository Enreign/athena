use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

use teloxide::prelude::*;
use teloxide::types::{BotCommand, InlineKeyboardButton, InlineKeyboardMarkup};

use crate::config::TelegramConfig;
use crate::confirm::Confirmer;
use crate::core::{CoreEvent, CoreHandle, SessionContext};
use crate::error::{AthenaError, Result};

/// Telegram confirmer: sends inline keyboard, waits on oneshot with timeout.
pub struct TelegramConfirmer {
    bot: Bot,
    chat_id: ChatId,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    timeout_secs: u64,
}

#[async_trait::async_trait]
impl Confirmer for TelegramConfirmer {
    async fn confirm(&self, action: &str) -> Result<bool> {
        let confirm_id = uuid::Uuid::new_v4().to_string();

        let keyboard = InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback("Approve", format!("confirm:{}:yes", confirm_id)),
            InlineKeyboardButton::callback("Deny", format!("confirm:{}:no", confirm_id)),
        ]]);

        let text = format!("Action: {}\n\nApprove?", action);

        self.bot
            .send_message(self.chat_id, &text)
            .reply_markup(keyboard)
            .await
            .map_err(|e| AthenaError::Tool(format!("Failed to send confirmation: {}", e)))?;

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(confirm_id.clone(), tx);
        }

        let timeout = tokio::time::Duration::from_secs(self.timeout_secs);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(true)) => Ok(true),
            Ok(Ok(false)) | Err(_) => {
                // Timed out or denied — clean up
                let mut pending = self.pending.lock().await;
                pending.remove(&confirm_id);
                Err(AthenaError::Cancelled)
            }
            Ok(Err(_)) => {
                // Channel dropped
                Err(AthenaError::Cancelled)
            }
        }
    }
}

/// Shared state for the Telegram bot.
#[derive(Clone)]
struct TelegramState {
    handle: CoreHandle,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    config: TelegramConfig,
}

/// Split a message into chunks at 4000 chars (Telegram limit is 4096).
fn chunk_message(text: &str, max: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max).min(text.len());
        // Try to break at a newline if possible
        let actual_end = if end < text.len() {
            text[start..end]
                .rfind('\n')
                .map(|pos| start + pos + 1)
                .unwrap_or(end)
        } else {
            end
        };
        chunks.push(&text[start..actual_end]);
        start = actual_end;
    }
    chunks
}

/// Handle an incoming text message.
async fn handle_message(bot: Bot, msg: Message, state: TelegramState) -> ResponseResult<()> {
    let text = match msg.text() {
        Some(t) => t.to_string(),
        None => return Ok(()),
    };

    let chat_id = msg.chat.id;

    // Auth check
    if !state.config.allowed_chats.is_empty()
        && !state.config.allowed_chats.contains(&chat_id.0)
    {
        bot.send_message(chat_id, "Unauthorized.")
            .await?;
        return Ok(());
    }

    if state.config.allowed_chats.is_empty() {
        tracing::warn!(chat_id = chat_id.0, "Processing message from unfiltered chat");
    }

    // Slash commands
    if text == "/start" || text == "/help" {
        bot.send_message(
            chat_id,
            "Send me a message and I'll process it. Use /agents to list agents, /memories to list memories.",
        )
        .await?;
        return Ok(());
    }

    if text == "/agents" {
        let agents = state.handle.list_agents();
        let mut out = String::from("Configured agents:\n\n");
        for a in &agents {
            out.push_str(&format!(
                "- {} — {} [{}]\n",
                a.name,
                a.description,
                a.tools.join(", ")
            ));
        }
        bot.send_message(chat_id, &out).await?;
        return Ok(());
    }

    if text == "/memories" {
        match state.handle.list_memories() {
            Ok(memories) if memories.is_empty() => {
                bot.send_message(chat_id, "No memories.").await?;
            }
            Ok(memories) => {
                let mut out = String::new();
                for m in &memories {
                    out.push_str(&format!("[{}] {} — {}\n", &m.id[..8], m.category, m.content));
                }
                bot.send_message(chat_id, &out).await?;
            }
            Err(e) => {
                bot.send_message(chat_id, &format!("Error: {}", e))
                    .await?;
            }
        }
        return Ok(());
    }

    // Build confirmer for this chat
    let confirmer: Arc<dyn Confirmer> = Arc::new(TelegramConfirmer {
        bot: bot.clone(),
        chat_id,
        pending: state.pending.clone(),
        timeout_secs: state.config.confirm_timeout_secs,
    });

    let session = SessionContext {
        platform: "telegram".into(),
        user_id: msg
            .from
            .as_ref()
            .map(|u| u.id.0.to_string())
            .unwrap_or_else(|| "unknown".into()),
        chat_id: chat_id.0.to_string(),
    };

    tracing::debug!("Sending Working... status message");
    // Send "Working..." status message that we'll update
    let status_msg = bot
        .send_message(chat_id, "Working...")
        .await?;
    tracing::debug!(msg_id = %status_msg.id, "Status message sent");

    tracing::debug!("Sending to core via handle.chat()");
    let mut events = match state.handle.chat(session, &text, confirmer).await {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!(error = %e, "handle.chat() failed");
            bot.edit_message_text(chat_id, status_msg.id, format!("Error: {}", e))
                .await?;
            return Ok(());
        }
    };
    tracing::debug!("Waiting for core events");

    while let Some(event) = events.recv().await {
        match event {
            CoreEvent::Status(s) => {
                let _ = bot
                    .edit_message_text(chat_id, status_msg.id, &s)
                    .await;
            }
            CoreEvent::Response(r) => {
                // Delete the status message
                let _ = bot.delete_message(chat_id, status_msg.id).await;

                // Send response, chunked if needed
                for chunk in chunk_message(&r, 4000) {
                    bot.send_message(chat_id, chunk).await?;
                }
            }
            CoreEvent::Error(e) => {
                let _ = bot
                    .edit_message_text(chat_id, status_msg.id, format!("Error: {}", e))
                    .await;
            }
        }
    }

    Ok(())
}

/// Handle callback queries (confirmation button presses).
async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    state: TelegramState,
) -> ResponseResult<()> {
    let data = match q.data {
        Some(d) => d,
        None => return Ok(()),
    };

    // Parse "confirm:<id>:<yes|no>"
    let parts: Vec<&str> = data.splitn(3, ':').collect();
    if parts.len() != 3 || parts[0] != "confirm" {
        bot.answer_callback_query(&q.id).await?;
        return Ok(());
    }

    let confirm_id = parts[1];
    let approved = parts[2] == "yes";

    let mut pending = state.pending.lock().await;
    if let Some(tx) = pending.remove(confirm_id) {
        let _ = tx.send(approved);
        let answer = if approved { "Approved" } else { "Denied" };

        // Update the keyboard message
        if let Some(msg) = &q.message {
            if let Some(regular) = msg.regular_message() {
                let _ = bot
                    .edit_message_text(
                        regular.chat.id,
                        regular.id,
                        format!(
                            "{}\n\n{}",
                            regular.text().unwrap_or(""),
                            answer
                        ),
                    )
                    .await;
            }
        }

        bot.answer_callback_query(&q.id)
            .text(answer)
            .await?;
    } else {
        bot.answer_callback_query(&q.id)
            .text("Session expired, please retry.")
            .await?;
    }

    Ok(())
}

/// Entry point: run the Telegram bot.
pub async fn run_telegram(handle: CoreHandle, config: TelegramConfig) -> anyhow::Result<()> {
    let token = config
        .token
        .clone()
        .or_else(|| std::env::var("ATHENA_TELEGRAM_TOKEN").ok())
        .ok_or_else(|| anyhow::anyhow!(
            "Telegram token not set. Set [telegram].token in config.toml or ATHENA_TELEGRAM_TOKEN env var"
        ))?;

    let bot = Bot::new(&token);

    if config.allowed_chats.is_empty() {
        tracing::warn!("No allowed_chats configured — bot will respond to ALL chats");
    }

    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Spawn stale confirmation cleanup task
    let cleanup_pending = pending.clone();
    let cleanup_timeout = config.confirm_timeout_secs;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let mut map = cleanup_pending.lock().await;
            // We can't tell how old entries are without timestamps, so we just
            // log the count. The actual timeout is enforced per-request in TelegramConfirmer.
            if !map.is_empty() {
                tracing::debug!(
                    count = map.len(),
                    timeout_secs = cleanup_timeout,
                    "Pending confirmations"
                );
                // Drop entries whose receivers have been dropped (task timed out)
                map.retain(|_, tx| !tx.is_closed());
            }
        }
    });

    let state = TelegramState {
        handle,
        pending,
        config,
    };

    // Register bot commands menu (the "/" button in Telegram)
    let commands = vec![
        BotCommand::new("help", "Show available commands"),
        BotCommand::new("agents", "List configured agents"),
        BotCommand::new("memories", "List saved memories"),
    ];
    bot.set_my_commands(commands)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to set bot commands: {}", e))?;

    eprintln!("Telegram bot starting...");

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .endpoint({
                    let state = state.clone();
                    move |bot: Bot, msg: Message| {
                        let state = state.clone();
                        async move { handle_message(bot, msg, state).await }
                    }
                }),
        )
        .branch(
            Update::filter_callback_query()
                .endpoint({
                    let state = state.clone();
                    move |bot: Bot, q: CallbackQuery| {
                        let state = state.clone();
                        async move { handle_callback(bot, q, state).await }
                    }
                }),
        );

    Dispatcher::builder(bot, handler)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}
