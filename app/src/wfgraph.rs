//! ComfyUI workflow visualizer: parse a captured `*.workflow.json` into a node
//! graph and render it on a pan/zoom canvas with egui's painter.
//!
//! Captured files are usually the litegraph "UI" format (nodes[] with pos/size +
//! links[]). The API "prompt" format (id -> {class_type, inputs}) is also handled
//! via a simple depth-based auto-layout.

use eframe::egui;
use egui::epaint::CubicBezierShape;
use serde_json::Value;
use std::collections::HashMap;

const TITLE_H: f32 = 22.0;
const SLOT_H: f32 = 18.0;
const LINE_H: f32 = 15.0;

pub struct WNode {
    pub id: i64,
    pub title: String,
    pub pos: egui::Vec2,
    pub size: egui::Vec2,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub widgets: Vec<String>, // widget values (sampler/steps/cfg/seed/prompt…)
}

pub struct WLink {
    pub from: i64,
    pub from_slot: usize,
    pub to: i64,
    pub to_slot: usize,
}

pub struct WGraph {
    pub nodes: Vec<WNode>,
    pub links: Vec<WLink>,
}

pub struct WfView {
    pub pan: egui::Vec2, // world coordinate shown at the canvas top-left
    pub zoom: f32,
    pub fitted: bool,
}

impl Default for WfView {
    fn default() -> Self {
        Self {
            pan: egui::vec2(0.0, 0.0),
            zoom: 0.6,
            fitted: false,
        }
    }
}

fn xy(v: Option<&Value>) -> Option<egui::Vec2> {
    let v = v?;
    if let Some(a) = v.as_array() {
        if a.len() >= 2 {
            return Some(egui::vec2(a[0].as_f64()? as f32, a[1].as_f64()? as f32));
        }
    }
    if v.is_object() {
        let x = v.get("0")?.as_f64()? as f32;
        let y = v.get("1")?.as_f64()? as f32;
        return Some(egui::vec2(x, y));
    }
    None
}

fn fit_size(size: egui::Vec2, inputs: usize, outputs: usize, widgets: usize) -> egui::Vec2 {
    let needed_h =
        TITLE_H + SLOT_H * inputs.max(outputs).max(1) as f32 + LINE_H * widgets as f32 + 8.0;
    egui::vec2(size.x.max(180.0), size.y.max(needed_h))
}

fn val_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

const MODEL_EXTS: &[&str] = &[
    "safetensors",
    "gguf",
    "ckpt",
    "pt",
    "pth",
    "sft",
    "bin",
    "vae",
];

/// Strip an author's subfolder path from a model-file reference so the graph shows
/// the bare filename (matching the vault/manifest), e.g. `GGUFFlux\Z\WIP\Fux.gguf`
/// -> `Fux.gguf`. Non-model strings (prompts, samplers) pass through untouched.
fn norm_model_ref(s: &str) -> String {
    let base = s.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(s);
    let is_model = base
        .rsplit_once('.')
        .map(|(_, e)| MODEL_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false);
    if is_model && base.len() < s.len() {
        base.to_string()
    } else {
        s.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

pub fn parse(json: &str) -> Option<WGraph> {
    let v: Value = serde_json::from_str(json).ok()?;
    if v.get("nodes").and_then(|n| n.as_array()).is_some() {
        Some(parse_ui(&v))
    } else if v.is_object() {
        Some(parse_api(&v))
    } else {
        None
    }
}

fn names(arr: Option<&Vec<Value>>, key: &str, alt: &str) -> Vec<String> {
    arr.map(|a| {
        a.iter()
            .map(|i| {
                i.get(key)
                    .and_then(|x| x.as_str())
                    .or_else(|| i.get(alt).and_then(|x| x.as_str()))
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    })
    .unwrap_or_default()
}

fn parse_ui(v: &Value) -> WGraph {
    let mut nodes = Vec::new();
    if let Some(arr) = v.get("nodes").and_then(|n| n.as_array()) {
        for n in arr {
            let id = n.get("id").and_then(|x| x.as_i64()).unwrap_or(0);
            let title = n
                .get("title")
                .and_then(|x| x.as_str())
                .or_else(|| n.get("type").and_then(|x| x.as_str()))
                .unwrap_or("node")
                .to_string();
            let pos = xy(n.get("pos")).unwrap_or(egui::vec2(0.0, 0.0));
            let inputs = names(n.get("inputs").and_then(|x| x.as_array()), "name", "type");
            let outputs = names(n.get("outputs").and_then(|x| x.as_array()), "name", "type");
            let widgets: Vec<String> = n
                .get("widgets_values")
                .and_then(|w| w.as_array())
                .map(|a| {
                    a.iter()
                        .map(|v| norm_model_ref(&val_str(v)))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let size = fit_size(
                xy(n.get("size")).unwrap_or(egui::vec2(200.0, 100.0)),
                inputs.len(),
                outputs.len(),
                widgets.len(),
            );
            nodes.push(WNode {
                id,
                title,
                pos,
                size,
                inputs,
                outputs,
                widgets,
            });
        }
    }
    let mut links = Vec::new();
    if let Some(arr) = v.get("links").and_then(|l| l.as_array()) {
        for l in arr {
            if let Some(a) = l.as_array() {
                if a.len() >= 5 {
                    links.push(WLink {
                        from: a[1].as_i64().unwrap_or(0),
                        from_slot: a[2].as_u64().unwrap_or(0) as usize,
                        to: a[3].as_i64().unwrap_or(0),
                        to_slot: a[4].as_u64().unwrap_or(0) as usize,
                    });
                }
            }
        }
    }
    WGraph { nodes, links }
}

fn parse_api(v: &Value) -> WGraph {
    let obj = match v.as_object() {
        Some(o) => o,
        None => {
            return WGraph {
                nodes: vec![],
                links: vec![],
            }
        }
    };
    let entries: Vec<(&String, &Value)> = obj
        .iter()
        .filter(|(_, n)| n.get("class_type").is_some())
        .collect();
    let key_id: HashMap<String, i64> = entries
        .iter()
        .enumerate()
        .map(|(i, (k, _))| ((*k).clone(), i as i64))
        .collect();

    let mut links = Vec::new();
    let mut tmp: Vec<(i64, String, Vec<String>, Vec<String>)> = Vec::new();
    for (k, n) in &entries {
        let id = key_id[*k];
        let title = n
            .get("class_type")
            .and_then(|x| x.as_str())
            .unwrap_or("node")
            .to_string();
        let mut slot_inputs = Vec::new();
        let mut widgets = Vec::new();
        if let Some(io) = n.get("inputs").and_then(|x| x.as_object()) {
            for (in_name, val) in io {
                // a [src_key, slot] pair = a wired link; anything else = a widget value
                let is_link = val
                    .as_array()
                    .map(|a| a.len() >= 2 && a[0].as_str().is_some())
                    .unwrap_or(false);
                if is_link {
                    let a = val.as_array().unwrap();
                    let to_slot = slot_inputs.len();
                    slot_inputs.push(in_name.clone());
                    if let Some(&src_id) = key_id.get(a[0].as_str().unwrap()) {
                        links.push(WLink {
                            from: src_id,
                            from_slot: a[1].as_u64().unwrap_or(0) as usize,
                            to: id,
                            to_slot,
                        });
                    }
                } else {
                    widgets.push(format!("{in_name}: {}", norm_model_ref(&val_str(val))));
                }
            }
        }
        tmp.push((id, title, slot_inputs, widgets));
    }
    // longest-path depth for a left-to-right layout
    let parents: HashMap<i64, Vec<i64>> = {
        let mut m: HashMap<i64, Vec<i64>> = HashMap::new();
        for l in &links {
            m.entry(l.to).or_default().push(l.from);
        }
        m
    };
    fn depth(id: i64, parents: &HashMap<i64, Vec<i64>>, memo: &mut HashMap<i64, i32>) -> i32 {
        if let Some(d) = memo.get(&id) {
            return *d;
        }
        memo.insert(id, 0); // cycle guard
        let d = parents
            .get(&id)
            .map(|ps| {
                ps.iter()
                    .map(|p| depth(*p, parents, memo) + 1)
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        memo.insert(id, d);
        d
    }
    let mut memo = HashMap::new();
    let mut col_count: HashMap<i32, i32> = HashMap::new();
    let out_slots: HashMap<i64, usize> = {
        let mut m: HashMap<i64, usize> = HashMap::new();
        for l in &links {
            let e = m.entry(l.from).or_insert(0);
            *e = (*e).max(l.from_slot + 1);
        }
        m
    };
    let mut nodes = Vec::new();
    for (id, title, inputs, widgets) in tmp {
        let d = depth(id, &parents, &mut memo);
        let row = *col_count.entry(d).or_insert(0);
        col_count.insert(d, row + 1);
        let pos = egui::vec2(d as f32 * 320.0, row as f32 * 190.0);
        let outs = out_slots.get(&id).copied().unwrap_or(1);
        let outputs: Vec<String> = (0..outs).map(|i| format!("out{i}")).collect();
        let size = fit_size(
            egui::vec2(200.0, 100.0),
            inputs.len(),
            outputs.len(),
            widgets.len(),
        );
        nodes.push(WNode {
            id,
            title,
            pos,
            size,
            inputs,
            outputs,
            widgets,
        });
    }
    WGraph { nodes, links }
}

fn in_slot(n: &WNode, i: usize) -> egui::Vec2 {
    n.pos + egui::vec2(0.0, TITLE_H + SLOT_H * (i as f32 + 0.5))
}
fn out_slot(n: &WNode, i: usize) -> egui::Vec2 {
    n.pos + egui::vec2(n.size.x, TITLE_H + SLOT_H * (i as f32 + 0.5))
}

/// Render the graph onto a pan/zoom canvas filling the available area.
pub fn show(ui: &mut egui::Ui, g: &WGraph, view: &mut WfView) {
    let size = ui.available_size();
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::click_and_drag());
    let rect = resp.rect;

    if !view.fitted && !g.nodes.is_empty() {
        let mut min = egui::vec2(f32::INFINITY, f32::INFINITY);
        let mut max = egui::vec2(f32::NEG_INFINITY, f32::NEG_INFINITY);
        for n in &g.nodes {
            min = min.min(n.pos);
            max = max.max(n.pos + n.size);
        }
        let span = (max - min).max(egui::vec2(1.0, 1.0));
        view.zoom = (rect.width() / (span.x + 80.0))
            .min(rect.height() / (span.y + 80.0))
            .clamp(0.05, 1.5);
        view.pan = min - egui::vec2(40.0, 40.0);
        view.fitted = true;
    }

    if resp.dragged() {
        view.pan -= resp.drag_delta() / view.zoom;
    }
    let scroll = ui.input(|i| i.raw_scroll_delta.y);
    if scroll != 0.0 && resp.hovered() {
        let factor = (scroll * 0.0015).exp();
        if let Some(mp) = resp.hover_pos() {
            let before = (mp - rect.min) / view.zoom + view.pan;
            view.zoom = (view.zoom * factor).clamp(0.05, 3.0);
            let after = (mp - rect.min) / view.zoom + view.pan;
            view.pan += before - after;
        }
    }

    let z = view.zoom;
    let tf = |w: egui::Vec2| rect.min + (w - view.pan) * z;
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(18));
    let clip = painter.with_clip_rect(rect);

    let idx: HashMap<i64, &WNode> = g.nodes.iter().map(|n| (n.id, n)).collect();

    // links first (under nodes)
    for l in &g.links {
        if let (Some(a), Some(b)) = (idx.get(&l.from), idx.get(&l.to)) {
            let p0 = tf(out_slot(a, l.from_slot));
            let p1 = tf(in_slot(b, l.to_slot));
            let dx = ((p1.x - p0.x).abs() * 0.5).max(30.0);
            let pts = [p0, p0 + egui::vec2(dx, 0.0), p1 - egui::vec2(dx, 0.0), p1];
            clip.add(CubicBezierShape::from_points_stroke(
                pts,
                false,
                egui::Color32::TRANSPARENT,
                egui::Stroke::new(1.4, egui::Color32::from_gray(140)),
            ));
        }
    }

    // nodes
    for n in &g.nodes {
        let p = tf(n.pos);
        let nrect = egui::Rect::from_min_size(p, n.size * z);
        clip.rect_filled(nrect, 4.0, egui::Color32::from_gray(42));
        clip.rect_stroke(
            nrect,
            4.0,
            egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
        );
        let tbar = egui::Rect::from_min_size(p, egui::vec2(n.size.x * z, TITLE_H * z));
        clip.rect_filled(tbar, 4.0, egui::Color32::from_rgb(58, 70, 96));
        clip.text(
            tbar.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &n.title,
            egui::FontId::proportional((12.0 * z).clamp(6.0, 18.0)),
            egui::Color32::WHITE,
        );
        let label = z > 0.45;
        for (i, name) in n.inputs.iter().enumerate() {
            let sp = tf(in_slot(n, i));
            clip.circle_filled(sp, 3.0, egui::Color32::from_gray(170));
            if label {
                clip.text(
                    sp + egui::vec2(6.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    name,
                    egui::FontId::proportional(10.0 * z),
                    egui::Color32::from_gray(185),
                );
            }
        }
        for (i, name) in n.outputs.iter().enumerate() {
            let sp = tf(out_slot(n, i));
            clip.circle_filled(sp, 3.0, egui::Color32::from_gray(170));
            if label {
                clip.text(
                    sp - egui::vec2(6.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    name,
                    egui::FontId::proportional(10.0 * z),
                    egui::Color32::from_gray(185),
                );
            }
        }
        // widget values in the node body (sampler/steps/cfg/seed/prompt…)
        if label && !n.widgets.is_empty() {
            let slots = n.inputs.len().max(n.outputs.len()) as f32;
            let max_chars = (n.size.x / 6.0) as usize;
            let mut wy = n.pos + egui::vec2(8.0, TITLE_H + SLOT_H * slots + 3.0);
            for w in &n.widgets {
                clip.text(
                    tf(wy),
                    egui::Align2::LEFT_TOP,
                    truncate(w, max_chars.max(6)),
                    egui::FontId::proportional(9.5 * z),
                    egui::Color32::from_gray(165),
                );
                wy.y += LINE_H;
            }
        }
    }
}
