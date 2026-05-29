# Brainiac

A voice-activated AI assistant that runs in your terminal. Press Space to speak, get a spoken response back. No GUI, no browser — just a TUI and your mic.

```
  ◈ BRAINIAC v0.1                                     ◉ LISTENING

 ┌ conversation ──────────────────────────────────────────────────┐
 │ ▸ You                                                          │
 │   What's the time complexity of Dijkstra's algorithm?         │
 │                                                                │
 │ ◈ Brainiac                                                     │
 │   O((V + E) log V) with a binary heap. E log V dominates on   │
 │   dense graphs. Use Bellman-Ford if you have negative edges.  │
 └────────────────────────────────────────────────────────────────┘
 ┌ transcript ────────────────────────────────────────────────────┐
 │ and what about A-star...                                       │
 └────────────────────────────────────────────────────────────────┘
 ┌ mic ───────────────────────────────────────────────────────────┐
 │ ░░▒▒▓▓██████▓▓▒▒░░  ░░▒▒▒▓▓██▓▒░                              │
 └────────────────────────────────────────────────────────────────┘
  [SPACE/ENTER] push-to-talk   [C] clear   [↑↓] scroll   [Q/ESC] quit
```

## What it does

Brainiac is a real-time voice ↔ AI pipeline:

1. **Mic** — captures audio from your default input device via [cpal](https://github.com/RustAudio/cpal)
2. **STT** — streams raw PCM to [Deepgram](https://deepgram.com) over a WebSocket, receiving partial and final transcripts in real time
3. **LLM** — sends the completed transcript plus conversation history to [DeepSeek](https://www.deepseek.com) and streams tokens back as they arrive
4. **TTS** — sends the full response to [Deepgram Aura](https://deepgram.com/product/text-to-speech) and plays the returned audio through your speakers via [rodio](https://github.com/RustAudio/rodio)
5. **TUI** — a [Ratatui](https://ratatui.rs) terminal UI shows the live waveform, partial transcript, streaming response, and conversation history

The interaction is toggle-based: one Space press to start recording, another to stop. Deepgram flushes its final transcript after you stop, so you don't need to hold a key.

## Architecture

```
[SPACE] → mic thread (cpal)
        → audio channel (tokio unbounded)
        → stt::stream task (Deepgram WebSocket)
        → TranscriptFinal event
        → llm::complete task (DeepSeek SSE)
        → LlmToken events (streaming to screen)
        → LlmComplete
        → tts::speak (Deepgram Aura → rodio OS thread)
        → TtsComplete
        → Idle
```

A few design constraints worth knowing:

- **Mic on a dedicated OS thread** — `cpal::Stream` is `!Send` on macOS CoreAudio. The thread owns the stream; the TUI sends start/stop commands over `std::sync::mpsc`. No `unsafe impl Send`.
- **Audio channel as lifecycle signal** — dropping the `UnboundedSender` on stop propagates as `None` to the STT task, which sends a WebSocket Close and waits for Deepgram's final transcript before exiting. Clean shutdown without cancellation tokens.
- **TTS on its own thread** — `rodio::OutputStream` is `!Send`. A `tokio::sync::oneshot` bridges playback completion back to the async runtime.

## Requirements

- **Rust** 1.75+ (`rustup update stable`)
- **macOS** — tested on macOS. Linux should work; Windows is untested.
- A **microphone** connected and set as the default input device
- A **[Deepgram](https://console.deepgram.com)** account (free tier works — STT and TTS)
- A **[DeepSeek](https://platform.deepseek.com)** account (API access)

## Setup

### 1. Clone and build

```bash
git clone https://github.com/yourname/brainiac
cd brainiac
cargo build --release
```

### 2. Configure API keys

Copy the example env file and fill in your keys:

```bash
cp src/env.example .env
```

Edit `.env`:

```env
DEEPGRAM_API_KEY=your_deepgram_key_here
DEEPSEEK_API_KEY=your_deepseek_key_here

# Optional — defaults shown
DEEPSEEK_BASE_URL=https://api.deepseek.com
DEEPSEEK_MODEL=deepseek-chat
TTS_VOICE=aura-asteria-en
```

**Getting keys:**
- Deepgram: [console.deepgram.com](https://console.deepgram.com) → API Keys → Create a new key
- DeepSeek: [platform.deepseek.com](https://platform.deepseek.com) → API Keys → Create new secret key

### 3. macOS microphone permission

The first time you run Brainiac, macOS will prompt for microphone access. Grant it. If you accidentally denied it, go to **System Settings → Privacy & Security → Microphone** and enable your terminal emulator.

### 4. Run

```bash
cargo run --release
# or after build:
./target/release/brainiac
```

## Usage

| Key | Action |
|---|---|
| `Space` or `Enter` | Start recording (press once to begin, once more to stop and send) |
| `C` | Clear conversation history |
| `↑` / `↓` | Scroll conversation |
| `Q` or `Esc` | Quit |

**Typical flow:**

1. Press `Space` — status changes to **LISTENING**, waveform activates
2. Speak your question
3. Press `Space` again — mic stops, Deepgram flushes the final transcript
4. Status changes to **THINKING** as DeepSeek streams its response token by token
5. Status changes to **SPEAKING** as Deepgram Aura reads the response aloud
6. Returns to **IDLE** — ready for the next turn

Conversation history is preserved across turns so you can ask follow-up questions naturally. Press `C` to start a fresh session.

## Configuration

| Variable | Default | Description |
|---|---|---|
| `DEEPGRAM_API_KEY` | required | Deepgram API key (used for both STT and TTS) |
| `DEEPSEEK_API_KEY` | required | DeepSeek API key |
| `DEEPSEEK_BASE_URL` | `https://api.deepseek.com` | Base URL for the DeepSeek API (swap for any OpenAI-compatible endpoint) |
| `DEEPSEEK_MODEL` | `deepseek-chat` | Model name |
| `TTS_VOICE` | `aura-asteria-en` | Deepgram Aura voice |

**Available Deepgram Aura voices** (partial list): `aura-asteria-en`, `aura-luna-en`, `aura-stella-en`, `aura-athena-en`, `aura-hera-en`, `aura-orion-en`, `aura-arcas-en`, `aura-perseus-en`. Full list in [Deepgram's docs](https://developers.deepgram.com/docs/tts-models).

Since `DEEPSEEK_BASE_URL` is configurable, you can point Brainiac at any OpenAI-compatible API — Groq, Together, a local Ollama instance, etc. — by changing that variable and setting the appropriate key.

## Dependencies

| Crate | Purpose |
|---|---|
| `tokio` | Async runtime |
| `cpal` | Cross-platform audio capture |
| `rodio` | Audio playback |
| `tokio-tungstenite` | WebSocket for Deepgram STT |
| `reqwest` | HTTP for DeepSeek SSE and Deepgram TTS |
| `ratatui` + `crossterm` | Terminal UI |
| `serde` / `serde_json` | JSON serialization |
| `dotenvy` | `.env` file loading |
| `anyhow` | Error handling |

## Project structure

```
src/
├── main.rs      — entry point; loads config and starts TUI
├── config.rs    — reads env vars into Config struct
├── events.rs    — AppEvent enum (the message bus between all layers)
├── audio.rs     — cpal mic management on a dedicated OS thread
├── stt.rs       — Deepgram WebSocket streaming
├── llm.rs       — DeepSeek SSE streaming + system prompt
├── tts.rs       — Deepgram Aura HTTP + rodio playback thread
└── tui.rs       — Ratatui render loop + state machine
```
