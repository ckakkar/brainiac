use std::collections::VecDeque;
use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::{audio::Microphone, config::Config, events::AppEvent, llm::Message};

// ── Palette ────────────────────────────────────────────────────────────────────

const BG: Color = Color::Rgb(8, 8, 14);
const CYAN: Color = Color::Rgb(0, 240, 255);
const MAGENTA: Color = Color::Rgb(220, 0, 255);
const GREEN: Color = Color::Rgb(0, 255, 120);
const YELLOW: Color = Color::Rgb(255, 210, 0);
const DIM: Color = Color::Rgb(70, 70, 90);
const WHITE: Color = Color::Rgb(220, 220, 230);

// ── State machine ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Listening,  // mic active OR waiting for Deepgram final transcript
    Thinking,   // LLM streaming
    Speaking,   // TTS playback
}

impl Status {
    fn label(&self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Listening => "LISTENING",
            Self::Thinking => "THINKING",
            Self::Speaking => "SPEAKING",
        }
    }
    fn color(&self) -> Color {
        match self {
            Self::Idle => GREEN,
            Self::Listening => YELLOW,
            Self::Thinking => MAGENTA,
            Self::Speaking => CYAN,
        }
    }
}

struct AppState {
    status: Status,
    recording: bool,             // mic actively capturing (vs. waiting for final transcript)
    conversation: Vec<Message>,
    current_transcript: String,  // partial STT
    current_response: String,    // streaming LLM tokens
    waveform: VecDeque<f32>,     // recent audio levels for bar display
    error: Option<String>,
    scroll_offset: u16,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            status: Status::Idle,
            recording: false,
            conversation: Vec::new(),
            current_transcript: String::new(),
            current_response: String::new(),
            waveform: VecDeque::from(vec![0.0_f32; 60]),
            error: None,
            scroll_offset: 0,
        }
    }
}

// ── Entry point ────────────────────────────────────────────────────────────────

pub async fn run(config: Config) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, config).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    config: Config,
) -> Result<()> {
    let mut state = AppState::default();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut ct_events = EventStream::new();

    let mic = match Microphone::spawn() {
        Ok(m) => Some(m),
        Err(e) => {
            state.error = Some(format!("Mic unavailable: {e}"));
            None
        }
    };
    let mic_sample_rate = mic.as_ref().map(|m| m.sample_rate).unwrap_or(16000);

    loop {
        terminal.draw(|f| render(f, &state))?;

        tokio::select! {
            // ── Crossterm input ──────────────────────────────────────────────
            Some(Ok(event)) = ct_events.next() => {
                if let Event::Key(key) = event {
                    if key.kind != KeyEventKind::Release {
                        handle_key(&mut state, key.code, &config, &event_tx, &mic, mic_sample_rate);
                    }
                }
            }

            // ── App events from background tasks ─────────────────────────────
            Some(ev) = event_rx.recv() => {
                handle_app_event(&mut state, ev, &config, &event_tx);
            }
        }

        // Exit on explicit quit
        if state.status == Status::Idle && state.error.as_deref() == Some("__quit__") {
            break;
        }
    }

    Ok(())
}

fn handle_key(
    state: &mut AppState,
    code: KeyCode,
    config: &Config,
    event_tx: &UnboundedSender<AppEvent>,
    mic: &Option<Microphone>,
    sample_rate: u32,
) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => {
            state.error = Some("__quit__".into());
        }

        KeyCode::Char('c') => {
            state.conversation.clear();
            state.error = None;
            state.current_transcript.clear();
            state.current_response.clear();
        }

        KeyCode::Up => {
            state.scroll_offset = state.scroll_offset.saturating_add(1);
        }
        KeyCode::Down => {
            state.scroll_offset = state.scroll_offset.saturating_sub(1);
        }

        KeyCode::Char(' ') | KeyCode::Enter => {
            match state.status {
                Status::Idle => {
                    // Start listening
                    if let Some(ref mic) = mic {
                        match mic.start_listening(event_tx.clone()) {
                            Ok(audio_rx) => {
                                state.status = Status::Listening;
                                state.recording = true;
                                state.current_transcript.clear();
                                state.error = None;

                                let api_key = config.deepgram_api_key.clone();
                                let ev = event_tx.clone();
                                tokio::spawn(crate::stt::stream(api_key, sample_rate, audio_rx, ev));
                            }
                            Err(e) => state.error = Some(format!("Mic error: {e}")),
                        }
                    } else {
                        state.error = Some("No microphone available".into());
                    }
                }

                Status::Listening if state.recording => {
                    // Stop recording; STT task will drain and send TranscriptFinal.
                    if let Some(ref mic) = mic {
                        mic.stop_listening();
                        state.recording = false;
                        // Status stays Listening until TranscriptFinal arrives.
                    }
                }

                _ => {}
            }
        }

        _ => {}
    }
}

fn handle_app_event(
    state: &mut AppState,
    ev: AppEvent,
    config: &Config,
    event_tx: &UnboundedSender<AppEvent>,
) {
    match ev {
        AppEvent::AudioLevel(level) => {
            state.waveform.pop_front();
            state.waveform.push_back(level);
        }

        AppEvent::TranscriptPartial(t) => {
            state.current_transcript = t;
        }

        AppEvent::TranscriptFinal(t) => {
            state.current_transcript.clear();
            if t.is_empty() {
                state.status = Status::Idle;
                return;
            }
            state.conversation.push(Message::User(t.clone()));
            state.status = Status::Thinking;
            state.scroll_offset = 0;

            let cfg = config.clone();
            let history = state.conversation.clone();
            let ev = event_tx.clone();
            tokio::spawn(crate::llm::complete(cfg, history, ev));
        }

        AppEvent::LlmToken(token) => {
            state.current_response.push_str(&token);
        }

        AppEvent::LlmComplete => {
            let response = std::mem::take(&mut state.current_response);
            if response.is_empty() {
                state.status = Status::Idle;
                return;
            }
            state.conversation.push(Message::Assistant(response.clone()));
            state.status = Status::Speaking;

            let cfg = config.clone();
            let ev = event_tx.clone();
            tokio::spawn(crate::tts::speak(cfg, response, ev));
        }

        AppEvent::TtsComplete => {
            state.status = Status::Idle;
        }

        AppEvent::Error(e) => {
            state.error = Some(e);
            state.status = Status::Idle;
        }
    }
}

// ── Rendering ──────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, state: &AppState) {
    let area = f.area();

    // Root background
    f.render_widget(Block::default().style(Style::default().bg(BG)), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(5),     // conversation
            Constraint::Length(3),  // active line (transcript / streaming / error)
            Constraint::Length(3),  // waveform
            Constraint::Length(3),  // footer
        ])
        .split(area);

    render_header(f, chunks[0], state);
    render_conversation(f, chunks[1], state);
    render_active(f, chunks[2], state);
    render_waveform(f, chunks[3], state);
    render_footer(f, chunks[4]);
}

fn render_header(f: &mut Frame, area: Rect, state: &AppState) {
    let sc = state.status.color();
    let status_span = Line::from(vec![
        Span::styled(" ◉ ", Style::default().fg(sc).add_modifier(Modifier::BOLD)),
        Span::styled(state.status.label(), Style::default().fg(sc).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ]);

    let title = Paragraph::new(Line::from(vec![
        Span::styled("  ◈ BRAINIAC", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
        Span::styled(" v0.1", Style::default().fg(DIM)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(CYAN))
            .title(status_span)
            .title_alignment(ratatui::layout::Alignment::Right),
    )
    .style(Style::default().bg(BG));

    f.render_widget(title, area);
}

fn render_conversation(f: &mut Frame, area: Rect, state: &AppState) {
    let items: Vec<ListItem> = state
        .conversation
        .iter()
        .map(|msg| match msg {
            Message::User(text) => ListItem::new(vec![
                Line::from(Span::styled(
                    "▸ You",
                    Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(format!("  {text}"), Style::default().fg(WHITE))),
                Line::from(""),
            ]),
            Message::Assistant(text) => ListItem::new(vec![
                Line::from(Span::styled(
                    "◈ Brainiac",
                    Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!("  {text}"),
                    Style::default().fg(Color::Rgb(160, 160, 190)),
                )),
                Line::from(""),
            ]),
        })
        .collect();

    // Auto-scroll to show latest messages
    let visible_items = (area.height.saturating_sub(2) / 3).max(1) as usize;
    let total = items.len();
    let start = if total > visible_items {
        total - visible_items
    } else {
        0
    };
    let visible: Vec<ListItem> = items.into_iter().skip(start).collect();

    let list = List::new(visible)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DIM))
                .title(Span::styled(
                    " conversation ",
                    Style::default().fg(DIM),
                )),
        )
        .style(Style::default().bg(BG));

    f.render_widget(list, area);
}

fn render_active(f: &mut Frame, area: Rect, state: &AppState) {
    // Show error, or the active streaming content depending on status
    let (content, color, title): (&str, Color, &str) = if let Some(ref err) = state.error {
        if err == "__quit__" {
            return;
        }
        (err.as_str(), Color::Red, " error ")
    } else {
        match state.status {
            Status::Listening => (&state.current_transcript, YELLOW, " transcript "),
            Status::Thinking => (&state.current_response, MAGENTA, " generating "),
            Status::Speaking => (&state.current_response, CYAN, " speaking "),
            Status::Idle => return,
        }
    };

    let style = Style::default().fg(color);
    let widget = Paragraph::new(content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(style)
                .title(Span::styled(title, style)),
        )
        .style(style)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(color).bg(BG));

    f.render_widget(widget, area);
}

fn render_waveform(f: &mut Frame, area: Rect, state: &AppState) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let bar_color = if state.status == Status::Listening {
        YELLOW
    } else {
        DIM
    };

    let bar: String = state
        .waveform
        .iter()
        .take(inner_w)
        .map(|&l| {
            if l > 0.75 { "█" }
            else if l > 0.50 { "▓" }
            else if l > 0.25 { "▒" }
            else if l > 0.08 { "░" }
            else { " " }
        })
        .collect();

    let widget = Paragraph::new(Span::styled(bar, Style::default().fg(bar_color)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DIM))
                .title(Span::styled(" mic ", Style::default().fg(DIM))),
        )
        .style(Style::default().bg(BG));

    f.render_widget(widget, area);
}

fn render_footer(f: &mut Frame, area: Rect) {
    let widget = Paragraph::new(Line::from(vec![
        Span::styled("  [SPACE/ENTER] ", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
        Span::styled("push-to-talk", Style::default().fg(DIM)),
        Span::styled("   [C] ", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
        Span::styled("clear", Style::default().fg(DIM)),
        Span::styled("   [↑↓] ", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
        Span::styled("scroll", Style::default().fg(DIM)),
        Span::styled("   [Q/ESC] ", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
        Span::styled("quit  ", Style::default().fg(DIM)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(DIM)),
    )
    .style(Style::default().bg(BG));

    f.render_widget(widget, area);
}
