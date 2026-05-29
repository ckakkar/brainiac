use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::events::AppEvent;

#[derive(Deserialize)]
struct DgResponse {
    channel: Option<DgChannel>,
    is_final: Option<bool>,
    speech_final: Option<bool>,
}

#[derive(Deserialize)]
struct DgChannel {
    alternatives: Vec<DgAlternative>,
}

#[derive(Deserialize)]
struct DgAlternative {
    transcript: String,
}

/// Streams PCM i16 audio from `audio_rx` to Deepgram, forwarding transcript
/// events back via `event_tx`. Exits cleanly when `audio_rx` is closed
/// (sender dropped on stop_listening).
pub async fn stream(
    api_key: String,
    sample_rate: u32,
    mut audio_rx: UnboundedReceiver<Vec<i16>>,
    event_tx: UnboundedSender<AppEvent>,
) -> Result<()> {
    let url = format!(
        "wss://api.deepgram.com/v1/listen\
        ?encoding=linear16\
        &sample_rate={sample_rate}\
        &channels=1\
        &model=nova-2\
        &language=en-IN\
        &endpointing=300\
        &utterance_end_ms=1000\
        &interim_results=true"
    );

    // Build WS upgrade request with auth header.
    use tokio_tungstenite::tungstenite::handshake::client::Request;
    let request = Request::builder()
        .uri(&url)
        .header("Authorization", format!("Token {api_key}"))
        .body(())?;

    let (ws, _) = connect_async(request).await?;
    let (mut ws_tx, mut ws_rx) = ws.split();

    let event_tx_recv = event_tx.clone();

    // Receive transcripts concurrently with sending audio.
    let recv_loop = async {
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(text) = msg {
                if let Ok(resp) = serde_json::from_str::<DgResponse>(&text) {
                    if let Some(ch) = resp.channel {
                        if let Some(alt) = ch.alternatives.into_iter().next() {
                            let t = alt.transcript.trim().to_string();
                            if t.is_empty() {
                                continue;
                            }
                            let event = if resp.speech_final.unwrap_or(false)
                                || resp.is_final.unwrap_or(false)
                            {
                                AppEvent::TranscriptFinal(t)
                            } else {
                                AppEvent::TranscriptPartial(t)
                            };
                            let _ = event_tx_recv.send(event);
                        }
                    }
                }
            }
        }
    };

    let send_loop = async {
        while let Some(chunk) = audio_rx.recv().await {
            let bytes: Vec<u8> = chunk.iter().flat_map(|&s| s.to_le_bytes()).collect();
            if ws_tx.send(Message::Binary(bytes)).await.is_err() {
                break;
            }
        }
        // Audio channel closed → tell Deepgram we're done so it flushes the final transcript.
        let _ = ws_tx.send(Message::Close(None)).await;
    };

    // Run both until WS closes.
    tokio::join!(send_loop, recv_loop);

    Ok(())
}
