//! Convert between the two generation-metadata representations a captured image
//! can carry: a ComfyUI node graph (`workflow`/`prompt` JSON, UI or API format)
//! and an A1111-style `parameters` text block.
//!
//! The Manifest lightbox uses this to synthesize whichever side is missing and
//! cache it next to the image, so every captured image ends up with both a
//! viewable graph and a copy-pasteable parameter string.

use serde_json::{json, Map, Value};

/// The shared subset both formats encode.
#[derive(Default, Debug, Clone)]
pub struct Recipe {
    pub positive: String,
    pub negative: String,
    pub steps: Option<i64>,
    pub cfg: Option<f64>,
    pub sampler: Option<String>,   // ComfyUI sampler name (euler, dpmpp_2m…)
    pub scheduler: Option<String>, // normal | karras | …
    pub seed: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub model: Option<String>,
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

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|x| x as f64))
}

/// API format: { id: { class_type, inputs: { name: literal | [src_id, slot] } } }
fn recipe_from_api(v: &Value) -> Recipe {
    let mut r = Recipe::default();
    let obj = match v.as_object() {
        Some(o) => o,
        None => return r,
    };
    let class = |id: &str| obj.get(id).and_then(|n| n.get("class_type")).and_then(|c| c.as_str());
    let inputs = |id: &str| obj.get(id).and_then(|n| n.get("inputs")).and_then(|i| i.as_object());
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
    let sampler_id = obj.keys().find(|k| class(k).map_or(false, |c| c.contains("KSampler")));
    if let Some(sid) = sampler_id {
        if let Some(ins) = inputs(sid) {
            r.seed = ins.get("seed").or_else(|| ins.get("noise_seed")).and_then(|x| x.as_i64());
            r.steps = ins.get("steps").and_then(|x| x.as_i64());
            r.cfg = ins.get("cfg").and_then(num);
            r.sampler = ins.get("sampler_name").and_then(|x| x.as_str()).map(String::from);
            r.scheduler = ins.get("scheduler").and_then(|x| x.as_str()).map(String::from);
            if let Some(src) = link_src(ins, "positive") {
                r.positive = text_of(&src).unwrap_or_default();
            }
            if let Some(src) = link_src(ins, "negative") {
                r.negative = text_of(&src).unwrap_or_default();
            }
        }
    }
    // model + size from loaders anywhere in the graph
    for (id, _) in obj {
        match class(id) {
            Some(c) if c.contains("CheckpointLoader") => {
                if let Some(ins) = inputs(id) {
                    r.model = r.model.take().or_else(|| {
                        ins.get("ckpt_name").and_then(|x| x.as_str()).map(String::from)
                    });
                }
            }
            Some("UNETLoader") => {
                if let Some(ins) = inputs(id) {
                    r.model = r.model.take().or_else(|| {
                        ins.get("unet_name").and_then(|x| x.as_str()).map(String::from)
                    });
                }
            }
            Some(c) if c.starts_with("Empty") && c.contains("Latent") => {
                if let Some(ins) = inputs(id) {
                    r.width = r.width.or_else(|| ins.get("width").and_then(|x| x.as_i64()));
                    r.height = r.height.or_else(|| ins.get("height").and_then(|x| x.as_i64()));
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
    let ntype = |n: &Value| n.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let wv = |n: &Value| n.get("widgets_values").and_then(|w| w.as_array()).cloned().unwrap_or_default();

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
    // latent size
    if let Some(l) = nodes.iter().find(|n| ntype(n).starts_with("Empty") && ntype(n).contains("Latent")) {
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

fn format_a1111(r: &Recipe) -> String {
    let mut out = String::new();
    out.push_str(r.positive.trim());
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
    out.push_str(&parts.join(", "));
    out.push_str("\n\n[synthesized from the ComfyUI workflow by Synthetrix]");
    out
}

// ---- A1111 params -> ComfyUI workflow (API format) -------------------------

pub fn params_to_workflow(params: &str) -> Option<String> {
    let r = parse_a1111(params);
    let (sampler, scheduler) = match &r.sampler {
        Some(s) => (s.clone(), r.scheduler.clone().unwrap_or_else(|| "normal".into())),
        None => ("euler".into(), "normal".into()),
    };
    let graph = json!({
        "4": {"class_type": "CheckpointLoaderSimple",
              "inputs": {"ckpt_name": r.model.clone().unwrap_or_else(|| "model.safetensors".into())}},
        "6": {"class_type": "CLIPTextEncode", "inputs": {"text": r.positive, "clip": ["4", 1]}},
        "7": {"class_type": "CLIPTextEncode", "inputs": {"text": r.negative, "clip": ["4", 1]}},
        "5": {"class_type": "EmptyLatentImage",
              "inputs": {"width": r.width.unwrap_or(1024), "height": r.height.unwrap_or(1024), "batch_size": 1}},
        "3": {"class_type": "KSampler",
              "inputs": {"seed": r.seed.unwrap_or(0), "steps": r.steps.unwrap_or(20),
                         "cfg": r.cfg.unwrap_or(7.0), "sampler_name": sampler, "scheduler": scheduler,
                         "denoise": 1.0, "model": ["4", 0], "positive": ["6", 0],
                         "negative": ["7", 0], "latent_image": ["5", 0]}},
        "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
        "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "converted", "images": ["8", 0]}},
    });
    serde_json::to_string_pretty(&graph).ok()
}

fn parse_a1111(text: &str) -> Recipe {
    let mut r = Recipe::default();
    let neg_marker = "Negative prompt:";
    // settings line = the last line that contains "Steps:"
    let lines: Vec<&str> = text.lines().collect();
    let steps_line_idx = lines.iter().rposition(|l| l.contains("Steps:"));

    let body_end = steps_line_idx.unwrap_or(lines.len());
    let body = lines[..body_end].join("\n");
    if let Some(npos) = body.find(neg_marker) {
        r.positive = body[..npos].trim().to_string();
        r.negative = body[npos + neg_marker.len()..].trim().to_string();
    } else {
        r.positive = body.trim().to_string();
    }

    if let Some(idx) = steps_line_idx {
        for kv in lines[idx].split(',') {
            let mut it = kv.splitn(2, ':');
            let key = it.next().unwrap_or("").trim();
            let val = it.next().unwrap_or("").trim();
            match key {
                "Steps" => r.steps = val.parse().ok(),
                "CFG scale" => r.cfg = val.parse().ok(),
                "Seed" => r.seed = val.parse().ok(),
                "Sampler" => {
                    let (s, sc) = normalize_sampler(val);
                    r.sampler = Some(s);
                    r.scheduler = Some(sc);
                }
                "Size" => {
                    let mut wh = val.split('x');
                    r.width = wh.next().and_then(|x| x.trim().parse().ok());
                    r.height = wh.next().and_then(|x| x.trim().parse().ok());
                }
                "Model" => r.model = Some(format!("{val}.safetensors")),
                _ => {}
            }
        }
    }
    r
}

/// Map an A1111 sampler label to a (comfy_sampler, scheduler) pair.
fn normalize_sampler(a: &str) -> (String, String) {
    let low = a.to_lowercase();
    let scheduler = if low.contains("karras") { "karras" } else { "normal" }.to_string();
    let sampler = if low.contains("dpm++ 2m sde") || low.contains("dpmpp_2m_sde") {
        "dpmpp_2m_sde"
    } else if low.contains("dpm++ 2m") || low.contains("dpmpp_2m") {
        "dpmpp_2m"
    } else if low.contains("dpm++ sde") || low.contains("dpmpp_sde") {
        "dpmpp_sde"
    } else if low.contains("euler a") || low.contains("euler_ancestral") {
        "euler_ancestral"
    } else if low.contains("euler") {
        "euler"
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
}
