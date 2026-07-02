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
use std::collections::HashSet;
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

/// Is a ComfyUI server answering on :8188? Fast preflight so the UI can tell the
/// user "start ComfyUI" instead of silently failing the upload. Connection-refused
/// is immediate on localhost; the timeout only bounds a wedged server.
pub fn is_running() -> bool {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .build()
        .and_then(|c| c.get(format!("{COMFY}/")).send())
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Open `image_path`'s workflow in the running ComfyUI. Blocking (run off the UI
/// thread). `wf_json` is the workflow text to embed if the PNG lacks one.
pub fn open_in_comfy(
    image_path: &str,
    wf_json: Option<&str>,
    vault_root: &str,
    nvme_root: &str,
) -> Result<(), String> {
    // Fail fast with an actionable message when ComfyUI isn't up: otherwise the
    // upload below errors deep in reqwest and (in a windowed app) that goes nowhere.
    if !is_running() {
        return Err("ComfyUI isn't running on :8188 — start it, then click again.".into());
    }
    let bytes = std::fs::read(image_path).map_err(|e| format!("read image: {e}"))?;
    let client = reqwest::blocking::Client::new();
    // Resolve the workflow's model loaders to a model ComfyUI can actually load:
    // keep it if installed, else find the same model in the vault and hotload it
    // (rewriting the name to the real file), else leave it (honest "missing").
    let patched = wf_json.map(|w| patch_model_names(&client, w, vault_root, nvme_root));
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
    // Only ?synflow= — no &synname (the view URL already carries filename=…, and
    // the `&` would be split by `cmd start` on Windows, truncating the URL). The
    // bridge reads the workflow from the file content, not the display name.
    let _ = name;
    let url = format!("{COMFY}/?synflow={}", enc(&view));
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

/// Tokenize a model filename into significant lowercase tokens, splitting on
/// non-alphanumerics AND camelCase / letter-digit boundaries, keeping tokens of
/// length >= 3. Lets us recognize the SAME model under a different filename:
/// "2758FluxAsianUtopian_v51KreaFp8Noclip.safetensors" and
/// "2758_hinaAsianFlux1-krea-dev_v51-fp8_noCLIP.safetensors" share
/// {2758, asian, flux, krea, noclip}.
/// The bare filename of a model reference: drops any author subfolder path, on
/// either separator ("GGUFFlux\\Z\\WIP\\Fux.gguf" -> "Fux.gguf", "flux/vae.st" ->
/// "vae.st"). ComfyUI enum values and vault files are matched on this.
fn base(name: &str) -> &str {
    name.rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(name)
}

fn tokens(name: &str) -> HashSet<String> {
    let name = base(name);
    let stem = name.rsplit_once('.').map(|(a, _)| a).unwrap_or(name);
    let mut out = HashSet::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    let mut flush = |cur: &mut String| {
        if cur.len() >= 3 {
            out.insert(cur.to_lowercase());
        }
        cur.clear();
    };
    for ch in stem.chars() {
        if !ch.is_ascii_alphanumeric() {
            flush(&mut cur);
            prev = None;
            continue;
        }
        if let Some(p) = prev {
            let boundary = (p.is_ascii_lowercase() && ch.is_ascii_uppercase())
                || (p.is_ascii_alphabetic() && ch.is_ascii_digit())
                || (p.is_ascii_digit() && ch.is_ascii_alphabetic());
            if boundary {
                flush(&mut cur);
            }
        }
        cur.push(ch);
        prev = Some(ch);
    }
    flush(&mut cur);
    out
}

/// Best name match for `wanted` among `candidates` by shared-token count, requiring
/// >= 3 shared significant tokens so a false match is unlikely.
fn best_match(wanted: &str, candidates: &[String]) -> Option<String> {
    let want = tokens(wanted);
    if want.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &String)> = None;
    for c in candidates {
        let score = tokens(c).intersection(&want).count();
        let better = match best {
            Some((s, _)) => score > s,
            None => true,
        };
        if score >= 3 && better {
            best = Some((score, c));
        }
    }
    best.map(|(_, c)| c.clone())
}

/// A model-name slot in a loader node: which widget index (UI graphs) / input
/// field (API graphs) carries a model filename, and the NVMe subdir the model is
/// hotloaded into (the subdir the *loader* reads from, regardless of where the
/// vault filed it).
struct Slot {
    ui_index: usize,
    field: &'static str,
    subdir: &'static str,
}

const fn slot(ui_index: usize, field: &'static str, subdir: &'static str) -> Slot {
    Slot {
        ui_index,
        field,
        subdir,
    }
}

/// The model-name slots for a loader class (UI + API). Covers the loaders a Flux/
/// SDXL workflow actually uses — checkpoints, diffusion/UNET (incl. GGUF), VAE,
/// CLIP (single/dual/triple, incl. GGUF), LoRA, ControlNet, upscalers — not just
/// the two the old code handled. Empty => not a model loader, leave it alone.
fn loader_slots(class_type: &str) -> Vec<Slot> {
    match class_type {
        c if c.contains("CheckpointLoader") => vec![slot(0, "ckpt_name", "checkpoints")],
        "UNETLoader" => vec![slot(0, "unet_name", "diffusion_models")],
        c if c.starts_with("UnetLoaderGGUF") => vec![slot(0, "unet_name", "unet")],
        "VAELoader" => vec![slot(0, "vae_name", "vae")],
        c if c.starts_with("TripleCLIPLoader") => vec![
            slot(0, "clip_name1", "clip"),
            slot(1, "clip_name2", "clip"),
            slot(2, "clip_name3", "clip"),
        ],
        c if c.starts_with("DualCLIPLoader") => {
            vec![slot(0, "clip_name1", "clip"), slot(1, "clip_name2", "clip")]
        }
        c if c.starts_with("CLIPLoader") => vec![slot(0, "clip_name", "clip")],
        "LoraLoader" | "LoraLoaderModelOnly" => vec![slot(0, "lora_name", "loras")],
        c if c.contains("ControlNetLoader") => vec![slot(0, "control_net_name", "controlnet")],
        "UpscaleModelLoader" => vec![slot(0, "model_name", "upscale_models")],
        _ => vec![],
    }
}

/// Vault subdirs to search when hotloading, keyed by the target subdir. Big model
/// files are routinely mis-filed across the diffusion trio, so those pool together.
fn vault_dirs_for(subdir: &str) -> &'static [&'static str] {
    match subdir {
        "checkpoints" | "diffusion_models" | "unet" => &["checkpoints", "diffusion_models", "unet"],
        "vae" => &["vae"],
        "clip" => &["clip", "text_encoders", "clip_vision"],
        "loras" => &["loras"],
        "controlnet" => &["controlnet"],
        "upscale_models" => &["upscale_models"],
        _ => &[],
    }
}

/// Resolve `wanted` (which may carry a foreign subfolder path) to a model ComfyUI
/// can load, matching on the bare filename. Returns Some(new_name) when the
/// reference should change, None to keep it as-is. Order: keep if installed
/// verbatim → adopt the installed spelling if only the path/case differs → match a
/// near-identical installed file → find it in the cold vault and hotload it → leave
/// it (honest "missing model"). `class_type` is the object_info node to read
/// installed values from.
fn resolve_model(
    client: &reqwest::blocking::Client,
    class_type: &str,
    field: &str,
    wanted: &str,
    target_subdir: &str,
    vault_root: &str,
    nvme_root: &str,
) -> Option<String> {
    let installed = obj_enum(client, class_type, field);
    if installed.iter().any(|m| m == wanted) {
        return None; // already exactly what ComfyUI lists
    }
    let wb = base(wanted).to_ascii_lowercase();
    // same file, different spelling (author subfolder / case / separator): adopt
    // ComfyUI's own spelling so the dropdown resolves.
    if let Some(exact) = installed
        .iter()
        .find(|m| base(m).to_ascii_lowercase() == wb)
    {
        return Some(exact.clone());
    }
    if let Some(m) = best_match(wanted, &installed) {
        return Some(m);
    }
    // cold vault → hotload into the subdir this loader reads from.
    for sub in vault_dirs_for(target_subdir) {
        let dir = Path::new(vault_root).join(sub);
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let names: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
                if matches!(
                    ext,
                    "safetensors" | "ckpt" | "gguf" | "sft" | "pt" | "pth" | "bin"
                ) {
                    p.file_name().and_then(|n| n.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect();
        if let Some(fname) = best_match(wanted, &names) {
            let src = dir.join(&fname);
            let dst_dir = Path::new(nvme_root).join(target_subdir);
            let _ = std::fs::create_dir_all(&dst_dir);
            let dst = dst_dir.join(&fname);
            if dst.exists() || std::fs::copy(&src, &dst).is_ok() {
                return Some(fname);
            }
        }
    }
    None
}

/// Rewrite CheckpointLoaderSimple/UNETLoader model names in a workflow (UI or API
/// format) so they resolve in ComfyUI — keeping installed models, hotloading vault
/// models, leaving the rest honestly missing.
fn patch_model_names(
    client: &reqwest::blocking::Client,
    wf: &str,
    vault_root: &str,
    nvme_root: &str,
) -> String {
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
                let slots = loader_slots(&t);
                if slots.is_empty() {
                    continue;
                }
                let Some(wv) = n.get_mut("widgets_values").and_then(|w| w.as_array_mut()) else {
                    continue;
                };
                for s in &slots {
                    let Some(cur) = wv
                        .get(s.ui_index)
                        .and_then(|x| x.as_str())
                        .map(String::from)
                    else {
                        continue;
                    };
                    if let Some(rep) =
                        resolve_model(client, &t, s.field, &cur, s.subdir, vault_root, nvme_root)
                    {
                        wv[s.ui_index] = serde_json::Value::String(rep);
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
            let slots = loader_slots(&t);
            if slots.is_empty() {
                continue;
            }
            if let Some(inp) = n.get_mut("inputs").and_then(|i| i.as_object_mut()) {
                for s in &slots {
                    // a wired link is an array, not a string — skip; only widget values resolve
                    let Some(cur) = inp.get(s.field).and_then(|x| x.as_str()).map(String::from)
                    else {
                        continue;
                    };
                    if let Some(rep) =
                        resolve_model(client, &t, s.field, &cur, s.subdir, vault_root, nvme_root)
                    {
                        inp.insert(s.field.to_string(), serde_json::Value::String(rep));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_same_model_under_different_filename() {
        let wanted = "2758_hinaAsianFlux1-krea-dev_v51-fp8_noCLIP.safetensors";
        let installed = vec![
            "flux_dev.safetensors".to_string(),
            "2758FluxAsianUtopian_v51KreaFp8Noclip.safetensors".to_string(),
            "sd_xl_base_1.0.safetensors".to_string(),
        ];
        assert_eq!(
            best_match(wanted, &installed).as_deref(),
            Some("2758FluxAsianUtopian_v51KreaFp8Noclip.safetensors")
        );
    }

    #[test]
    fn no_match_when_tokens_dont_overlap() {
        let installed = vec![
            "sd_xl_base_1.0.safetensors".to_string(),
            "deliberate_v2.safetensors".to_string(),
        ];
        assert!(best_match("2758_hinaAsianFlux1-krea-dev.safetensors", &installed).is_none());
    }

    #[test]
    fn base_strips_author_subfolders() {
        assert_eq!(
            base(r"GGUFFlux\Z\WIP\FuxCapacity4.0_Q8_0.gguf"),
            "FuxCapacity4.0_Q8_0.gguf"
        );
        assert_eq!(base("flux/flux_vae.safetensors"), "flux_vae.safetensors");
        assert_eq!(base("plain.safetensors"), "plain.safetensors");
    }

    #[test]
    fn same_file_under_foreign_subfolder_matches_by_basename() {
        // The exact-basename branch of resolve_model catches "same file, foreign
        // path" (short names can share <3 tokens, so this must NOT rely on best_match).
        let wanted = r"GGUFFlux\Z\WIP\FuxCapacity4.0_Q8_0.gguf";
        let installed = "FuxCapacity4.0_Q8_0.gguf";
        assert_eq!(
            base(wanted).to_ascii_lowercase(),
            base(installed).to_ascii_lowercase()
        );
    }

    #[test]
    fn fuzzy_match_still_ignores_subfolder_path() {
        // Foreign subfolder path + a differently-spelled but same-model file.
        let wanted = r"GGUFFlux\Z\WIP\2758FluxAsianUtopian_v51KreaFp8Noclip.safetensors";
        let installed = vec!["2758_hinaAsianFlux1-krea-dev_v51-fp8_noCLIP.safetensors".to_string()];
        assert_eq!(
            best_match(wanted, &installed).as_deref(),
            Some("2758_hinaAsianFlux1-krea-dev_v51-fp8_noCLIP.safetensors")
        );
    }

    #[test]
    fn loader_slots_cover_flux_stack() {
        assert_eq!(loader_slots("UnetLoaderGGUF").len(), 1);
        assert_eq!(loader_slots("VAELoader").len(), 1);
        assert_eq!(loader_slots("TripleCLIPLoader").len(), 3);
        assert_eq!(loader_slots("DualCLIPLoaderGGUF").len(), 2);
        assert_eq!(loader_slots("LoraLoader").len(), 1);
        assert!(loader_slots("KSampler").is_empty());
    }

    #[test]
    fn tokens_split_camelcase_and_digits() {
        let t = tokens("2758FluxAsianUtopian_v51KreaFp8Noclip.safetensors");
        for w in ["2758", "flux", "asian", "utopian", "krea", "noclip"] {
            assert!(t.contains(w), "missing token {w}");
        }
    }
}
