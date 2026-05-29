use anyhow::Context;

#[derive(Clone, Debug)]
pub struct Config {
    pub deepgram_api_key: String,
    pub deepseek_api_key: String,
    pub deepseek_base_url: String,
    pub deepseek_model: String,
    pub tts_voice: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();
        Ok(Self {
            deepgram_api_key: std::env::var("DEEPGRAM_API_KEY")
                .context("DEEPGRAM_API_KEY not set")?,
            deepseek_api_key: std::env::var("DEEPSEEK_API_KEY")
                .context("DEEPSEEK_API_KEY not set")?,
            deepseek_base_url: std::env::var("DEEPSEEK_BASE_URL")
                .unwrap_or_else(|_| "https://api.deepseek.com".to_string()),
            deepseek_model: std::env::var("DEEPSEEK_MODEL")
                .unwrap_or_else(|_| "deepseek-chat".to_string()),
            tts_voice: std::env::var("TTS_VOICE")
                .unwrap_or_else(|_| "aura-asteria-en".to_string()),
        })
    }
}
