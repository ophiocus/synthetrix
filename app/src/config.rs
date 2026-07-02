use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One game IP / project Synthetrix can focus on (the "currently open project").
/// The model vault (vault_root/catalog) is global/shared; everything per-IP —
/// lore, prompts, generated assets, metadata, releases — hangs off `lore_root`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub lore_root: String,   // lore-bible repo, e.g. I:/moar | I:/discarded
    pub engine_root: String, // UE5 project root (may be empty), e.g. F:/moarvibe
    pub asset_vault: String, // generated-asset vault; empty => <lore_root>/.synthetrix/assets
}

impl Project {
    pub fn synthetrix_dir(&self) -> PathBuf {
        Path::new(&self.lore_root).join(".synthetrix")
    }
    /// Per-IP project DB (lore index, prompt matrix, assets, jobs, releases).
    pub fn project_db_path(&self) -> PathBuf {
        self.synthetrix_dir().join("project.sqlite")
    }
    pub fn asset_vault_path(&self) -> PathBuf {
        if self.asset_vault.trim().is_empty() {
            self.synthetrix_dir().join("assets")
        } else {
            PathBuf::from(&self.asset_vault)
        }
    }
}

/// Seeded IPs (used on first run / when config carries none).
fn default_projects() -> Vec<Project> {
    vec![
        Project {
            name: "MOAR".into(),
            lore_root: "I:/moar".into(),
            engine_root: "F:/moarvibe".into(),
            asset_vault: String::new(),
        },
        Project {
            name: "DISCARDED".into(),
            lore_root: "I:/discarded".into(),
            engine_root: String::new(),
            asset_vault: String::new(),
        },
    ]
}

/// Persistent app config (window prefs + storage tiers + crawl knobs).
/// Lives at %APPDATA%/Synthetrix/config.json.
///
/// `#[serde(default)]` at the container level is load-bearing: a config written by
/// an older build is missing fields newer builds added. Without it, serde fails
/// the whole deserialize on any absent field and `load()` falls back to
/// `Config::default()` — silently wiping user data (the token, tiers, projects)
/// on every upgrade. With it, absent fields fall back to their default and the
/// present ones (token!) are preserved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub dark_mode: bool,
    pub zoom: f32,

    // Per-IP project registry + the currently-open project (by name).
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub active_project: Option<String>,

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

    // Generation backends.
    #[serde(default = "default_comfy_url")]
    pub comfy_url: String, // local ComfyUI REST base
    /// Tripo API key for image→3D-mesh stages (empty => stage blocked).
    #[serde(default)]
    pub tripo_key: String,
    /// ElevenLabs API key for text→voice stages (empty => stage blocked).
    #[serde(default)]
    pub elevenlabs_key: String,
    /// ElevenLabs voice id used for voice stages.
    #[serde(default = "default_eleven_voice")]
    pub elevenlabs_voice: String,
}

fn default_comfy_url() -> String {
    "http://127.0.0.1:8188".into()
}

fn default_eleven_voice() -> String {
    // ElevenLabs' stock "Rachel" voice — a safe default until the IP sets its own.
    "21m00Tcm4TlvDq8ikWAM".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dark_mode: true,
            zoom: 1.0,
            projects: default_projects(),
            active_project: Some("MOAR".into()),
            vault_root: "H:/Models".into(),
            catalog_dir: "H:/Models/.civitai".into(),
            gallery_root: "H:/Models/.civitai/gallery".into(),
            nvme_root: "E:/model loader/ComfyUI/models".into(),
            token: String::new(),
            types: ["Checkpoint", "LORA", "LoCon", "TextualInversion"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            base_models: [
                "Flux.1 D",
                "Flux.1 S",
                "SDXL 1.0",
                "Pony",
                "Illustrious",
                "SD 1.5",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            top_n: 150,
            nsfw: true,
            per_model: 20,
            include_video: true,
            comfy_url: default_comfy_url(),
            tripo_key: String::new(),
            elevenlabs_key: String::new(),
            elevenlabs_voice: default_eleven_voice(),
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
        let mut cfg: Config = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Older configs predate the project registry — seed it.
        if cfg.projects.is_empty() {
            cfg.projects = default_projects();
        }
        if cfg.active_project.is_none() {
            cfg.active_project = cfg.projects.first().map(|p| p.name.clone());
        }
        cfg
    }

    /// The currently-open project, if any.
    pub fn active(&self) -> Option<&Project> {
        let name = self.active_project.as_ref()?;
        self.projects.iter().find(|p| &p.name == name)
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
            std::env::var("CIVITAI_TOKEN")
                .ok()
                .filter(|s| !s.is_empty())
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
