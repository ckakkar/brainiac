use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;

use crate::{config::Config, events::AppEvent};

/// Calls Deepgram Aura TTS, plays the audio synchronously on a dedicated thread
/// (rodio OutputStream is !Send on some platforms), then fires TtsComplete.
pub async fn speak(config: Config, text: String, event_tx: UnboundedSender<AppEvent>) -> Result<()> {
    let audio_bytes = fetch_audio(&config, &text).await?;

    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Result<()>>();

    std::thread::Builder::new()
        .name("brainiac-tts".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let (_stream, handle) = rodio::OutputStream::try_default()?;
                let sink = rodio::Sink::try_new(&handle)?;
                let cursor = std::io::Cursor::new(audio_bytes);
                let source = rodio::Decoder::new(cursor)?;
                sink.append(source);
                sink.sleep_until_end();
                Ok(())
            })();
            let _ = done_tx.send(result);
        })?;

    done_rx.await??;
    let _ = event_tx.send(AppEvent::TtsComplete);
    Ok(())
}

async fn fetch_audio(config: &Config, text: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::new();

    // Request WAV (linear16) for broadest rodio decoder compatibility.
    let url = format!(
        "https://api.deepgram.com/v1/speak?model={}&encoding=linear16&container=wav",
        config.tts_voice
    );

    let response = client
        .post(&url)
        .header("Authorization", format!("Token {}", config.deepgram_api_key))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!("TTS request failed: {status}");
    }

    Ok(response.bytes().await?.to_vec())
}
