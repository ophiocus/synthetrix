use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persistent app config (window prefs + storage tiers + crawl knobs).
/// Lives at %APPDATA%/Synthetrix/config.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub dark_mode: bool,
    pub zoom: f32,

    // Storage tiers.
    pub vault_root: String,   // HDD vault: the authoritative model tree
    pub catalog_dir: String,  // catalog.sqlite + usage sidecars
    pub gallery_root: String, // example images + extracted workflows
    pub nvme_root: String,    // NVMe active-replica tree (ComfyUI models)

    // CivitAI access. Empty => read $CIVITAI_TOKEN at runtime.
    pub token: String,

    // Curated crawl.
    pub types: Vec<String>,
    pub base_models: Vec<String>,
    pub top_n: u32,
    pub nsfw: bool,

    // Per-model example images pulled on download.
    pub per_model: u32,
    pub include_video: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dark_mode: true,
            zoom: 1.0,
            vault_root: "H:/Models".into(),
            catalog_dir: "H:/Models/.civitai".into(),
            gallery_root: "H:/Models/.civitai/gallery".into(),
            nvme_root: "F:/tinyforge/ComfyUI/ComfyUI/models".into(),
            token: String::new(),
            types: ["Checkpoint", "LORA", "LoCon", "TextualInversion"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            base_models: [
                "Flux.1 D", "Flux.1 S", "SDXL 1.0", "Pony", "Illustrious", "SD 1.5",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            top_n: 150,
            nsfw: true,
            per_model: 20,
            include_video: true,
        }
    }
}

impl Config {
    pub fn dir() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join(crate::APP_NAME))
    }

    pub fn path() -> Option<PathBuf> {
        Self::dir().map(|p| p.join("config.json"))
    }

    pub fn load() -> Self {
        let Some(p) = Self::path() else {
            return Self::default();
        };
        std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(dir) = Self::dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        if let Some(p) = Self::path() {
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }

    /// Effective token: config value, else the CIVITAI_TOKEN env var.
    pub fn effective_token(&self) -> Option<String> {
        if !self.token.trim().is_empty() {
            Some(self.token.trim().to_string())
        } else {
            std::env::var("CIVITAI_TOKEN").ok().filter(|s| !s.is_empty())
        }
    }

    /// Local on-disk cache for Picker cover thumbnails.
    pub fn covers_dir(&self) -> PathBuf {
        std::path::Path::new(&self.gallery_root).join(".covers")
    }

    /// Vault/NVMe subfolder for a CivitAI model type.
    pub fn subdir_for(model_type: &str) -> &'static str {
        match model_type {
            "Checkpoint" => "checkpoints",
            "LORA" | "LoCon" => "loras",
            "TextualInversion" => "embeddings",
            "Controlnet" => "controlnet",
            "VAE" => "vae",
            "Upscaler" => "upscale_models",
            _ => "misc",
        }
    }
}
