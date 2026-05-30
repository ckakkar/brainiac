use anyhow::Result;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::{audio::Microphone, config::Config, events::AppEvent, llm::Message};

// ── ANSI helpers ───────────────────────────────────────────────────────────────

const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const GREEN: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CLEAR_LINE: &str = "\x1b[2K\r";

macro_rules! cprintln {
    ($color:expr, $($t:tt)*) => {
        println!("{}{}{}", $color, format!($($t)*), RESET)
    };
}

// ── State ──────────────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum Status {
    Idle,
    Listening,
    Thinking,
    Speaking,
}

struct State {
    status: Status,
    recording: bool,
    conversation: Vec<Message>,
    current_response: String,
}

impl State {
    fn new() -> Self {
        Self {
            status: Status::Idle,
            recording: false,
            conversation: Vec::new(),
            current_response: String::new(),
        }
    }
}

// ── Entry point ────────────────────────────────────────────────────────────────

pub async fn run(config: Config) -> Result<()> {
    print_banner();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<String>();

    // Stdin reader — each line is a command.
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if key_tx.send(line.trim().to_lowercase()).is_err() {
                break;
            }
        }
    });

    let mic = match Microphone::spawn() {
        Ok(m) => {
            cprintln!(DIM, "  mic ready @ {}Hz", m.sample_rate);
            Some(m)
        }
        Err(e) => {
            cprintln!("\x1b[31m", "  mic unavailable: {e}");
            None
        }
    };
    let mic_sample_rate = mic.as_ref().map(|m| m.sample_rate).unwrap_or(16000);

    print_prompt();

    let mut state = State::new();

    loop {
        tokio::select! {
            Some(line) = key_rx.recv() => {
                match line.as_str() {
                    "q" | "quit" | "exit" => break,
                    "c" | "clear" => {
                        state.conversation.clear();
                        cprintln!(DIM, "  conversation cleared");
                        print_prompt();
                    }
                    _ => handle_enter(&mut state, &config, &event_tx, &mic, mic_sample_rate),
                }
            }

            Some(ev) = event_rx.recv() => {
                let should_prompt = handle_event(&mut state, ev, &config, &event_tx);
                if should_prompt {
                    print_prompt();
                }
            }
        }
    }

    println!("\n{DIM}  brainiac offline{RESET}");
    Ok(())
}

fn handle_enter(
    state: &mut State,
    config: &Config,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    mic: &Option<Microphone>,
    sample_rate: u32,
) {
    match state.status {
        Status::Idle => {
            let Some(ref mic) = mic else {
                cprintln!("\x1b[31m", "  no microphone");
                print_prompt();
                return;
            };
            match mic.start_listening(event_tx.clone()) {
                Ok(audio_rx) => {
                    state.status = Status::Listening;
                    state.recording = true;
                    println!("\n{YELLOW}{BOLD}  ◉ listening{RESET}{DIM}  (ENTER to stop){RESET}");

                    let api_key = config.deepgram_api_key.clone();
                    let ev = event_tx.clone();
                    tokio::spawn(crate::stt::stream(api_key, sample_rate, audio_rx, ev));
                }
                Err(e) => {
                    cprintln!("\x1b[31m", "  mic error: {e}");
                    print_prompt();
                }
            }
        }

        Status::Listening if state.recording => {
            if let Some(ref mic) = mic {
                mic.stop_listening();
                state.recording = false;
                print!("{CLEAR_LINE}{DIM}  processing...{RESET}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
        }

        _ => {} // ignore ENTER while thinking/speaking
    }
}

fn handle_event(
    state: &mut State,
    ev: AppEvent,
    config: &Config,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
) -> bool {
    match ev {
        // Overwrite current line with partial transcript
        AppEvent::TranscriptPartial(t) => {
            print!("{CLEAR_LINE}{YELLOW}  ◉ {t}{RESET}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            false
        }

        AppEvent::TranscriptFinal(t) => {
            print!("{CLEAR_LINE}");
            if t.is_empty() {
                state.status = Status::Idle;
                cprintln!(DIM, "  (no speech detected)");
                return true;
            }
            cprintln!("\x1b[2m\x1b[33m", "  you: {t}");
            state.conversation.push(Message::User(t.clone()));
            state.status = Status::Thinking;

            print!("\n{MAGENTA}{BOLD}  ◈ brainiac:{RESET} ");
            let _ = std::io::Write::flush(&mut std::io::stdout());

            let cfg = config.clone();
            let history = state.conversation.clone();
            let ev = event_tx.clone();
            tokio::spawn(crate::llm::complete(cfg, history, ev));
            false
        }

        // Stream tokens inline, no newline yet
        AppEvent::LlmToken(token) => {
            print!("{CYAN}{token}{RESET}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            state.current_response.push_str(&token);
            false
        }

        AppEvent::LlmComplete => {
            println!(); // end the streaming line
            let response = std::mem::take(&mut state.current_response);
            if response.is_empty() {
                state.status = Status::Idle;
                return true;
            }
            state.conversation.push(Message::Assistant(response.clone()));
            state.status = Status::Speaking;

            println!("{DIM}  ◈ speaking...{RESET}");

            let cfg = config.clone();
            let ev = event_tx.clone();
            tokio::spawn(crate::tts::speak(cfg, response, ev));
            false
        }

        AppEvent::TtsComplete => {
            state.status = Status::Idle;
            true // print prompt again
        }

        AppEvent::AudioLevel(_) => false, // unused in terminal mode

        AppEvent::Error(e) => {
            print!("{CLEAR_LINE}");
            cprintln!("\x1b[31m", "  error: {e}");
            state.status = Status::Idle;
            true
        }
    }
}

// ── UI helpers ─────────────────────────────────────────────────────────────────

fn print_banner() {
    println!();
    println!("{CYAN}{BOLD}  ◈ BRAINIAC{RESET}{DIM} v0.1 — DeepSeek + Deepgram{RESET}");
    println!("{DIM}  ─────────────────────────────────────{RESET}");
    println!("{DIM}  ENTER  start/stop listening{RESET}");
    println!("{DIM}  c      clear conversation{RESET}");
    println!("{DIM}  q      quit{RESET}");
    println!("{DIM}  ─────────────────────────────────────{RESET}");
    println!();
}

fn print_prompt() {
    print!("{GREEN}  ▸ {RESET}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}
