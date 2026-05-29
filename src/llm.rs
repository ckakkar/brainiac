use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;

use crate::{config::Config, events::AppEvent};

const SYSTEM_PROMPT: &str = "\
You are Brainiac — a voice-activated AI assistant running on the local machine of Cyrus Kakkar, \
a systems engineer, architect, and polymath. \
\n\n\
Rules:\n\
- Be concise, technically precise, and direct. No filler words.\n\
- Assume expert-level comprehension across all domains.\n\
- When discussing code, default to Rust or C++ for systems work, TypeScript/Next.js for web.\n\
- Keep responses short enough to read aloud comfortably — expand only when depth is explicitly requested.\n\
- Dry wit is welcome. Jargon is fine.";

#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Assistant(String),
}

pub async fn complete(
    config: Config,
    history: Vec<Message>,
    event_tx: UnboundedSender<AppEvent>,
) -> Result<()> {
    let client = reqwest::Client::new();

    let mut messages = vec![serde_json::json!({
        "role": "system",
        "content": SYSTEM_PROMPT,
    })];

    for msg in &history {
        match msg {
            Message::User(t) => {
                messages.push(serde_json::json!({"role": "user", "content": t}))
            }
            Message::Assistant(t) => {
                messages.push(serde_json::json!({"role": "assistant", "content": t}))
            }
        }
    }

    let body = serde_json::json!({
        "model": config.deepseek_model,
        "messages": messages,
        "stream": true,
        "max_tokens": 512,
        "temperature": 0.7,
    });

    let response = client
        .post(format!("{}/v1/chat/completions", config.deepseek_base_url))
        .header("Authorization", format!("Bearer {}", config.deepseek_api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let _ = event_tx.send(AppEvent::Error(format!("LLM {status}: {body}")));
        return Ok(());
    }

    use futures_util::StreamExt;
    let mut byte_stream = response.bytes_stream();
    let mut buf = String::new();

    while let Some(chunk) = byte_stream.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // SSE events are separated by double newlines.
        while let Some(pos) = buf.find("\n\n") {
            let event = buf[..pos].trim().to_string();
            buf = buf[pos + 2..].to_string();

            for line in event.lines() {
                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                if data.trim() == "[DONE]" {
                    let _ = event_tx.send(AppEvent::LlmComplete);
                    return Ok(());
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                        if !content.is_empty() {
                            let _ = event_tx.send(AppEvent::LlmToken(content.to_string()));
                        }
                    }
                }
            }
        }
    }

    let _ = event_tx.send(AppEvent::LlmComplete);
    Ok(())
}
