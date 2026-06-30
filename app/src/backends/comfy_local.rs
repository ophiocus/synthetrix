//! Local ComfyUI text→image backend over the HTTP API (`/prompt`, `/history`,
//! `/view`). Builds a standard checkpoint text2img API graph, submits it, polls
//! history to completion, and returns the produced image bytes + metadata.

use super::{Backend, GenRequest, GenResult};
use serde_json::{json, Value};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct ComfyLocal {
    base: String,
    http: reqwest::blocking::Client,
}

impl ComfyLocal {
    pub fn new(base_url: &str) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new()),
        }
    }
}

fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn pseudo_seed() -> i64 {
    (nanos() as u64 % 2_147_483_647) as i64
}

/// A standard checkpoint text2img graph in ComfyUI API format.
fn build_graph(req: &GenRequest, seed: i64) -> Value {
    json!({
        "3": {"class_type": "KSampler", "inputs": {
            "seed": seed, "steps": req.steps, "cfg": req.cfg,
            "sampler_name": req.sampler, "scheduler": req.scheduler, "denoise": 1.0,
            "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0],
            "latent_image": ["5", 0]}},
        "4": {"class_type": "CheckpointLoaderSimple",
              "inputs": {"ckpt_name": req.model}},
        "5": {"class_type": "EmptyLatentImage",
              "inputs": {"width": req.width, "height": req.height, "batch_size": 1}},
        "6": {"class_type": "CLIPTextEncode",
              "inputs": {"text": req.prompt, "clip": ["4", 1]}},
        "7": {"class_type": "CLIPTextEncode",
              "inputs": {"text": req.negative, "clip": ["4", 1]}},
        "8": {"class_type": "VAEDecode",
              "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
        "9": {"class_type": "SaveImage",
              "inputs": {"filename_prefix": "synthetrix", "images": ["8", 0]}}
    })
}

impl Backend for ComfyLocal {
    fn id(&self) -> &'static str {
        "comfy_local"
    }

    fn generate_image(
        &mut self,
        req: &GenRequest,
        progress: &mut dyn FnMut(f32, &str),
    ) -> Result<GenResult, String> {
        let seed = if req.seed < 0 {
            pseudo_seed()
        } else {
            req.seed
        };
        let client_id = format!("synthetrix-{}", nanos());
        progress(0.05, "submitting to ComfyUI");

        let resp = self
            .http
            .post(format!("{}/prompt", self.base))
            .json(&json!({ "prompt": build_graph(req, seed), "client_id": client_id }))
            .send()
            .map_err(|e| format!("connect ComfyUI ({}): {e}", self.base))?;
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().unwrap_or_default();
            return Err(format!(
                "ComfyUI /prompt {code}: {}",
                body.chars().take(300).collect::<String>()
            ));
        }
        let pid = resp
            .json::<Value>()
            .ok()
            .and_then(|v| {
                v.get("prompt_id")
                    .and_then(|x| x.as_str())
                    .map(String::from)
            })
            .ok_or("ComfyUI did not return a prompt_id")?;

        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(900));
            if start.elapsed() > Duration::from_secs(600) {
                return Err("ComfyUI generation timed out (10 min)".into());
            }
            let hv: Value = self
                .http
                .get(format!("{}/history/{}", self.base, pid))
                .send()
                .and_then(|r| r.json())
                .map_err(|e| e.to_string())?;
            let Some(entry) = hv.get(&pid) else {
                progress(
                    (0.1 + start.elapsed().as_secs_f32() / 90.0).min(0.9),
                    "queued",
                );
                continue;
            };
            progress(
                (0.1 + start.elapsed().as_secs_f32() / 90.0).min(0.9),
                "generating",
            );

            let img = entry
                .get("outputs")
                .and_then(|o| o.as_object())
                .and_then(|o| {
                    o.values().find_map(|n| {
                        n.get("images")
                            .and_then(|i| i.as_array())
                            .and_then(|a| a.first())
                    })
                });
            let Some(img) = img else {
                if entry
                    .get("status")
                    .and_then(|s| s.get("status_str"))
                    .and_then(|s| s.as_str())
                    == Some("error")
                {
                    return Err("ComfyUI reported an error (check its console)".into());
                }
                continue;
            };

            let filename = img.get("filename").and_then(|x| x.as_str()).unwrap_or("");
            let subfolder = img.get("subfolder").and_then(|x| x.as_str()).unwrap_or("");
            let typ = img.get("type").and_then(|x| x.as_str()).unwrap_or("output");
            progress(0.95, "downloading image");
            let r = self
                .http
                .get(format!("{}/view", self.base))
                .query(&[
                    ("filename", filename),
                    ("subfolder", subfolder),
                    ("type", typ),
                ])
                .send()
                .map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(format!("ComfyUI /view {}", r.status().as_u16()));
            }
            let ct = r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("image/png")
                .split(';')
                .next()
                .unwrap_or("image/png")
                .to_string();
            let bytes = r.bytes().map_err(|e| e.to_string())?.to_vec();
            let meta = json!({
                "backend": "comfy_local", "model": req.model, "prompt": req.prompt,
                "negative": req.negative, "width": req.width, "height": req.height,
                "steps": req.steps, "cfg": req.cfg, "sampler": req.sampler,
                "scheduler": req.scheduler, "seed": seed, "comfy_filename": filename
            });
            return Ok(GenResult {
                bytes,
                content_type: ct,
                seed,
                meta,
            });
        }
    }
}
