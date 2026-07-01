//! Tripo image→3D-mesh backend (Phase 5). Uploads a concept image, opens an
//! image-to-model task, polls to completion, and downloads the produced GLB.
//! Gated on a configured API key — with none, callers get a clear `blocked`
//! reason rather than a silent no-op.
//!
//! REST surface (Tripo v2 `api.tripo3d.ai`): `POST /v2/openapi/upload` →
//! image_token; `POST /v2/openapi/task {type:image_to_model}` → task_id;
//! `GET /v2/openapi/task/{id}` → status + model url; then GET the url.

use serde_json::Value;
use std::time::{Duration, Instant};

pub struct Tripo {
    key: String,
    http: reqwest::blocking::Client,
}

impl Tripo {
    pub fn new(key: &str) -> Self {
        Self {
            key: key.trim().to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new()),
        }
    }

    pub fn configured(&self) -> bool {
        !self.key.is_empty()
    }

    /// Turn a concept image into a GLB mesh. Returns the GLB bytes.
    /// `progress(frac, note)` reports task progress.
    pub fn image_to_mesh(
        &self,
        image: &[u8],
        content_type: &str,
        progress: &mut dyn FnMut(f32, &str),
    ) -> Result<Vec<u8>, String> {
        if !self.configured() {
            return Err("Tripo key not set (Settings)".into());
        }
        progress(0.05, "uploading image to Tripo");
        let ext = match content_type {
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => "png",
        };
        let part = reqwest::blocking::multipart::Part::bytes(image.to_vec())
            .file_name(format!("concept.{ext}"))
            .mime_str(content_type)
            .map_err(|e| e.to_string())?;
        let form = reqwest::blocking::multipart::Form::new().part("file", part);
        let up: Value = self
            .http
            .post("https://api.tripo3d.ai/v2/openapi/upload")
            .bearer_auth(&self.key)
            .multipart(form)
            .send()
            .map_err(|e| format!("tripo upload: {e}"))?
            .json()
            .map_err(|e| e.to_string())?;
        let image_token = up
            .get("data")
            .and_then(|d| d.get("image_token"))
            .and_then(|t| t.as_str())
            .ok_or("tripo upload: no image_token")?
            .to_string();

        progress(0.2, "opening image_to_model task");
        let task: Value = self
            .http
            .post("https://api.tripo3d.ai/v2/openapi/task")
            .bearer_auth(&self.key)
            .json(&serde_json::json!({
                "type": "image_to_model",
                "file": { "type": ext, "file_token": image_token }
            }))
            .send()
            .map_err(|e| format!("tripo task: {e}"))?
            .json()
            .map_err(|e| e.to_string())?;
        let task_id = task
            .get("data")
            .and_then(|d| d.get("task_id"))
            .and_then(|t| t.as_str())
            .ok_or("tripo task: no task_id")?
            .to_string();

        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(3));
            if start.elapsed() > Duration::from_secs(600) {
                return Err("tripo task timed out (10 min)".into());
            }
            let st: Value = self
                .http
                .get(format!("https://api.tripo3d.ai/v2/openapi/task/{task_id}"))
                .bearer_auth(&self.key)
                .send()
                .and_then(|r| r.json())
                .map_err(|e| e.to_string())?;
            let data = st.get("data");
            let status = data
                .and_then(|d| d.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let pct = data
                .and_then(|d| d.get("progress"))
                .and_then(|p| p.as_f64())
                .unwrap_or(0.0) as f32;
            progress((0.2 + pct / 100.0 * 0.7).min(0.9), "meshing");
            match status {
                "success" => {
                    let url = data
                        .and_then(|d| d.get("output"))
                        .and_then(|o| o.get("pbr_model").or_else(|| o.get("model")))
                        .and_then(|m| m.as_str())
                        .ok_or("tripo success but no model url")?;
                    progress(0.95, "downloading mesh");
                    let bytes = self
                        .http
                        .get(url)
                        .send()
                        .map_err(|e| e.to_string())?
                        .bytes()
                        .map_err(|e| e.to_string())?
                        .to_vec();
                    return Ok(bytes);
                }
                "failed" | "cancelled" | "banned" | "expired" => {
                    return Err(format!("tripo task {status}"));
                }
                _ => continue, // queued | running
            }
        }
    }
}
