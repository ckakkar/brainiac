mod audio;
mod config;
mod events;
mod llm;
mod stt;
mod tts;
mod tui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = config::Config::from_env()?;
    tui::run(config).await
}
