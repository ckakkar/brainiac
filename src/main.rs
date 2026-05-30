mod audio;
mod config;
mod events;
mod llm;
mod stt;
mod terminal;
mod tts;
mod tui;

/// Brainiac — a voice-activated AI assistant in your terminal.
///
/// Two front-ends share the same mic → STT → LLM → TTS pipeline and differ
/// only in how they render:
///   • default          rich Ratatui TUI (waveform, live transcript, history)
///   • --minimal / -m   plain line-based terminal UI (no alt-screen)
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        print_usage();
        return Ok(());
    }

    let minimal = args
        .iter()
        .any(|a| matches!(a.as_str(), "-m" | "--minimal" | "--mini"));

    let config = config::Config::from_env()?;

    if minimal {
        terminal::run(config).await
    } else {
        tui::run(config).await
    }
}

fn print_usage() {
    println!(
        "\
brainiac — voice-activated AI assistant in your terminal

USAGE:
    brainiac [OPTIONS]

OPTIONS:
    -m, --minimal    Use the minimal line-based terminal UI
                     (default is the full Ratatui TUI)
    -h, --help       Print this help

Both modes share the same mic → STT → LLM → TTS pipeline; they differ only
in how they render. Configuration is read from the environment (see .env /
src/env.example): DEEPGRAM_API_KEY, DEEPSEEK_API_KEY, DEEPSEEK_BASE_URL,
DEEPSEEK_MODEL, TTS_VOICE."
    );
}
