//! Service router — the media bus. One interface to dispatch generation jobs to
//! pluggable backends. Phase 1 ships the local-ComfyUI image backend; Comfy
//! Cloud (same `/prompt` REST), OpenArt MCP, Tripo, and audio land in later
//! phases behind this same trait.

pub mod comfy_local;

/// A text→image generation request (the common subset across backends).
#[derive(Clone, Debug)]
pub struct GenRequest {
    pub prompt: String,
    pub negative: String,
    pub model: String, // checkpoint filename as the runtime sees it
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg: f32,
    pub sampler: String,
    pub scheduler: String,
    pub seed: i64, // < 0 => random
}

impl Default for GenRequest {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            negative: String::new(),
            model: String::new(),
            width: 1024,
            height: 1024,
            steps: 25,
            cfg: 7.0,
            sampler: "dpmpp_2m".into(),
            scheduler: "karras".into(),
            seed: -1,
        }
    }
}

/// The product of a successful generation: raw bytes + the resolved metadata to
/// persist as a provenance sidecar.
pub struct GenResult {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub seed: i64,
    pub meta: serde_json::Value,
}

/// A generation backend. `progress(frac 0..1, note)` is called as work advances.
pub trait Backend {
    fn id(&self) -> &'static str;
    fn generate_image(
        &mut self,
        req: &GenRequest,
        progress: &mut dyn FnMut(f32, &str),
    ) -> Result<GenResult, String>;
}

/// Backend selector (persisted as a string in jobs/config). Future ids:
/// "comfy_cloud", "openart", "tripo" — all behind this same trait.
pub fn backend_for(_id: &str, comfy_url: &str) -> Box<dyn Backend> {
    Box::new(comfy_local::ComfyLocal::new(comfy_url))
}
