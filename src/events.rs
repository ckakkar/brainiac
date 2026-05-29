#[derive(Debug, Clone)]
pub enum AppEvent {
    // Mic audio level for waveform (only fires while recording)
    AudioLevel(f32),

    // Deepgram STT
    TranscriptPartial(String),
    TranscriptFinal(String),

    // DeepSeek LLM (streaming)
    LlmToken(String),
    LlmComplete,

    // Deepgram Aura TTS
    TtsComplete,

    // Generic error surface
    Error(String),
}
