//! Open a captured image's workflow in the *running* ComfyUI by programmatically
//! "dropping" the image into it.
//!
//! ComfyUI has no server API / launch arg to open a workflow, so the flow is:
//!   1. ensure the image is a PNG that carries the workflow (embed it if missing);
//!   2. upload it to ComfyUI's input dir via `POST /upload/image`;
//!   3. open the browser at `…/?synflow=<view-url>`, which the bundled frontend
//!      bridge extension (`extensions/synthetrix/open.js`) fetches and feeds to
//!      ComfyUI's own `app.handleFile` — the exact drag-drop import path.

use crate::pngmeta;
use std::path::Path;

const COMFY: &str = "http://127.0.0.1:8188";
const PNG_SIG: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Percent-encode a string for use as a URL query value.
fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// Produce a PNG carrying exactly `wf` as its workflow. Decodes to PNG if needed,
/// strips any existing `workflow`/`prompt` chunks, then embeds `wf` (UI graphs ->
/// `workflow` chunk, API graphs -> `prompt`). Re-embedding (rather than trusting
/// the original chunk) is what lets the model-patched graph win for *harvested*
/// images too, not just synthesized ones.
fn prepare_png(bytes: &[u8], wf: &str) -> Result<Vec<u8>, String> {
    let is_png = bytes.len() > 8 && &bytes[..8] == PNG_SIG;
    let png = if is_png {
        bytes.to_vec()
    } else {
        let img = image::load_from_memory(bytes).map_err(|e| format!("decode image: {e}"))?;
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| format!("encode png: {e}"))?;
        buf.into_inner()
    };
    let stripped = pngmeta::strip_text_chunks(&png, &["workflow", "prompt"]).unwrap_or(png);
    // UI graphs have a top-level "nodes" array; API graphs don't.
    let is_ui = serde_json::from_str::<serde_json::Value>(wf)
        .ok()
        .and_then(|v| v.get("nodes").cloned())
        .is_some();
    let keyword = if is_ui { "workflow" } else { "prompt" };
    pngmeta::insert_text_chunk(&stripped, keyword, wf)
        .ok_or_else(|| "failed to embed workflow".into())
}

/// Open `image_path`'s workflow in the running ComfyUI. Blocking (run off the UI
/// thread). `wf_json` is the workflow text to embed if the PNG lacks one.
pub fn open_in_comfy(image_path: &str, wf_json: Option<&str>) -> Result<(), String> {
    let bytes = std::fs::read(image_path).map_err(|e| format!("read image: {e}"))?;
    let client = reqwest::blocking::Client::new();
    // Repoint the workflow's model loaders at an installed model, so the graph
    // doesn't open with an empty/invalid checkpoint widget ("fails to show the
    // model"). Best-effort: if ComfyUI doesn't answer, embed the workflow as-is.
    let patched = wf_json.map(|w| patch_model_names(&client, w));
    let png = match patched.as_deref() {
        Some(wf) => prepare_png(&bytes, wf)?,
        None => bytes.clone(),
    };

    let stem = Path::new(image_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("synthetrix");
    let upload_name = format!("{stem}.png");

    let part = reqwest::blocking::multipart::Part::bytes(png)
        .file_name(upload_name.clone())
        .mime_str("image/png")
        .map_err(|e| e.to_string())?;
    let form = reqwest::blocking::multipart::Form::new()
        .part("image", part)
        .text("subfolder", "synthetrix")
        .text("type", "input")
        .text("overwrite", "true");

    let resp = client
        .post(format!("{COMFY}/upload/image"))
        .multipart(form)
        .send()
        .map_err(|e| format!("upload to ComfyUI failed (running on :8188?): {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "ComfyUI /upload/image returned HTTP {}",
            resp.status()
        ));
    }
    let j: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let name = j
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&upload_name);
    let subfolder = j
        .get("subfolder")
        .and_then(|v| v.as_str())
        .unwrap_or("synthetrix");
    let typ = j.get("type").and_then(|v| v.as_str()).unwrap_or("input");

    let view = format!(
        "/api/view?filename={}&type={}&subfolder={}",
        enc(name),
        enc(typ),
        enc(subfolder)
    );
    let url = format!("{COMFY}/?synflow={}&synname={}", enc(&view), enc(name));
    open_url(&url)
}

/// The installed values for a loader field, e.g. CheckpointLoaderSimple/ckpt_name.
fn obj_enum(client: &reqwest::blocking::Client, node: &str, field: &str) -> Vec<String> {
    let resp = match client
        .get(format!("{COMFY}/object_info/{node}"))
        .send()
        .ok()
        .and_then(|r| r.error_for_status().ok())
    {
        Some(r) => r,
        None => return Vec::new(),
    };
    let j: serde_json::Value = match resp.json() {
        Ok(j) => j,
        Err(_) => return Vec::new(),
    };
    j.get(node)
        .and_then(|n| n.get("input"))
        .and_then(|i| i.get("required"))
        .and_then(|r| r.get(field))
        .and_then(|f| f.get(0))
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Coarse model architecture family, so a missing model is swapped for a
/// *compatible* installed one (a Flux graph shouldn't get an SDXL checkpoint).
fn family(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("flux") {
        "flux"
    } else if n.contains("pony") {
        "pony"
    } else if n.contains("illustrious") || n.contains("ilxl") {
        "illustrious"
    } else if n.contains("sdxl") || n.contains("xl") {
        "xl"
    } else if n.contains("hunyuan") {
        "hunyuan"
    } else if n.contains("wan") {
        "wan"
    } else {
        "sd"
    }
}

/// Pick a replacement when `current` isn't installed. Prefer an exact name match,
/// then a same-architecture installed model. None = keep current (already valid,
/// nothing installed, or no compatible swap — leaving ComfyUI's honest "missing
/// model" rather than silently loading an incompatible checkpoint).
fn pick_model(current: &str, installed: &[String]) -> Option<String> {
    if installed.is_empty() || installed.iter().any(|m| m == current) {
        return None;
    }
    let fam = family(current);
    installed.iter().find(|m| family(m) == fam).cloned()
}

/// Rewrite CheckpointLoaderSimple/UNETLoader model names in a workflow (UI or API
/// format) to models ComfyUI actually has installed.
fn patch_model_names(client: &reqwest::blocking::Client, wf: &str) -> String {
    let ckpts = obj_enum(client, "CheckpointLoaderSimple", "ckpt_name");
    let unets = obj_enum(client, "UNETLoader", "unet_name");
    if ckpts.is_empty() && unets.is_empty() {
        return wf.to_string();
    }
    let mut v: serde_json::Value = match serde_json::from_str(wf) {
        Ok(v) => v,
        Err(_) => return wf.to_string(),
    };
    let is_ui = v.get("nodes").map(|n| n.is_array()).unwrap_or(false);
    if is_ui {
        if let Some(nodes) = v.get_mut("nodes").and_then(|n| n.as_array_mut()) {
            for n in nodes.iter_mut() {
                let t = n
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let list = if t.contains("CheckpointLoader") {
                    &ckpts
                } else if t == "UNETLoader" {
                    &unets
                } else {
                    continue;
                };
                if let Some(first) = n
                    .get_mut("widgets_values")
                    .and_then(|w| w.as_array_mut())
                    .and_then(|a| a.get_mut(0))
                {
                    if let Some(cur) = first.as_str() {
                        if let Some(rep) = pick_model(cur, list) {
                            *first = serde_json::Value::String(rep);
                        }
                    }
                }
            }
        }
    } else if let Some(obj) = v.as_object_mut() {
        for (_, n) in obj.iter_mut() {
            let t = n
                .get("class_type")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let (field, list) = if t.contains("CheckpointLoader") {
                ("ckpt_name", &ckpts)
            } else if t == "UNETLoader" {
                ("unet_name", &unets)
            } else {
                continue;
            };
            if let Some(inp) = n.get_mut("inputs").and_then(|i| i.as_object_mut()) {
                if let Some(cur) = inp.get(field).and_then(|x| x.as_str()).map(String::from) {
                    if let Some(rep) = pick_model(&cur, list) {
                        inp.insert(field.to_string(), serde_json::Value::String(rep));
                    }
                }
            }
        }
    }
    serde_json::to_string(&v).unwrap_or_else(|_| wf.to_string())
}

#[cfg(target_os = "windows")]
fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open browser: {e}"))
}

#[cfg(not(target_os = "windows"))]
fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open browser: {e}"))
}
