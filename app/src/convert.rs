//! Convert between the two generation-metadata representations a captured image
//! can carry: a ComfyUI node graph (`workflow`/`prompt` JSON, UI or API format)
//! and an A1111-style `parameters` text block.
//!
//! The Manifest lightbox uses this to synthesize whichever side is missing and
//! cache it next to the image, so every captured image ends up with both a
//! viewable graph and a copy-pasteable parameter string.
//!
//! A1111 is not a flat key/value list — its prompt is a mini-language. This module
//! parses the parts that change the *graph shape*: inline `<lora:name:wm[:wc]>`
//! tags (which become a LoraLoader chain, not literal prompt text), `Clip skip`
//! (a CLIPSetLastLayer node), a `VAE` override (a VAELoader instead of the
//! checkpoint's baked VAE), the `Schedule type` field (newer A1111 splits it out
//! of the sampler), and the Hires-fix pass (a latent upscale + second KSampler).

use serde_json::{json, Map, Value};

/// An inline `<lora:name:model_weight[:clip_weight]>` reference pulled from a prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct LoraRef {
    pub name: String,
    pub model_w: f64,
    pub clip_w: f64,
}

/// A1111 Hires-fix pass. `upscale` is the linear scale factor (2.0 => 2x).
#[derive(Debug, Clone, PartialEq)]
pub struct Hires {
    pub upscale: f64,
    pub steps: Option<i64>,
    pub denoise: f64,
    pub upscaler: Option<String>,
}

/// The shared subset both formats encode.
#[derive(Default, Debug, Clone)]
pub struct Recipe {
    pub positive: String,
    pub negative: String,
    pub steps: Option<i64>,
    pub cfg: Option<f64>,
    pub sampler: Option<String>, // ComfyUI sampler name (euler, dpmpp_2m…)
    pub scheduler: Option<String>, // normal | karras | …
    pub seed: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub model: Option<String>,
    pub loras: Vec<LoraRef>,
    pub clip_skip: Option<i64>, // A1111 value (2 = skip one layer); comfy uses -n
    pub vae: Option<String>,
    pub denoise: Option<f64>,
    pub hires: Option<Hires>,
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|x| x as f64))
}

/// Ensure a bare model name carries a loadable extension (A1111 writes `foo`,
/// ComfyUI's dropdown lists `foo.safetensors`). Left as-is if it already has one.
fn with_ext(name: &str) -> String {
    let low = name.to_ascii_lowercase();
    if [
        ".safetensors",
        ".ckpt",
        ".gguf",
        ".sft",
        ".pt",
        ".pth",
        ".bin",
    ]
    .iter()
    .any(|e| low.ends_with(e))
    {
        name.to_string()
    } else {
        format!("{name}.safetensors")
    }
}

// ---- ComfyUI workflow -> A1111 params --------------------------------------

pub fn workflow_to_params(wf_json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(wf_json).ok()?;
    let r = if v.get("nodes").and_then(|n| n.as_array()).is_some() {
        recipe_from_ui(&v)
    } else {
        recipe_from_api(&v)
    };
    Some(format_a1111(&r))
}

/// API format: { id: { class_type, inputs: { name: literal | [src_id, slot] } } }
fn recipe_from_api(v: &Value) -> Recipe {
    let mut r = Recipe::default();
    let obj = match v.as_object() {
        Some(o) => o,
        None => return r,
    };
    let class = |id: &str| {
        obj.get(id)
            .and_then(|n| n.get("class_type"))
            .and_then(|c| c.as_str())
    };
    let inputs = |id: &str| {
        obj.get(id)
            .and_then(|n| n.get("inputs"))
            .and_then(|i| i.as_object())
    };
    let link_src = |node: &Map<String, Value>, name: &str| -> Option<String> {
        node.get(name)
            .and_then(|x| x.as_array())
            .filter(|a| a.len() >= 2 && a[0].as_str().is_some())
            .map(|a| a[0].as_str().unwrap().to_string())
    };
    // text of a CLIPTextEncode-ish node id, following one wrapper hop if needed
    let text_of = |start: &str| -> Option<String> {
        let mut cur = start.to_string();
        for _ in 0..3 {
            let ins = inputs(&cur)?;
            if let Some(t) = ins.get("text").and_then(|t| t.as_str()) {
                return Some(t.to_string());
            }
            // step through a wrapper (e.g. conditioning combine) via its first link
            let next = ins.values().find_map(|val| {
                val.as_array()
                    .filter(|a| a.len() >= 2 && a[0].as_str().is_some())
                    .map(|a| a[0].as_str().unwrap().to_string())
            })?;
            cur = next;
        }
        None
    };

    // find a sampler node
    let sampler_id = obj
        .keys()
        .find(|k| class(k).is_some_and(|c| c.contains("KSampler")));
    if let Some(sid) = sampler_id {
        if let Some(ins) = inputs(sid) {
            r.seed = ins
                .get("seed")
                .or_else(|| ins.get("noise_seed"))
                .and_then(|x| x.as_i64());
            r.steps = ins.get("steps").and_then(|x| x.as_i64());
            r.cfg = ins.get("cfg").and_then(num);
            r.sampler = ins
                .get("sampler_name")
                .and_then(|x| x.as_str())
                .map(String::from);
            r.scheduler = ins
                .get("scheduler")
                .and_then(|x| x.as_str())
                .map(String::from);
            if let Some(src) = link_src(ins, "positive") {
                r.positive = text_of(&src).unwrap_or_default();
            }
            if let Some(src) = link_src(ins, "negative") {
                r.negative = text_of(&src).unwrap_or_default();
            }
        }
    }
    // model + size + loras + clip-skip + vae from the rest of the graph
    for (id, _) in obj {
        match class(id) {
            Some(c) if c.contains("CheckpointLoader") => {
                if let Some(ins) = inputs(id) {
                    r.model = r.model.take().or_else(|| {
                        ins.get("ckpt_name")
                            .and_then(|x| x.as_str())
                            .map(String::from)
                    });
                }
            }
            Some("UNETLoader") => {
                if let Some(ins) = inputs(id) {
                    r.model = r.model.take().or_else(|| {
                        ins.get("unet_name")
                            .and_then(|x| x.as_str())
                            .map(String::from)
                    });
                }
            }
            Some(c) if c == "LoraLoader" || c == "LoraLoaderModelOnly" => {
                if let Some(ins) = inputs(id) {
                    if let Some(name) = ins.get("lora_name").and_then(|x| x.as_str()) {
                        r.loras.push(LoraRef {
                            name: name.to_string(),
                            model_w: ins.get("strength_model").and_then(num).unwrap_or(1.0),
                            clip_w: ins.get("strength_clip").and_then(num).unwrap_or(1.0),
                        });
                    }
                }
            }
            Some("VAELoader") => {
                if let Some(ins) = inputs(id) {
                    r.vae = ins
                        .get("vae_name")
                        .and_then(|x| x.as_str())
                        .map(String::from);
                }
            }
            Some("CLIPSetLastLayer") => {
                if let Some(ins) = inputs(id) {
                    r.clip_skip = ins
                        .get("stop_at_clip_layer")
                        .and_then(|x| x.as_i64())
                        .map(|n| n.abs());
                }
            }
            Some(c) if c.starts_with("Empty") && c.contains("Latent") => {
                if let Some(ins) = inputs(id) {
                    r.width = r
                        .width
                        .or_else(|| ins.get("width").and_then(|x| x.as_i64()));
                    r.height = r
                        .height
                        .or_else(|| ins.get("height").and_then(|x| x.as_i64()));
                }
            }
            _ => {}
        }
    }
    r
}

/// UI (litegraph) format: { nodes: [ { type, widgets_values, id } ], links: [...] }
fn recipe_from_ui(v: &Value) -> Recipe {
    let mut r = Recipe::default();
    let nodes = match v.get("nodes").and_then(|n| n.as_array()) {
        Some(n) => n,
        None => return r,
    };
    let ntype = |n: &Value| {
        n.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    let wv = |n: &Value| {
        n.get("widgets_values")
            .and_then(|w| w.as_array())
            .cloned()
            .unwrap_or_default()
    };

    // KSampler widget order: [seed, control_after_generate, steps, cfg, sampler, scheduler, denoise]
    if let Some(k) = nodes.iter().find(|n| ntype(n).contains("KSampler")) {
        let w = wv(k);
        let g = |i: usize| w.get(i);
        r.seed = g(0).and_then(|x| x.as_i64());
        r.steps = g(2).and_then(|x| x.as_i64());
        r.cfg = g(3).and_then(num);
        r.sampler = g(4).and_then(|x| x.as_str()).map(String::from);
        r.scheduler = g(5).and_then(|x| x.as_str()).map(String::from);
    }
    // checkpoint loader
    if let Some(c) = nodes.iter().find(|n| ntype(n).contains("CheckpointLoader")) {
        r.model = wv(c).first().and_then(|x| x.as_str()).map(String::from);
    }
    // lora loaders: widgets [lora_name, strength_model, strength_clip]
    for l in nodes
        .iter()
        .filter(|n| ntype(n) == "LoraLoader" || ntype(n) == "LoraLoaderModelOnly")
    {
        let w = wv(l);
        if let Some(name) = w.first().and_then(|x| x.as_str()) {
            r.loras.push(LoraRef {
                name: name.to_string(),
                model_w: w.get(1).and_then(num).unwrap_or(1.0),
                clip_w: w.get(2).and_then(num).unwrap_or(1.0),
            });
        }
    }
    // VAELoader / CLIPSetLastLayer
    if let Some(vae) = nodes.iter().find(|n| ntype(n) == "VAELoader") {
        r.vae = wv(vae).first().and_then(|x| x.as_str()).map(String::from);
    }
    if let Some(cs) = nodes.iter().find(|n| ntype(n) == "CLIPSetLastLayer") {
        r.clip_skip = wv(cs).first().and_then(|x| x.as_i64()).map(|n| n.abs());
    }
    // latent size
    if let Some(l) = nodes
        .iter()
        .find(|n| ntype(n).starts_with("Empty") && ntype(n).contains("Latent"))
    {
        let w = wv(l);
        r.width = w.first().and_then(|x| x.as_i64());
        r.height = w.get(1).and_then(|x| x.as_i64());
    }
    // positive/negative via the link graph: KSampler input slot 1 = positive, 2 = negative
    let id_of = |n: &Value| n.get("id").and_then(|x| x.as_i64());
    let text_of_id = |target: i64| -> Option<String> {
        nodes
            .iter()
            .find(|n| id_of(n) == Some(target) && ntype(n).contains("CLIPTextEncode"))
            .and_then(|n| wv(n).first().and_then(|x| x.as_str()).map(String::from))
    };
    if let Some(k) = nodes.iter().find(|n| ntype(n).contains("KSampler")) {
        if let (Some(kid), Some(links)) = (id_of(k), v.get("links").and_then(|l| l.as_array())) {
            // link = [link_id, from_node, from_slot, to_node, to_slot, type]
            let src_for = |slot: i64| -> Option<i64> {
                links.iter().find_map(|l| {
                    let a = l.as_array()?;
                    if a.len() >= 5 && a[3].as_i64() == Some(kid) && a[4].as_i64() == Some(slot) {
                        a[1].as_i64()
                    } else {
                        None
                    }
                })
            };
            if let Some(p) = src_for(1).and_then(text_of_id) {
                r.positive = p;
            }
            if let Some(n) = src_for(2).and_then(text_of_id) {
                r.negative = n;
            }
        }
    }
    // fallback: if links didn't resolve, take first two CLIPTextEncode in order
    if r.positive.is_empty() {
        let mut texts = nodes
            .iter()
            .filter(|n| ntype(n).contains("CLIPTextEncode"))
            .filter_map(|n| wv(n).first().and_then(|x| x.as_str()).map(String::from));
        r.positive = texts.next().unwrap_or_default();
        if r.negative.is_empty() {
            r.negative = texts.next().unwrap_or_default();
        }
    }
    r
}

/// Render a weight without a trailing `.0` so `<lora:x:1>` round-trips cleanly.
fn fmt_w(w: f64) -> String {
    if (w - w.round()).abs() < f64::EPSILON {
        format!("{}", w as i64)
    } else {
        format!("{w}")
    }
}

fn format_a1111(r: &Recipe) -> String {
    let mut out = String::new();
    out.push_str(r.positive.trim());
    // Re-attach LoRAs as inline tags so the A1111 text is faithful and re-usable.
    for l in &r.loras {
        let stem = l.name.rsplit_once('.').map(|(a, _)| a).unwrap_or(&l.name);
        if l.model_w == l.clip_w {
            out.push_str(&format!(" <lora:{}:{}>", stem, fmt_w(l.model_w)));
        } else {
            out.push_str(&format!(
                " <lora:{}:{}:{}>",
                stem,
                fmt_w(l.model_w),
                fmt_w(l.clip_w)
            ));
        }
    }
    out.push('\n');
    out.push_str("Negative prompt: ");
    out.push_str(r.negative.trim());
    out.push('\n');
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = r.steps {
        parts.push(format!("Steps: {s}"));
    }
    if let Some(s) = &r.sampler {
        let sched = r.scheduler.as_deref().filter(|x| *x != "normal");
        parts.push(match sched {
            Some(sc) => format!("Sampler: {s} {sc}"),
            None => format!("Sampler: {s}"),
        });
    }
    if let Some(c) = r.cfg {
        parts.push(format!("CFG scale: {c}"));
    }
    if let Some(s) = r.seed {
        parts.push(format!("Seed: {s}"));
    }
    if let (Some(w), Some(h)) = (r.width, r.height) {
        parts.push(format!("Size: {w}x{h}"));
    }
    if let Some(m) = &r.model {
        parts.push(format!("Model: {m}"));
    }
    if let Some(v) = &r.vae {
        parts.push(format!("VAE: {v}"));
    }
    if let Some(cs) = r.clip_skip.filter(|n| *n >= 2) {
        parts.push(format!("Clip skip: {cs}"));
    }
    if let Some(h) = &r.hires {
        parts.push(format!("Denoising strength: {}", h.denoise));
        parts.push(format!("Hires upscale: {}", h.upscale));
        if let Some(s) = h.steps {
            parts.push(format!("Hires steps: {s}"));
        }
        if let Some(u) = &h.upscaler {
            parts.push(format!("Hires upscaler: {u}"));
        }
    } else if let Some(d) = r.denoise {
        parts.push(format!("Denoising strength: {d}"));
    }
    out.push_str(&parts.join(", "));
    out.push_str("\n\n[synthesized from the ComfyUI workflow by Synthetrix]");
    out
}

// ---- A1111 params -> ComfyUI workflow (API format) -------------------------

pub fn params_to_workflow(params: &str) -> Option<String> {
    let r = parse_a1111(params);
    let (sampler, scheduler) = match &r.sampler {
        Some(s) => (
            s.clone(),
            r.scheduler.clone().unwrap_or_else(|| "normal".into()),
        ),
        None => ("euler".into(), "normal".into()),
    };

    let mut g = Map::new();
    let node = |g: &mut Map<String, Value>, id: &str, class: &str, inputs: Value| {
        g.insert(
            id.to_string(),
            json!({ "class_type": class, "inputs": inputs }),
        );
    };

    // Checkpoint. model_src/clip_src/vae_src are the live outputs, rewired as we
    // insert LoRA / clip-skip / VAE nodes so the graph stays correctly threaded.
    node(
        &mut g,
        "4",
        "CheckpointLoaderSimple",
        json!({ "ckpt_name": r.model.clone().unwrap_or_else(|| "model.safetensors".into()) }),
    );
    let mut model_src = json!(["4", 0]);
    let mut clip_src = json!(["4", 1]);
    let mut vae_src = json!(["4", 2]);
    let mut next_id = 100; // LoRA / aux nodes live in a high range to avoid clashes

    // LoRA chain: each hop consumes the previous model+clip and emits patched ones.
    for l in &r.loras {
        let id = next_id.to_string();
        next_id += 1;
        node(
            &mut g,
            &id,
            "LoraLoader",
            json!({
                "lora_name": with_ext(&l.name),
                "strength_model": l.model_w,
                "strength_clip": l.clip_w,
                "model": model_src,
                "clip": clip_src,
            }),
        );
        model_src = json!([id, 0]);
        clip_src = json!([id, 1]);
    }

    // Clip skip -> CLIPSetLastLayer (only when skipping a layer; 1 == default).
    if let Some(cs) = r.clip_skip.filter(|n| *n >= 2) {
        let id = next_id.to_string();
        next_id += 1;
        node(
            &mut g,
            &id,
            "CLIPSetLastLayer",
            json!({ "stop_at_clip_layer": -cs, "clip": clip_src }),
        );
        clip_src = json!([id, 0]);
    }

    // VAE override -> VAELoader (else the checkpoint's baked VAE is used).
    if let Some(vae) = &r.vae {
        let id = next_id.to_string();
        next_id += 1;
        node(&mut g, &id, "VAELoader", json!({ "vae_name": vae }));
        vae_src = json!([id, 0]);
    }

    node(
        &mut g,
        "6",
        "CLIPTextEncode",
        json!({ "text": r.positive, "clip": clip_src }),
    );
    node(
        &mut g,
        "7",
        "CLIPTextEncode",
        json!({ "text": r.negative, "clip": clip_src }),
    );
    node(
        &mut g,
        "5",
        "EmptyLatentImage",
        json!({ "width": r.width.unwrap_or(1024), "height": r.height.unwrap_or(1024), "batch_size": 1 }),
    );
    node(
        &mut g,
        "3",
        "KSampler",
        json!({
            "seed": r.seed.unwrap_or(0), "steps": r.steps.unwrap_or(20),
            "cfg": r.cfg.unwrap_or(7.0), "sampler_name": sampler, "scheduler": scheduler,
            "denoise": 1.0, "model": model_src, "positive": ["6", 0],
            "negative": ["7", 0], "latent_image": ["5", 0],
        }),
    );
    let mut latent_src = json!(["3", 0]);

    // Hires fix: latent-upscale the first pass, then a second KSampler at the hires
    // denoise. (Latent upscale covers the common "Latent" upscaler; a model
    // upscaler is approximated by the same latent path.)
    if let Some(h) = &r.hires {
        let up = next_id.to_string();
        next_id += 1;
        node(
            &mut g,
            &up,
            "LatentUpscaleBy",
            json!({ "upscale_method": "nearest-exact", "scale_by": h.upscale, "samples": latent_src }),
        );
        let ks2 = next_id.to_string();
        node(
            &mut g,
            &ks2,
            "KSampler",
            json!({
                "seed": r.seed.unwrap_or(0), "steps": h.steps.unwrap_or(r.steps.unwrap_or(20)),
                "cfg": r.cfg.unwrap_or(7.0),
                "sampler_name": r.sampler.clone().unwrap_or_else(|| "euler".into()),
                "scheduler": r.scheduler.clone().unwrap_or_else(|| "normal".into()),
                "denoise": h.denoise, "model": model_src, "positive": ["6", 0],
                "negative": ["7", 0], "latent_image": [up, 0],
            }),
        );
        latent_src = json!([ks2, 0]);
    }

    node(
        &mut g,
        "8",
        "VAEDecode",
        json!({ "samples": latent_src, "vae": vae_src }),
    );
    node(
        &mut g,
        "9",
        "SaveImage",
        json!({ "filename_prefix": "converted", "images": ["8", 0] }),
    );

    serde_json::to_string_pretty(&Value::Object(g)).ok()
}

/// Pull `<lora:name:wm[:wc]>` (and `<lyco:…>`) tags out of a prompt, returning the
/// cleaned text and the parsed refs. Non-LoRA angle-bracket content is preserved.
fn extract_loras(text: &str) -> (String, Vec<LoraRef>) {
    let mut out = String::with_capacity(text.len());
    let mut loras = Vec::new();
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            // no closing bracket — keep the remainder verbatim and stop
            out.push('<');
            out.push_str(after);
            return (tidy(&out), loras);
        };
        let inner = &after[..gt];
        let mut parts = inner.split(':');
        let kind = parts.next().unwrap_or("");
        if kind.eq_ignore_ascii_case("lora") || kind.eq_ignore_ascii_case("lyco") {
            let name = parts.next().unwrap_or("").trim();
            let mw = parts
                .next()
                .and_then(|x| x.trim().parse().ok())
                .unwrap_or(1.0);
            let cw = parts
                .next()
                .and_then(|x| x.trim().parse().ok())
                .unwrap_or(mw);
            if !name.is_empty() {
                loras.push(LoraRef {
                    name: name.to_string(),
                    model_w: mw,
                    clip_w: cw,
                });
            }
            // drop the tag from the prompt text
        } else {
            out.push('<');
            out.push_str(inner);
            out.push('>');
        }
        rest = &after[gt + 1..];
    }
    out.push_str(rest);
    (tidy(&out), loras)
}

/// Collapse the doubled spaces / stray commas a removed tag leaves behind, per line.
fn tidy(s: &str) -> String {
    s.lines()
        .map(|line| {
            let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
            collapsed.replace(" ,", ",").replace(",,", ",")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .trim_matches(',')
        .trim()
        .to_string()
}

/// Split an A1111 settings line on commas that are NOT inside a `"…"` value
/// (`Lora hashes: "a: 1, b: 2"` must stay one field).
fn split_settings(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                cur.push(ch);
            }
            ',' if !in_quote => {
                fields.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        fields.push(cur);
    }
    fields
}

fn parse_a1111(text: &str) -> Recipe {
    let mut r = Recipe::default();
    let neg_marker = "Negative prompt:";
    // settings line = the last line that contains "Steps:"
    let lines: Vec<&str> = text.lines().collect();
    let steps_line_idx = lines.iter().rposition(|l| l.contains("Steps:"));

    let body_end = steps_line_idx.unwrap_or(lines.len());
    let body = lines[..body_end].join("\n");
    let (pos, neg) = if let Some(npos) = body.find(neg_marker) {
        (
            body[..npos].trim().to_string(),
            body[npos + neg_marker.len()..].trim().to_string(),
        )
    } else {
        (body.trim().to_string(), String::new())
    };
    // LoRAs can appear in either prompt; hoist them out to the graph.
    let (pos, mut loras) = extract_loras(&pos);
    let (neg, neg_loras) = extract_loras(&neg);
    loras.extend(neg_loras);
    r.positive = pos;
    r.negative = neg;
    r.loras = loras;

    // Hires-fix fields are assembled from several keys, then folded into one struct.
    let mut hires_upscale: Option<f64> = None;
    let mut hires_steps: Option<i64> = None;
    let mut hires_upscaler: Option<String> = None;
    let mut hires_resize: Option<(i64, i64)> = None;

    if let Some(idx) = steps_line_idx {
        for kv in split_settings(lines[idx]) {
            let mut it = kv.splitn(2, ':');
            let key = it.next().unwrap_or("").trim();
            let val = it.next().unwrap_or("").trim().trim_matches('"').trim();
            match key {
                "Steps" => r.steps = val.parse().ok(),
                "CFG scale" => r.cfg = val.parse().ok(),
                "Seed" => r.seed = val.parse().ok(),
                "Sampler" => {
                    let (s, sc) = normalize_sampler(val);
                    r.sampler = Some(s);
                    // Only take the sampler-embedded schedule if a dedicated
                    // "Schedule type" field doesn't override it below.
                    r.scheduler.get_or_insert(sc);
                }
                "Schedule type" => r.scheduler = Some(normalize_schedule(val)),
                "Size" => {
                    let mut wh = val.split('x');
                    r.width = wh.next().and_then(|x| x.trim().parse().ok());
                    r.height = wh.next().and_then(|x| x.trim().parse().ok());
                }
                "Model" => r.model = Some(with_ext(val)),
                "VAE" => r.vae = Some(with_ext(val)),
                "Clip skip" => r.clip_skip = val.parse().ok(),
                "Denoising strength" => r.denoise = val.parse().ok(),
                "Hires upscale" => hires_upscale = val.parse().ok(),
                "Hires steps" => hires_steps = val.parse().ok(),
                "Hires upscaler" => hires_upscaler = Some(val.to_string()),
                "Hires resize" => {
                    let mut wh = val.split('x');
                    let w = wh.next().and_then(|x| x.trim().parse().ok());
                    let h = wh.next().and_then(|x| x.trim().parse().ok());
                    if let (Some(w), Some(h)) = (w, h) {
                        hires_resize = Some((w, h));
                    }
                }
                _ => {}
            }
        }
    }

    // A Hires pass exists if A1111 emitted any hires key. Derive the scale factor
    // from "Hires upscale", else from "Hires resize" vs the base size.
    let has_hires = hires_upscale.is_some()
        || hires_steps.is_some()
        || hires_upscaler.is_some()
        || hires_resize.is_some();
    if has_hires {
        let upscale = hires_upscale
            .or_else(|| match (hires_resize, r.width) {
                (Some((rw, _)), Some(w)) if w > 0 => Some(rw as f64 / w as f64),
                _ => None,
            })
            .unwrap_or(2.0);
        r.hires = Some(Hires {
            upscale,
            steps: hires_steps,
            // A1111 txt2img's "Denoising strength" IS the hires-pass denoise.
            denoise: r.denoise.unwrap_or(0.5),
            upscaler: hires_upscaler,
        });
    }
    r
}

/// Map an A1111 `Schedule type` label to a ComfyUI scheduler name.
fn normalize_schedule(a: &str) -> String {
    let low = a.to_lowercase();
    if low.contains("karras") {
        "karras"
    } else if low.contains("exponential") {
        "exponential"
    } else if low.contains("sgm") {
        "sgm_uniform"
    } else if low.contains("simple") {
        "simple"
    } else if low.contains("beta") {
        "beta"
    } else if low.contains("ddim") {
        "ddim_uniform"
    } else {
        "normal"
    }
    .to_string()
}

/// Map an A1111 sampler label to a (comfy_sampler, scheduler) pair. Newer A1111
/// splits the schedule into its own field; older builds fold it into the sampler
/// name ("DPM++ 2M Karras"), which this still recovers.
fn normalize_sampler(a: &str) -> (String, String) {
    let low = a.to_lowercase();
    let scheduler = if low.contains("karras") {
        "karras"
    } else if low.contains("exponential") {
        "exponential"
    } else {
        "normal"
    }
    .to_string();
    let sampler = if low.contains("3m sde") || low.contains("3m_sde") {
        "dpmpp_3m_sde"
    } else if low.contains("2m sde") || low.contains("2m_sde") {
        "dpmpp_2m_sde"
    } else if low.contains("2m") {
        "dpmpp_2m"
    } else if low.contains("2s a") || low.contains("2s_ancestral") {
        "dpmpp_2s_ancestral"
    } else if low.contains("++ sde") || low.contains("dpmpp_sde") || low.contains("dpm++ sde") {
        "dpmpp_sde"
    } else if low.contains("dpm2") || low.contains("dpm 2") {
        "dpm_2"
    } else if low.contains("euler a") || low.contains("euler_ancestral") {
        "euler_ancestral"
    } else if low.contains("euler") {
        "euler"
    } else if low.contains("lcm") {
        "lcm"
    } else if low.contains("ddim") {
        "ddim"
    } else if low.contains("unipc") || low.contains("uni_pc") {
        "uni_pc"
    } else if low.contains("heun") {
        "heun"
    } else if low.contains("lms") {
        "lms"
    } else {
        "euler"
    };
    (sampler.to_string(), scheduler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_workflow_to_params_roundtrips_key_fields() {
        let wf = r#"{
          "3": {"class_type":"KSampler","inputs":{"seed":42,"steps":28,"cfg":6.5,
                "sampler_name":"dpmpp_2m","scheduler":"karras",
                "positive":["6",0],"negative":["7",0],"model":["4",0],"latent_image":["5",0]}},
          "4": {"class_type":"CheckpointLoaderSimple","inputs":{"ckpt_name":"dreamshaper.safetensors"}},
          "5": {"class_type":"EmptyLatentImage","inputs":{"width":832,"height":1216,"batch_size":1}},
          "6": {"class_type":"CLIPTextEncode","inputs":{"text":"a red fox","clip":["4",1]}},
          "7": {"class_type":"CLIPTextEncode","inputs":{"text":"blurry","clip":["4",1]}}
        }"#;
        let p = workflow_to_params(wf).unwrap();
        assert!(p.starts_with("a red fox"));
        assert!(p.contains("Negative prompt: blurry"));
        assert!(p.contains("Steps: 28"));
        assert!(p.contains("Sampler: dpmpp_2m karras"));
        assert!(p.contains("CFG scale: 6.5"));
        assert!(p.contains("Seed: 42"));
        assert!(p.contains("Size: 832x1216"));
        assert!(p.contains("Model: dreamshaper.safetensors"));
    }

    #[test]
    fn params_to_workflow_builds_valid_graph() {
        let params = "a red fox in snow\nNegative prompt: blurry, lowres\n\
                      Steps: 30, Sampler: DPM++ 2M Karras, CFG scale: 7, Seed: 123, Size: 768x1024, Model: dreamshaper";
        let wf = params_to_workflow(params).unwrap();
        let v: Value = serde_json::from_str(&wf).unwrap();
        assert_eq!(v["3"]["class_type"], "KSampler");
        assert_eq!(v["3"]["inputs"]["seed"], 123);
        assert_eq!(v["3"]["inputs"]["steps"], 30);
        assert_eq!(v["3"]["inputs"]["sampler_name"], "dpmpp_2m");
        assert_eq!(v["3"]["inputs"]["scheduler"], "karras");
        assert_eq!(v["6"]["inputs"]["text"], "a red fox in snow");
        assert_eq!(v["7"]["inputs"]["text"], "blurry, lowres");
        assert_eq!(v["5"]["inputs"]["width"], 768);
        assert_eq!(v["5"]["inputs"]["height"], 1024);
    }

    #[test]
    fn ui_workflow_extracts_recipe() {
        let wf = r#"{
          "nodes":[
            {"id":1,"type":"KSampler","widgets_values":[111,"randomize",24,5.0,"euler","normal",1.0]},
            {"id":2,"type":"CheckpointLoaderSimple","widgets_values":["base.safetensors"]},
            {"id":3,"type":"EmptyLatentImage","widgets_values":[512,768,1]},
            {"id":4,"type":"CLIPTextEncode","widgets_values":["a cat"]},
            {"id":5,"type":"CLIPTextEncode","widgets_values":["ugly"]}
          ],
          "links":[[1,4,0,1,1,"COND"],[2,5,0,1,2,"COND"]]
        }"#;
        let p = workflow_to_params(wf).unwrap();
        assert!(p.starts_with("a cat"));
        assert!(p.contains("Negative prompt: ugly"));
        assert!(p.contains("Steps: 24"));
        assert!(p.contains("Seed: 111"));
        assert!(p.contains("Size: 512x768"));
    }

    #[test]
    fn loras_become_a_chain_not_prompt_text() {
        let params = "1girl, <lora:animeStyle:0.8> masterpiece <lora:detailHands:1:0.5>\n\
                      Negative prompt: bad\n\
                      Steps: 20, Sampler: Euler a, CFG scale: 6, Seed: 5, Size: 512x512, Model: any";
        let wf = params_to_workflow(params).unwrap();
        let v: Value = serde_json::from_str(&wf).unwrap();
        // the prompt text is clean — no <lora:…> literal leaked in
        let pos = v["6"]["inputs"]["text"].as_str().unwrap();
        assert!(!pos.contains("<lora"), "lora tag leaked into prompt: {pos}");
        assert!(pos.contains("1girl") && pos.contains("masterpiece"));
        // two LoraLoader nodes exist, chained off the checkpoint
        let loaders: Vec<_> = v
            .as_object()
            .unwrap()
            .values()
            .filter(|n| n["class_type"] == "LoraLoader")
            .collect();
        assert_eq!(loaders.len(), 2);
        assert_eq!(
            loaders
                .iter()
                .filter(|n| n["inputs"]["lora_name"] == "animeStyle.safetensors")
                .count(),
            1
        );
        // the second lora keeps split model/clip weights
        let hands = loaders
            .iter()
            .find(|n| n["inputs"]["lora_name"] == "detailHands.safetensors")
            .unwrap();
        assert_eq!(hands["inputs"]["strength_model"], 1.0);
        assert_eq!(hands["inputs"]["strength_clip"], 0.5);
        // KSampler's model input is fed by a lora, not the checkpoint directly
        assert_ne!(v["3"]["inputs"]["model"][0], "4");
    }

    #[test]
    fn clip_skip_and_vae_and_schedule_type() {
        let params = "portrait\nNegative prompt: bad\n\
                      Steps: 25, Sampler: DPM++ 2M, Schedule type: Karras, CFG scale: 5, Seed: 1, \
                      Size: 1024x1024, Model: base, VAE: sdxl_vae.safetensors, Clip skip: 2";
        let wf = params_to_workflow(params).unwrap();
        let v: Value = serde_json::from_str(&wf).unwrap();
        let vals: Vec<_> = v.as_object().unwrap().values().collect();
        // schedule type overrides the sampler-embedded default
        assert_eq!(v["3"]["inputs"]["scheduler"], "karras");
        assert_eq!(v["3"]["inputs"]["sampler_name"], "dpmpp_2m");
        // CLIPSetLastLayer at -2 exists and feeds the encoders
        let cs = vals
            .iter()
            .find(|n| n["class_type"] == "CLIPSetLastLayer")
            .expect("clip skip node");
        assert_eq!(cs["inputs"]["stop_at_clip_layer"], -2);
        assert_ne!(v["6"]["inputs"]["clip"][0], "4"); // encoder clip comes from the skip node
                                                      // VAELoader override wired into the decode
        let vae = vals
            .iter()
            .find(|n| n["class_type"] == "VAELoader")
            .expect("vae node");
        assert_eq!(vae["inputs"]["vae_name"], "sdxl_vae.safetensors");
        assert_ne!(v["8"]["inputs"]["vae"][0], "4");
    }

    #[test]
    fn hires_fix_adds_second_pass() {
        let params = "landscape\nNegative prompt: bad\n\
                      Steps: 20, Sampler: Euler, CFG scale: 7, Seed: 9, Size: 512x512, Model: base, \
                      Denoising strength: 0.4, Hires upscale: 2, Hires steps: 10, Hires upscaler: Latent";
        let wf = params_to_workflow(params).unwrap();
        let v: Value = serde_json::from_str(&wf).unwrap();
        let vals: Vec<_> = v.as_object().unwrap().values().collect();
        let up = vals
            .iter()
            .find(|n| n["class_type"] == "LatentUpscaleBy")
            .expect("upscale node");
        assert_eq!(up["inputs"]["scale_by"], 2.0);
        // two KSamplers now; the second runs at the hires denoise
        let ksamplers: Vec<_> = vals
            .iter()
            .filter(|n| n["class_type"] == "KSampler")
            .collect();
        assert_eq!(ksamplers.len(), 2);
        assert!(ksamplers.iter().any(|n| n["inputs"]["denoise"] == 0.4));
        // SaveImage decodes the hires latent, not the base one
        assert_ne!(v["8"]["inputs"]["samples"][0], "3");
    }

    #[test]
    fn lora_roundtrips_through_both_directions() {
        let params = "a hero <lora:epicStyle:0.7>\nNegative prompt: bad\n\
                      Steps: 20, Sampler: Euler, CFG scale: 7, Seed: 1, Size: 512x512, Model: base";
        let wf = params_to_workflow(params).unwrap();
        let back = workflow_to_params(&wf).unwrap();
        assert!(back.contains("<lora:epicStyle:0.7>"), "got: {back}");
        assert!(!back.starts_with("<lora"));
    }
}
