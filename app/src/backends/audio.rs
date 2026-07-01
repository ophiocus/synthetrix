//! ElevenLabs text→speech backend (Phase 5). Turns a line of text into an MP3
//! voice clip. Gated on a configured API key — with none, callers get a clear
//! `blocked` reason rather than a silent no-op.
//!
//! REST surface: `POST /v1/text-to-speech/{voice_id}` with an `xi-api-key`
//! header returns audio bytes (audio/mpeg) directly.

use std::time::Duration;

pub struct ElevenLabs {
    key: String,
    voice: String,
    http: reqwest::blocking::Client,
}

impl ElevenLabs {
    pub fn new(key: &str, voice: &str) -> Self {
        Self {
            key: key.trim().to_string(),
            voice: voice.trim().to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new()),
        }
    }

    pub fn configured(&self) -> bool {
        !self.key.is_empty() && !self.voice.is_empty()
    }

    /// Synthesize `text` into MP3 bytes.
    pub fn text_to_speech(
        &self,
        text: &str,
        progress: &mut dyn FnMut(f32, &str),
    ) -> Result<Vec<u8>, String> {
        if !self.configured() {
            return Err("ElevenLabs key/voice not set (Settings)".into());
        }
        progress(0.2, "synthesizing voice");
        let resp = self
            .http
            .post(format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{}",
                self.voice
            ))
            .header("xi-api-key", &self.key)
            .header("accept", "audio/mpeg")
            .json(&serde_json::json!({
                "text": text,
                "model_id": "eleven_multilingual_v2",
                "voice_settings": { "stability": 0.5, "similarity_boost": 0.75 }
            }))
            .send()
            .map_err(|e| format!("elevenlabs: {e}"))?;
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().unwrap_or_default();
            return Err(format!(
                "elevenlabs {code}: {}",
                body.chars().take(200).collect::<String>()
            ));
        }
        progress(0.9, "downloading audio");
        let bytes = resp.bytes().map_err(|e| e.to_string())?.to_vec();
        if bytes.is_empty() {
            return Err("elevenlabs returned no audio".into());
        }
        Ok(bytes)
    }
}
