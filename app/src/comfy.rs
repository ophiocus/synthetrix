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

/// Decode to PNG if needed, then ensure it carries the workflow. `wf_json` is the
/// workflow to embed when the image has none (UI format -> `workflow` chunk, API
/// format -> `prompt` chunk).
fn ensure_png_with_workflow(bytes: &[u8], wf_json: Option<&str>) -> Result<Vec<u8>, String> {
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
    if pngmeta::has_embedded_workflow(&png) {
        return Ok(png);
    }
    let wf = wf_json.ok_or("image has no embedded workflow and none was provided")?;
    // UI graphs have a top-level "nodes" array; API graphs don't.
    let is_ui = serde_json::from_str::<serde_json::Value>(wf)
        .ok()
        .and_then(|v| v.get("nodes").cloned())
        .is_some();
    let keyword = if is_ui { "workflow" } else { "prompt" };
    pngmeta::insert_text_chunk(&png, keyword, wf).ok_or_else(|| "failed to embed workflow".into())
}

/// Open `image_path`'s workflow in the running ComfyUI. Blocking (run off the UI
/// thread). `wf_json` is the workflow text to embed if the PNG lacks one.
pub fn open_in_comfy(image_path: &str, wf_json: Option<&str>) -> Result<(), String> {
    let bytes = std::fs::read(image_path).map_err(|e| format!("read image: {e}"))?;
    let png = ensure_png_with_workflow(&bytes, wf_json)?;

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

    let client = reqwest::blocking::Client::new();
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
