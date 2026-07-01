//! The three workflow tabs + settings. Rendering reads app state via shared
//! borrows and buffers any actions in RefCells, which are drained into worker
//! commands after the UI closures close (keeps the borrow checker happy).

use crate::app::{non_empty, CoverState, SynthetrixApp};
use crate::config::Config;
use crate::worker::{Cmd, CoverReq};
use eframe::egui;
use std::cell::RefCell;

fn gb(size_kb: f64) -> f64 {
    size_kb / 1_048_576.0
}

/// Compact large counts: 1234 -> "1.2k", 2255692 -> "2.3M".
fn human(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// A small status badge painted over a corner of the cover.
fn corner_badge(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32, glyph: &str) {
    let sz = 22.0;
    let r = egui::Rect::from_min_size(rect.left_top() + egui::vec2(5.0, 5.0), egui::vec2(sz, sz));
    let p = ui.painter();
    p.rect_filled(r, egui::Rounding::same(5.0), color);
    p.text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(15.0),
        egui::Color32::from_gray(15),
    );
}

/// Sized placeholder while a cover is fetching or absent.
fn cover_placeholder(ui: &mut egui::Ui, w: f32, h: f32, spinner: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        egui::Rounding::same(4.0),
        egui::Color32::from_gray(38),
    );
    if spinner {
        ui.put(rect, egui::Spinner::new());
    }
}

fn state_badge(status: &str, locked: bool) -> (egui::Color32, String) {
    match status {
        "promoted" if locked => (egui::Color32::from_rgb(80, 200, 120), "🔒 ACTIVE".into()),
        "promoted" => (egui::Color32::from_rgb(80, 200, 120), "● ACTIVE".into()),
        "downloaded" => (egui::Color32::from_rgb(90, 160, 240), "▼ DOWNLOADED".into()),
        _ => (egui::Color32::GRAY, "○ listed".into()),
    }
}

// ---- Dashboard -------------------------------------------------------------

fn stat_box(ui: &mut egui::Ui, label: &str, n: i64) {
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(n.to_string()).heading().strong());
        ui.weak(label);
    });
    ui.add_space(20.0);
}

pub fn dashboard(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.heading("Dashboard — IP cockpit");
    ui.separator();
    match &app.project_info {
        Some(info) => {
            ui.label(
                egui::RichText::new(format!("◉ {}", info.name))
                    .heading()
                    .strong(),
            );
            ui.add_space(4.0);
            egui::Grid::new("proj_grid")
                .num_columns(2)
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Lore root");
                    if info.lore_root_exists {
                        ui.label(&info.lore_root);
                    } else {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 120, 120),
                            format!("{} (missing)", info.lore_root),
                        );
                    }
                    ui.end_row();
                    ui.label("Engine root");
                    ui.label(if info.engine_root.is_empty() {
                        "—"
                    } else {
                        &info.engine_root
                    });
                    ui.end_row();
                    ui.label("Asset vault");
                    ui.label(&info.asset_vault);
                    ui.end_row();
                    ui.label("Project DB");
                    ui.weak(&info.db_path);
                    ui.end_row();
                });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                stat_box(ui, "assets", info.stats.assets);
                stat_box(ui, "jobs", info.stats.jobs);
                stat_box(ui, "prompts", info.stats.prompts);
                stat_box(ui, "lore", info.stats.lore);
            });
            ui.add_space(12.0);
            ui.weak(
                "Coming online for this IP: Forge (prompt→asset), Asset vault, \
                 Prompt matrix, Lore, Releases. The global model vault stays shared \
                 across IPs (Fetcher / Picker / Manifest).",
            );
        }
        None => {
            ui.add_space(20.0);
            ui.weak("No project open. Pick an IP from the switcher (top-right).");
        }
    }
}

// ---- Forge -----------------------------------------------------------------

pub fn forge(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.heading("Forge — text → image");
    if app.project_info.is_none() {
        ui.add_space(16.0);
        ui.weak("No project open. Pick an IP from the switcher (top-right) first.");
        return;
    }
    ui.weak(format!(
        "Backend: local ComfyUI @ {}   ·   output → the IP's asset vault",
        app.config.comfy_url
    ));
    ui.separator();

    let mut submit = false;
    {
        let fu = &mut app.forge_ui;
        ui.horizontal(|ui| {
            ui.label("Entity:");
            ui.add(egui::TextEdit::singleline(&mut fu.entity).desired_width(140.0))
                .on_hover_text("asset name stem, e.g. med-pack-small");
            ui.label("Model (ckpt):");
            ui.add(egui::TextEdit::singleline(&mut fu.model).desired_width(260.0))
                .on_hover_text("checkpoint filename as ComfyUI sees it");
        });
        ui.label("Prompt:");
        ui.add(
            egui::TextEdit::multiline(&mut fu.prompt)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        ui.label("Negative:");
        ui.add(
            egui::TextEdit::multiline(&mut fu.negative)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            ui.label("W");
            ui.add(
                egui::DragValue::new(&mut fu.width)
                    .range(64..=2048)
                    .speed(8),
            );
            ui.label("H");
            ui.add(
                egui::DragValue::new(&mut fu.height)
                    .range(64..=2048)
                    .speed(8),
            );
            ui.label("Steps");
            ui.add(egui::DragValue::new(&mut fu.steps).range(1..=150));
            ui.label("CFG");
            ui.add(
                egui::DragValue::new(&mut fu.cfg)
                    .range(0.0..=30.0)
                    .speed(0.1),
            );
            ui.label("Sampler");
            ui.add(egui::TextEdit::singleline(&mut fu.sampler).desired_width(90.0));
            ui.label("Sched");
            ui.add(egui::TextEdit::singleline(&mut fu.scheduler).desired_width(70.0));
            ui.label("Seed");
            ui.add(egui::TextEdit::singleline(&mut fu.seed).desired_width(90.0));
        });
    }
    ui.add_space(6.0);
    let ready = !app.forge_ui.prompt.trim().is_empty()
        && !app.forge_ui.model.trim().is_empty()
        && !app.busy;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                ready,
                egui::Button::new("✦ Generate").min_size(egui::vec2(150.0, 30.0)),
            )
            .clicked()
        {
            submit = true;
        }
        if app.busy {
            ui.spinner();
            ui.label("generating…");
        }
        if ui
            .button("↻")
            .on_hover_text("refresh jobs/assets")
            .clicked()
        {
            app.send(Cmd::QueryForge);
        }
    });
    if submit {
        let fu = &app.forge_ui;
        let req = crate::backends::GenRequest {
            prompt: fu.prompt.clone(),
            negative: fu.negative.clone(),
            model: fu.model.trim().to_string(),
            width: fu.width,
            height: fu.height,
            steps: fu.steps,
            cfg: fu.cfg,
            sampler: fu.sampler.trim().to_string(),
            scheduler: fu.scheduler.trim().to_string(),
            seed: fu.seed.trim().parse().unwrap_or(-1),
        };
        let entity = fu.entity.clone();
        app.send(Cmd::Generate { req, entity });
    }

    ui.separator();
    egui::ScrollArea::vertical().show(ui, |ui| {
        if !app.forge_assets.is_empty() {
            ui.label(egui::RichText::new("Generated").weak());
            ui.horizontal_wrapped(|ui| {
                for a in &app.forge_assets {
                    if a.media_type.starts_with("image") {
                        ui.add(
                            egui::Image::new(format!("file://{}", a.path))
                                .max_height(128.0)
                                .maintain_aspect_ratio(true)
                                .rounding(4.0),
                        )
                        .on_hover_text(format!(
                            "#{} {} · {} · {} · {}",
                            a.id, a.name, a.kind, a.entity, a.created_at
                        ));
                    }
                }
            });
            ui.separator();
        }
        ui.label(egui::RichText::new("Jobs").weak());
        for j in &app.forge_jobs {
            let (col, glyph) = match j.status.as_str() {
                "done" => (egui::Color32::from_rgb(80, 200, 120), "✔"),
                "failed" => (egui::Color32::from_rgb(220, 100, 100), "✘"),
                _ => (egui::Color32::from_rgb(90, 160, 240), "…"),
            };
            ui.horizontal(|ui| {
                ui.colored_label(col, glyph);
                ui.weak(format!("#{} [{}] {}·{}", j.id, j.status, j.kind, j.backend));
                ui.strong(&j.entity);
                let p: String = j.prompt.chars().take(60).collect();
                ui.label(p);
                if let Some(d) = &j.detail {
                    ui.weak(d);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(&j.created_at);
                    if j.output_path.is_some() {
                        ui.weak("🖼");
                    }
                });
            });
        }
    });
}

// ---- Assets (multi-modal manager) ------------------------------------------

const ASSET_KINDS: [&str; 6] = ["All", "image", "video", "audio", "mesh", "other"];

pub fn assets(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.heading("Assets — the IP's media vault");
    if app.project_info.is_none() {
        ui.add_space(16.0);
        ui.weak("No project open. Pick an IP from the switcher (top-right) first.");
        return;
    }
    let cmds: RefCell<Vec<Cmd>> = RefCell::new(Vec::new());
    let mut do_scan = false;
    let mut do_query = false;
    {
        let au = &mut app.assets_ui;
        ui.horizontal_wrapped(|ui| {
            ui.label("Kind:");
            egui::ComboBox::from_id_source("asset_kind")
                .selected_text(ASSET_KINDS[au.kind_idx.min(5)])
                .show_ui(ui, |ui| {
                    for (i, k) in ASSET_KINDS.iter().enumerate() {
                        if ui.selectable_value(&mut au.kind_idx, i, *k).clicked() {
                            do_query = true;
                        }
                    }
                });
            ui.label("Entity:");
            ui.add(egui::TextEdit::singleline(&mut au.entity).desired_width(140.0));
            if ui.button("Apply").clicked() {
                do_query = true;
            }
            ui.separator();
            if ui
                .button("⟳ Scan vault")
                .on_hover_text("register new media files found in the IP asset vault")
                .clicked()
            {
                do_scan = true;
            }
            ui.separator();
            ui.label("Place topic:");
            ui.add(egui::TextEdit::singleline(&mut au.topic).desired_width(120.0))
                .on_hover_text("engine subfolder: Characters / Props / Weapons / Mechs / Worlds");
        });
    }
    ui.separator();
    ui.weak(format!("{} shown", app.assets.len()));

    let topic = app.assets_ui.topic.trim().to_string();
    let has_engine = app
        .project_info
        .as_ref()
        .map(|i| !i.engine_root.is_empty())
        .unwrap_or(false);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for a in &app.assets {
                egui::Frame::group(ui.style()).rounding(6.0).show(ui, |ui| {
                    ui.set_width(150.0);
                    ui.vertical(|ui| {
                        if a.kind == "image" {
                            ui.add(
                                egui::Image::new(format!("file://{}", a.path))
                                    .fit_to_exact_size(egui::vec2(140.0, 110.0))
                                    .maintain_aspect_ratio(true)
                                    .rounding(4.0),
                            );
                        } else {
                            let glyph = match a.kind.as_str() {
                                "video" => "🎬",
                                "audio" => "♪",
                                "mesh" => "⬡",
                                _ => "◻",
                            };
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(140.0, 110.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(4.0),
                                egui::Color32::from_gray(40),
                            );
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                glyph,
                                egui::FontId::proportional(38.0),
                                egui::Color32::from_gray(180),
                            );
                        }
                        ui.add(
                            egui::Label::new(egui::RichText::new(&a.name).small().strong())
                                .truncate(),
                        )
                        .on_hover_text(&a.path);
                        ui.weak(format!(
                            "{}{}",
                            a.kind,
                            if a.entity.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", a.entity)
                            }
                        ));
                        ui.horizontal(|ui| {
                            if a.engine_path.is_some() {
                                ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "● placed");
                            } else if has_engine
                                && ui
                                    .add_enabled(
                                        !topic.is_empty(),
                                        egui::Button::new("→ engine").small(),
                                    )
                                    .on_hover_text(format!(
                                        "copy into engine/Content/Generated/{topic}"
                                    ))
                                    .clicked()
                            {
                                cmds.borrow_mut().push(Cmd::PlaceAsset {
                                    id: a.id,
                                    topic: topic.clone(),
                                });
                            }
                        });
                    });
                });
            }
        });
    });

    if do_scan {
        app.send(Cmd::ScanAssets);
    }
    if do_query {
        let ku = app.assets_ui.kind_idx;
        let kind = if ku == 0 {
            None
        } else {
            Some(ASSET_KINDS[ku].to_string())
        };
        let entity = if app.assets_ui.entity.trim().is_empty() {
            None
        } else {
            Some(app.assets_ui.entity.trim().to_string())
        };
        app.send(Cmd::QueryAssets { kind, entity });
    }
    for c in cmds.into_inner() {
        app.send(c);
    }
}

// ---- Prompts (the prompt storage matrix) -----------------------------------

/// Positive-anchoring lint: flag IP-bleed brand terms + too-short prompts.
fn prompt_lint(body: &str) -> Option<String> {
    let low = body.to_lowercase();
    let bleed = [
        "warhammer",
        "40k",
        "primaris",
        "space marine",
        "adeptus",
        "halo",
        "spartan",
        "master chief",
        "star wars",
        "stormtrooper",
        "jedi",
        "sith",
        "gundam",
        "mandalorian",
        "witcher",
    ];
    let hits: Vec<&str> = bleed.iter().copied().filter(|t| low.contains(t)).collect();
    if !hits.is_empty() {
        return Some(format!(
            "IP-bleed risk: {} — anchor positively (describe the look) instead of naming the brand",
            hits.join(", ")
        ));
    }
    if body.trim().len() < 8 {
        return Some("very short — add subject + style anchors".into());
    }
    None
}

pub fn prompts(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.heading("Prompts — the prompt storage matrix");
    if app.project_info.is_none() {
        ui.add_space(16.0);
        ui.weak("No project open. Pick an IP from the switcher (top-right) first.");
        return;
    }
    let cmds: RefCell<Vec<Cmd>> = RefCell::new(Vec::new());
    let load_edit: RefCell<Option<crate::project::PromptRow>> = RefCell::new(None);
    let to_forge: RefCell<Option<crate::project::PromptRow>> = RefCell::new(None);
    let mut do_query = false;
    let mut do_import = false;
    let mut new_row = false;
    {
        let pu = &mut app.prompts_ui;
        ui.horizontal_wrapped(|ui| {
            ui.label("Entity filter:");
            ui.add(egui::TextEdit::singleline(&mut pu.filter).desired_width(130.0));
            if ui.button("Apply").clicked() {
                do_query = true;
            }
            ui.separator();
            ui.label("Import:");
            ui.add(egui::TextEdit::singleline(&mut pu.import_entity).desired_width(130.0))
                .on_hover_text("entity whose <entity>/prompts.md to parse from the lore repo");
            if ui.button("⇩ from prompts.md").clicked() {
                do_import = true;
            }
            ui.separator();
            if ui.button("＋ New").clicked() {
                new_row = true;
            }
        });
    }
    ui.separator();

    // editor panel
    let mut save = false;
    let mut cancel = false;
    if app.prompts_ui.editing {
        let pu = &mut app.prompts_ui;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(
                egui::RichText::new(if pu.edit.id > 0 {
                    "Edit prompt"
                } else {
                    "New prompt"
                })
                .strong(),
            );
            egui::Grid::new("p_edit")
                .num_columns(2)
                .spacing([10.0, 3.0])
                .show(ui, |ui| {
                    ui.label("Entity");
                    ui.text_edit_singleline(&mut pu.edit.entity);
                    ui.end_row();
                    ui.label("Slot");
                    ui.text_edit_singleline(&mut pu.edit.slot);
                    ui.end_row();
                    ui.label("Stage");
                    ui.text_edit_singleline(&mut pu.edit.stage);
                    ui.end_row();
                    ui.label("Backend");
                    ui.text_edit_singleline(&mut pu.edit.backend);
                    ui.end_row();
                    ui.label("Model");
                    ui.text_edit_singleline(&mut pu.edit.model);
                    ui.end_row();
                });
            ui.label("Body:");
            ui.add(
                egui::TextEdit::multiline(&mut pu.edit.body)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
            if let Some(w) = prompt_lint(&pu.edit.body) {
                ui.colored_label(egui::Color32::from_rgb(230, 170, 60), format!("⚠ {w}"));
            }
            ui.horizontal(|ui| {
                if ui.button("💾 Save").clicked() {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        ui.separator();
    }

    // list
    egui::ScrollArea::vertical().show(ui, |ui| {
        for p in &app.prompts {
            ui.horizontal(|ui| {
                ui.strong(&p.entity);
                ui.label(egui::RichText::new(&p.slot).italics());
                ui.weak(format!("[{}·{}]", p.stage, p.backend));
                if !p.model.is_empty() {
                    ui.weak(&p.model);
                }
                if prompt_lint(&p.body).is_some() {
                    ui.colored_label(egui::Color32::from_rgb(230, 170, 60), "⚠");
                }
                let preview: String = p.body.chars().take(60).collect();
                ui.label(preview);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("🗑").clicked() {
                        cmds.borrow_mut().push(Cmd::DeletePrompt(p.id));
                    }
                    if ui.small_button("→ Forge").clicked() {
                        *to_forge.borrow_mut() = Some(p.clone());
                    }
                    if ui.small_button("edit").clicked() {
                        *load_edit.borrow_mut() = Some(p.clone());
                    }
                    if !p.updated_at.is_empty() {
                        ui.weak(&p.updated_at);
                    }
                });
            });
            ui.separator();
        }
    });

    // drain
    if do_query {
        let entity = if app.prompts_ui.filter.trim().is_empty() {
            None
        } else {
            Some(app.prompts_ui.filter.trim().to_string())
        };
        app.send(Cmd::QueryPrompts { entity });
    }
    if do_import {
        app.send(Cmd::ImportPrompts {
            entity: app.prompts_ui.import_entity.clone(),
        });
    }
    if new_row {
        app.prompts_ui.edit = crate::project::PromptRow {
            backend: "comfy_local".into(),
            stage: "2d".into(),
            ..Default::default()
        };
        app.prompts_ui.editing = true;
    }
    if save {
        app.send(Cmd::SavePrompt(app.prompts_ui.edit.clone()));
        app.prompts_ui.editing = false;
    }
    if cancel {
        app.prompts_ui.editing = false;
    }
    if let Some(r) = load_edit.into_inner() {
        app.prompts_ui.edit = r;
        app.prompts_ui.editing = true;
    }
    if let Some(r) = to_forge.into_inner() {
        app.forge_ui.prompt = r.body;
        if !r.model.is_empty() {
            app.forge_ui.model = r.model;
        }
        app.forge_ui.entity = r.entity;
        app.tab = crate::app::Tab::Forge;
    }
    for c in cmds.into_inner() {
        app.send(c);
    }
}

// ---- Lore ------------------------------------------------------------------

/// Colour a lore kind consistently in the chip row + list.
fn lore_kind_color(kind: &str) -> egui::Color32 {
    match kind {
        "characters" => egui::Color32::from_rgb(120, 190, 240),
        "factions" => egui::Color32::from_rgb(230, 150, 90),
        "vehicles" => egui::Color32::from_rgb(150, 200, 130),
        "weapons" => egui::Color32::from_rgb(220, 120, 120),
        "world" => egui::Color32::from_rgb(160, 200, 200),
        "concepts" => egui::Color32::from_rgb(200, 160, 220),
        "timeline" => egui::Color32::from_rgb(210, 200, 120),
        "root" => egui::Color32::GRAY,
        _ => egui::Color32::from_gray(150),
    }
}

pub fn lore(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.heading("Lore — the IP's universal scaffolding");
    if app.project_info.is_none() {
        ui.add_space(16.0);
        ui.weak("No project open. Pick an IP from the switcher (top-right) first.");
        return;
    }
    let lore_root = app
        .project_info
        .as_ref()
        .map(|i| i.lore_root.clone())
        .unwrap_or_default();
    ui.weak(format!(
        "Indexed from {lore_root} · {} docs · reader is read-only (git repo is source of truth)",
        app.lore.len()
    ));
    ui.separator();

    let mut do_query = false;
    let mut do_reindex = false;
    let mut set_kind: Option<Option<String>> = None; // outer Some = "changed"
    let open_id: RefCell<Option<i64>> = RefCell::new(None);
    let to_prompt: RefCell<Option<String>> = RefCell::new(None);

    // controls
    {
        let lu = &mut app.lore_ui;
        ui.horizontal_wrapped(|ui| {
            ui.label("Search:");
            let r = ui.add(egui::TextEdit::singleline(&mut lu.search).desired_width(180.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                do_query = true;
            }
            if ui.button("Find").clicked() {
                do_query = true;
            }
            ui.separator();
            if ui.button("↻ Reindex").clicked() {
                do_reindex = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            let all_sel = lu.kind.is_none();
            if ui.selectable_label(all_sel, "all").clicked() && !all_sel {
                set_kind = Some(None);
            }
            for k in &app.lore_kinds {
                let sel = lu.kind.as_deref() == Some(k.as_str());
                let label = egui::RichText::new(k).color(lore_kind_color(k));
                if ui.selectable_label(sel, label).clicked() && !sel {
                    set_kind = Some(Some(k.clone()));
                }
            }
        });
    }
    ui.separator();

    // split: list on the left, reader on the right
    ui.columns(2, |cols| {
        // --- left: entry list ---
        egui::ScrollArea::vertical()
            .id_source("lore_list")
            .show(&mut cols[0], |ui| {
                for e in &app.lore {
                    let open = app.lore_ui.open.as_ref().map(|(o, _)| o.id) == Some(e.id);
                    let resp = ui.selectable_label(
                        open,
                        egui::RichText::new(format!("● {}", e.title))
                            .color(lore_kind_color(&e.kind)),
                    );
                    if resp.clicked() {
                        *open_id.borrow_mut() = Some(e.id);
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(14.0);
                        ui.weak(format!("{} · {}", e.kind, e.name));
                    });
                    if !e.summary.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(14.0);
                            ui.label(egui::RichText::new(&e.summary).small());
                        });
                    }
                    ui.separator();
                }
                if app.lore.is_empty() {
                    ui.add_space(20.0);
                    ui.weak("No lore indexed. Click ↻ Reindex to scan the lore repo.");
                }
            });

        // --- right: reader ---
        egui::ScrollArea::vertical()
            .id_source("lore_reader")
            .show(&mut cols[1], |ui| match &app.lore_ui.open {
                Some((entry, body)) => {
                    ui.heading(&entry.title);
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(lore_kind_color(&entry.kind), &entry.kind);
                        ui.weak(&entry.rel_path);
                        if !entry.updated_at.is_empty() {
                            ui.weak(format!("· indexed {}", entry.updated_at));
                        }
                    });
                    if !entry.vocab.is_empty() {
                        ui.add_space(2.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.small("vocab:");
                            ui.label(egui::RichText::new(&entry.vocab).small().italics());
                        });
                        if ui
                            .small_button("→ Prompts")
                            .on_hover_text("stage this entry's name for a prompts.md import")
                            .clicked()
                        {
                            *to_prompt.borrow_mut() = Some(entry.name.clone());
                        }
                    }
                    ui.separator();
                    // read-only markdown source view
                    let mut src = body.clone();
                    ui.add(
                        egui::TextEdit::multiline(&mut src)
                            .desired_rows(30)
                            .desired_width(f32::INFINITY)
                            .code_editor()
                            .interactive(false),
                    );
                }
                None => {
                    ui.add_space(20.0);
                    ui.weak("Select an entry on the left to read it.");
                }
            });
    });

    // drain
    if let Some(k) = set_kind {
        app.lore_ui.kind = k.clone();
        app.send(Cmd::QueryLore {
            kind: k,
            search: non_empty(&app.lore_ui.search),
        });
    }
    if do_query {
        app.send(Cmd::QueryLore {
            kind: app.lore_ui.kind.clone(),
            search: non_empty(&app.lore_ui.search),
        });
    }
    if do_reindex {
        app.send(Cmd::ReindexLore);
    }
    if let Some(id) = open_id.into_inner() {
        app.send(Cmd::ReadLore(id));
    }
    if let Some(name) = to_prompt.into_inner() {
        app.prompts_ui.import_entity = name;
        app.tab = crate::app::Tab::Prompts;
    }
}

// ---- Fetcher ---------------------------------------------------------------

pub fn fetcher(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.heading("Fetcher — sync the list from CivitAI Red");
    ui.label("Crawls model metadata into the local manifest. Covers load on demand in the Picker.");
    ui.separator();

    let has_token = app.config.effective_token().is_some();
    let combos = app.config.types.len() * app.config.base_models.len() * 3;

    ui.horizontal(|ui| {
        ui.label("Token:");
        if has_token {
            ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "✔ present");
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(240, 120, 120),
                "✘ missing — set it in Settings",
            );
        }
        ui.separator();
        ui.weak(format!(
            "{} types × {} base × 3 rankings = {combos} combos, top {}, NSFW {}",
            app.config.types.len(),
            app.config.base_models.len(),
            app.config.top_n,
            if app.config.nsfw { "on" } else { "off" }
        ));
    });

    ui.add_space(10.0);
    let mut start = false;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!app.busy && has_token, |ui| {
            if ui
                .add(
                    egui::Button::new("⬇  Sync from CivitAI Red").min_size(egui::vec2(220.0, 34.0)),
                )
                .clicked()
            {
                start = true;
            }
        });
        if app.busy {
            ui.spinner();
            ui.label("syncing…");
        }
    });

    // --- live progress ---
    if let Some((done, total, rows, unique)) = app.sync {
        ui.add_space(8.0);
        let frac = if total > 0 {
            done as f32 / total as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(frac)
                .text(format!("{done}/{total} combos"))
                .desired_width(ui.available_width().min(700.0)),
        );
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{unique}")).heading().strong());
            ui.label("unique models");
            ui.separator();
            ui.label(egui::RichText::new(format!("{rows}")).heading());
            ui.label("rows processed");
        });
    }

    // --- live log ---
    if !app.log.is_empty() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Log").weak());
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(ui.available_height() - 12.0)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &app.log {
                        ui.monospace(line);
                    }
                });
        });
    }

    if start {
        app.log.clear();
        app.sync = Some((0, combos, 0, 0));
        app.send(Cmd::Sync);
    }
}

// ---- Picker ----------------------------------------------------------------

pub fn picker(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    let cmds: RefCell<Vec<Cmd>> = RefCell::new(Vec::new());
    let sel_toggle: RefCell<Vec<i64>> = RefCell::new(Vec::new());
    let info_toggle: RefCell<Option<i64>> = RefCell::new(None);
    let mut do_refresh = false;
    let mut select_all = false;
    let mut select_none = false;

    // --- filter bar ---
    let types = app.config.types.clone();
    {
        let pu = &mut app.picker_ui;
        ui.horizontal_wrapped(|ui| {
            ui.label("Type:");
            egui::ComboBox::from_id_source("type_combo")
                .selected_text(if pu.type_idx == 0 {
                    "All".to_string()
                } else {
                    types.get(pu.type_idx - 1).cloned().unwrap_or_default()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut pu.type_idx, 0, "All");
                    for (i, t) in types.iter().enumerate() {
                        ui.selectable_value(&mut pu.type_idx, i + 1, t);
                    }
                });
            ui.label("Base:");
            ui.add(egui::TextEdit::singleline(&mut pu.base).desired_width(110.0));
            ui.label("Search:");
            ui.add(egui::TextEdit::singleline(&mut pu.search).desired_width(140.0));
            ui.checkbox(&mut pu.only_downloaded, "downloaded only");
            ui.label("min dl:");
            ui.add(egui::TextEdit::singleline(&mut pu.min_downloads).desired_width(60.0));
            ui.label("limit:");
            ui.add(egui::TextEdit::singleline(&mut pu.limit).desired_width(50.0));
            if ui.button("Apply").clicked() {
                do_refresh = true;
            }
            ui.separator();
            ui.label("Sort:");
            egui::ComboBox::from_id_source("sort_combo")
                .selected_text(if pu.sort_idx == 1 {
                    "👍 Likes"
                } else {
                    "⬇ Downloads"
                })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut pu.sort_idx, 0, "⬇ Downloads")
                        .clicked()
                    {
                        do_refresh = true;
                    }
                    if ui
                        .selectable_value(&mut pu.sort_idx, 1, "👍 Likes")
                        .clicked()
                    {
                        do_refresh = true;
                    }
                });
            ui.separator();
            ui.add(
                egui::Slider::new(&mut pu.cover_px, 120.0..=400.0)
                    .step_by(20.0)
                    .text("card size"),
            );
        });
    }

    // --- batch action bar ---
    ui.separator();
    let sel_count = app.selected.len();
    ui.horizontal(|ui| {
        ui.label(format!("{} selected", sel_count));
        if ui.button("Select all").clicked() {
            select_all = true;
        }
        if ui.button("Clear").clicked() {
            select_none = true;
        }
        ui.separator();
        let en = sel_count > 0 && !app.busy;
        let per = app.config.per_model;
        if ui
            .add_enabled(en, egui::Button::new("⬇ Download selected"))
            .clicked()
        {
            for id in &app.selected {
                cmds.borrow_mut().push(Cmd::Download {
                    file_id: *id,
                    promote: false,
                    images: per,
                });
            }
        }
        if ui
            .add_enabled(en, egui::Button::new("⬇🔥 Download + hotload selected"))
            .clicked()
        {
            for id in &app.selected {
                cmds.borrow_mut().push(Cmd::Download {
                    file_id: *id,
                    promote: true,
                    images: per,
                });
            }
        }
    });
    ui.separator();

    // --- results table ---
    if app.picks.is_empty() {
        ui.add_space(20.0);
        ui.weak("No rows. Set filters and press Apply (run a Fetcher sync first if the catalog is empty).");
    }
    let to_resolve: RefCell<Vec<(i64, Option<String>)>> = RefCell::new(Vec::new());
    let covers_dir = app.config.covers_dir();
    let picks = &app.picks;
    let selected = &app.selected;
    let covers = &app.covers;
    let per = app.config.per_model;
    let cover_px = app.picker_ui.cover_px;
    let card_w = cover_px;
    let card_h = cover_px + 118.0; // cover + text block + buttons
    let accent = egui::Color32::from_rgb(90, 160, 240);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for row in picks {
                let is_sel = selected.contains(&row.file_id);
                let downloaded = row.status == "downloaded";
                let active = row.status == "promoted";
                let mut frame = egui::Frame::group(ui.style())
                    .rounding(6.0)
                    .inner_margin(egui::Margin::same(6.0));
                // tint the whole card by ownership state
                if active {
                    frame = frame.fill(egui::Color32::from_rgb(28, 46, 38));
                } else if downloaded {
                    frame = frame.fill(egui::Color32::from_rgb(26, 40, 30));
                }
                // border: selection wins, else a state-colored edge
                if is_sel {
                    frame = frame.stroke(egui::Stroke::new(2.5, accent));
                } else if active {
                    frame = frame.stroke(egui::Stroke::new(
                        1.5,
                        egui::Color32::from_rgb(80, 200, 120),
                    ));
                } else if downloaded {
                    frame =
                        frame.stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 150, 90)));
                }
                ui.allocate_ui(egui::vec2(card_w + 16.0, card_h), |ui| {
                    frame.show(ui, |ui| {
                        ui.set_width(card_w);
                        ui.vertical(|ui| {
                            // --- cover (click toggles selection) ---
                            match covers.get(&row.model_id) {
                                Some(CoverState::Ready(p)) => {
                                    let img = egui::Image::new(format!("file://{p}"))
                                        .fit_to_exact_size(egui::vec2(card_w, cover_px))
                                        .maintain_aspect_ratio(true)
                                        .rounding(4.0);
                                    let resp = ui
                                        .add(egui::ImageButton::new(img).frame(false))
                                        .on_hover_text("click to select");
                                    if resp.clicked() {
                                        sel_toggle.borrow_mut().push(row.file_id);
                                    }
                                    // ownership badge on the cover corner
                                    if active {
                                        corner_badge(
                                            ui,
                                            resp.rect,
                                            egui::Color32::from_rgb(255, 165, 60),
                                            "✓",
                                        );
                                    } else if downloaded {
                                        corner_badge(
                                            ui,
                                            resp.rect,
                                            egui::Color32::from_rgb(80, 200, 120),
                                            "✓",
                                        );
                                    }
                                }
                                Some(CoverState::Requested) => {
                                    cover_placeholder(ui, card_w, cover_px, true)
                                }
                                Some(CoverState::Missing) => {
                                    cover_placeholder(ui, card_w, cover_px, false)
                                }
                                None => {
                                    to_resolve
                                        .borrow_mut()
                                        .push((row.model_id, row.cover_url.clone()));
                                    cover_placeholder(ui, card_w, cover_px, true);
                                }
                            }
                            // --- header: select + state + nsfw + info ---
                            ui.horizontal(|ui| {
                                let mut sel = is_sel;
                                if ui.checkbox(&mut sel, "").changed() {
                                    sel_toggle.borrow_mut().push(row.file_id);
                                }
                                let (col, badge) = state_badge(&row.status, row.locked);
                                ui.colored_label(col, badge);
                                if row.nsfw {
                                    ui.colored_label(egui::Color32::from_rgb(220, 100, 100), "🔞");
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button("ℹ")
                                            .on_hover_text("info / trigger words")
                                            .clicked()
                                        {
                                            *info_toggle.borrow_mut() = Some(row.model_id);
                                        }
                                    },
                                );
                            });
                            // --- title (truncated) + meta ---
                            ui.add(
                                egui::Label::new(egui::RichText::new(&row.model_name).strong())
                                    .truncate(),
                            )
                            .on_hover_text(format!(
                                "{}\nhttps://civitai.com/models/{}",
                                row.model_name, row.model_id
                            ));
                            ui.weak(format!(
                                "{} · {} · {}",
                                row.model_type,
                                row.base_model,
                                if row.size_kb > 0.0 {
                                    format!("{:.2} GB", gb(row.size_kb))
                                } else {
                                    "—".into()
                                }
                            ));
                            ui.weak(format!(
                                "⬇ {}   👍 {}",
                                human(row.downloads),
                                human(row.thumbs_up)
                            ));
                            // --- actions ---
                            ui.horizontal(|ui| {
                                if row.status == "indexed" {
                                    if ui.small_button("⬇").on_hover_text("Download").clicked() {
                                        cmds.borrow_mut().push(Cmd::Download {
                                            file_id: row.file_id,
                                            promote: false,
                                            images: per,
                                        });
                                    }
                                    if ui
                                        .small_button("⬇🔥")
                                        .on_hover_text("Download + hotload to NVMe")
                                        .clicked()
                                    {
                                        cmds.borrow_mut().push(Cmd::Download {
                                            file_id: row.file_id,
                                            promote: true,
                                            images: per,
                                        });
                                    }
                                } else if ui
                                    .small_button("🔥")
                                    .on_hover_text("Hotload → NVMe")
                                    .clicked()
                                {
                                    cmds.borrow_mut().push(Cmd::Promote(row.file_id));
                                }
                            });
                        });
                    });
                });
            }
        });
    });

    // --- drain buffered actions ---
    if do_refresh {
        app.refresh_picks();
    }
    if select_all {
        for r in &app.picks {
            app.selected.insert(r.file_id);
        }
    }
    if select_none {
        app.selected.clear();
    }
    // Resolve covers: prefer the on-disk cache (persists across launches); only
    // ask the worker to fetch a genuine miss. Never re-pulls what's already saved.
    for (mid, url) in to_resolve.into_inner() {
        let mut found = None;
        for ext in ["jpg", "png", "webp"] {
            let p = covers_dir.join(format!("{mid}.{ext}"));
            if p.exists() {
                found = Some(p.to_string_lossy().into_owned());
                break;
            }
        }
        if let Some(p) = found {
            app.covers.insert(mid, CoverState::Ready(p));
        } else if let Some(u) = url {
            app.covers.insert(mid, CoverState::Requested);
            let _ = app.covers_pool.tx.send(CoverReq {
                model_id: mid,
                url: u,
            });
        } else {
            app.covers.insert(mid, CoverState::Missing);
        }
    }
    for id in sel_toggle.into_inner() {
        if !app.selected.remove(&id) {
            app.selected.insert(id);
        }
    }
    for c in cmds.into_inner() {
        app.send(c);
    }

    // --- info overlay: toggle open/close from the ℹ buttons ---
    if let Some(id) = info_toggle.into_inner() {
        if app.info_open == Some(id) {
            app.info_open = None;
        } else {
            let tw = app
                .picks
                .iter()
                .find(|r| r.model_id == id)
                .map(|r| {
                    r.trained_words
                        .trim_matches(|c| c == '[' || c == ']')
                        .replace('"', "")
                        .replace(',', ", ")
                })
                .unwrap_or_default();
            app.info_text = if tw.trim().is_empty() {
                "(this model has no trigger words)".into()
            } else {
                tw
            };
            app.info_open = Some(id);
        }
    }

    // --- render the info overlay window ---
    if let Some(id) = app.info_open {
        let name = app
            .picks
            .iter()
            .find(|r| r.model_id == id)
            .map(|r| r.model_name.clone())
            .unwrap_or_else(|| format!("Model {id}"));
        let mut open = true;
        egui::Window::new(format!("ℹ  {name}"))
            .id(egui::Id::new(("info", id)))
            .collapsible(false)
            .resizable(true)
            .default_width(380.0)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.label(egui::RichText::new("Trigger words").strong());
                ui.add(
                    egui::TextEdit::multiline(&mut app.info_text)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
                ui.horizontal(|ui| {
                    if ui.button("📋 Copy").clicked() {
                        let t = app.info_text.clone();
                        ui.output_mut(|o| o.copied_text = t);
                    }
                    ui.hyperlink_to(
                        "Open on CivitAI",
                        format!("https://civitai.com/models/{id}"),
                    );
                });
            });
        if !open {
            app.info_open = None;
        }
    }
}

// ---- Manifest --------------------------------------------------------------

pub fn manifest(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    let cmds: RefCell<Vec<Cmd>> = RefCell::new(Vec::new());
    let mut do_audit = false;
    let mut do_heal = false;
    let mut do_refresh = false;
    let mut do_harvest = false;
    let mut do_recover = false;

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.heading("Manifest — local registry");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("↻ Refresh").clicked() {
                do_refresh = true;
            }
            if ui
                .add_enabled(!app.busy, egui::Button::new("🩺 Audit"))
                .clicked()
            {
                do_audit = true;
            }
            if app.audit.is_some() && ui.button("🛠 Heal").clicked() {
                do_heal = true;
            }
            // recover unmatched orphans by hash via CivitAI
            let has_orphans = app.audit.as_ref().is_some_and(|a| !a.orphans.is_empty());
            if ui
                .add_enabled(!app.busy && has_orphans, egui::Button::new("🔍 Recover"))
                .on_hover_text("Identify orphan files by hash via CivitAI and adopt matches")
                .clicked()
            {
                do_recover = true;
            }
            if ui
                .add_enabled(!app.busy, egui::Button::new("📸 Capture images"))
                .on_hover_text("Harvest example images + workflows for all tracked models")
                .clicked()
            {
                do_harvest = true;
            }
        });
    });

    let active = app
        .manifest
        .iter()
        .filter(|r| r.status == "promoted")
        .count();
    let downloaded = app.manifest.len() - active;
    let nvme_gb: f64 = app
        .manifest
        .iter()
        .filter(|r| r.status == "promoted")
        .map(|r| gb(r.size_kb))
        .sum();
    ui.label(format!(
        "{} downloaded · {} active on NVMe ({:.1} GB hotloaded)",
        downloaded, active, nvme_gb
    ));

    if let Some(rep) = &app.audit {
        ui.separator();
        ui.label(
            egui::RichText::new(format!(
                "Audit: {} checked · {} missing-vault · {} missing-nvme · {} orphans",
                rep.checked,
                rep.missing_vault.len(),
                rep.missing_nvme.len(),
                rep.orphans.len()
            ))
            .italics(),
        );
        for o in rep.orphans.iter().take(8) {
            ui.weak(format!("orphan: {o}"));
        }
    }
    ui.separator();

    let exp_toggle: RefCell<Vec<i64>> = RefCell::new(Vec::new());
    // buffered lightbox open: (model_id, image_path, wf_path, pr_path, show_info)
    let lb_open: RefCell<Option<(i64, String, Option<String>, Option<String>, bool)>> =
        RefCell::new(None);
    let rows = &app.manifest;
    let expanded = &app.manifest_expanded;
    let imgs_map = &app.manifest_imgs;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for r in rows {
            let is_exp = expanded.contains(&r.model_id);
            ui.horizontal(|ui| {
                let tri = if is_exp { "▼" } else { "▶" };
                if ui.small_button(tri).on_hover_text("show captured images").clicked() {
                    exp_toggle.borrow_mut().push(r.model_id);
                }
                let (col, badge) = state_badge(&r.status, r.locked);
                ui.colored_label(col, badge);
                // clicking the name also toggles the strip
                if ui
                    .add(
                        egui::Label::new(egui::RichText::new(&r.model_name).strong())
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text("click to show captured images")
                    .clicked()
                {
                    exp_toggle.borrow_mut().push(r.model_id);
                }
                let resp = ui.weak(format!(
                    "[{}] {} · {:.2}GB",
                    r.model_type, r.file_name, gb(r.size_kb)
                ));
                if let Some(sha) = &r.sha256 {
                    resp.on_hover_text(format!("sha256: {sha}"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if r.status == "promoted" {
                        let lock_label = if r.locked { "Unlock" } else { "Lock" };
                        if ui.small_button(lock_label).clicked() {
                            cmds.borrow_mut().push(Cmd::Lock(r.file_id, !r.locked));
                        }
                        if ui.small_button("Evict").clicked() {
                            cmds.borrow_mut().push(Cmd::Evict(r.file_id));
                        }
                    } else if ui.small_button("Hotload → NVMe").clicked() {
                        cmds.borrow_mut().push(Cmd::Promote(r.file_id));
                    }
                });
            });
            // --- expanded: horizontal strip of captured images ---
            if is_exp {
                match imgs_map.get(&r.model_id) {
                    Some(paths) if !paths.is_empty() => {
                        egui::ScrollArea::horizontal()
                            .id_source(("strip", r.file_id))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    for (img, wf, pr) in paths {
                                        ui.allocate_ui(egui::vec2(120.0, 148.0), |ui| {
                                            ui.vertical(|ui| {
                                                // click the image -> full-size silverbox
                                                let resp = ui
                                                    .add(
                                                        egui::ImageButton::new(
                                                            egui::Image::new(format!("file://{img}"))
                                                                .fit_to_exact_size(egui::vec2(120.0, 120.0))
                                                                .maintain_aspect_ratio(true)
                                                                .rounding(4.0),
                                                        )
                                                        .frame(false),
                                                    )
                                                    .on_hover_text("click for full size");
                                                if resp.clicked() {
                                                    *lb_open.borrow_mut() = Some((
                                                        r.model_id,
                                                        img.clone(),
                                                        wf.clone(),
                                                        pr.clone(),
                                                        false,
                                                    ));
                                                }
                                                // info button replaces the WF/A1 badges: opens the
                                                // same silverbox on the info view, converting the
                                                // missing side on demand.
                                                ui.horizontal(|ui| {
                                                    if ui
                                                        .small_button("ⓘ")
                                                        .on_hover_text(
                                                            "info — workflow + params (synthesizes the missing side)",
                                                        )
                                                        .clicked()
                                                    {
                                                        *lb_open.borrow_mut() = Some((
                                                            r.model_id,
                                                            img.clone(),
                                                            wf.clone(),
                                                            pr.clone(),
                                                            true,
                                                        ));
                                                    }
                                                    let tag = match (wf.is_some(), pr.is_some()) {
                                                        (true, true) => "WF·A1",
                                                        (true, false) => "WF",
                                                        (false, true) => "A1",
                                                        (false, false) => "—",
                                                    };
                                                    ui.weak(tag);
                                                });
                                            });
                                        });
                                    }
                                });
                            });
                    }
                    Some(_) => {
                        ui.weak("  no captured images — run 📸 Capture images");
                    }
                    None => {
                        ui.weak("  loading…");
                    }
                }
            }
            ui.separator();
        }
    });

    // resolve expand toggles: flip state, and scan the gallery dir on expand
    let gallery_root = app.config.gallery_root.clone();
    for mid in exp_toggle.into_inner() {
        if app.manifest_expanded.remove(&mid) {
            continue; // collapsed
        }
        app.manifest_expanded.insert(mid);
        let dir = std::path::Path::new(&gallery_root).join(mid.to_string());
        let mut paths: Vec<(String, Option<String>, Option<String>)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
                if matches!(ext, "png" | "jpg" | "jpeg" | "webp") {
                    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    let wf = dir.join(format!("{stem}.workflow.json"));
                    let pr = dir.join(format!("{stem}.params.txt"));
                    let opt = |q: std::path::PathBuf| {
                        if q.exists() {
                            Some(q.to_string_lossy().into_owned())
                        } else {
                            None
                        }
                    };
                    paths.push((p.to_string_lossy().into_owned(), opt(wf), opt(pr)));
                }
            }
        }
        paths.sort_by(|a, b| a.0.cmp(&b.0));
        app.manifest_imgs.insert(mid, paths);
    }

    // open the silverbox for a clicked thumbnail / info button (converts + caches
    // the missing side), then reflect any new files back into the strip cache.
    if let Some((mid, img, wf, pr, info)) = lb_open.into_inner() {
        let lb = crate::app::Lightbox::open(mid, &img, wf, pr, info);
        if let Some(list) = app.manifest_imgs.get_mut(&mid) {
            if let Some(entry) = list.iter_mut().find(|(p, _, _)| p == &img) {
                entry.1 = lb.wf_path.clone();
                entry.2 = lb.pr_path.clone();
            }
        }
        app.lightbox = Some(lb);
    }

    if do_refresh {
        app.send(Cmd::QueryManifest);
    }
    if do_audit {
        app.send(Cmd::Audit);
    }
    if do_heal {
        if let Some(rep) = app.audit.clone() {
            app.send(Cmd::Heal(rep));
        }
    }
    if do_recover {
        if let Some(rep) = app.audit.as_ref() {
            app.send(Cmd::RecoverOrphans(rep.orphans.clone()));
        }
    }
    if do_harvest {
        app.send(Cmd::HarvestImages);
    }
    for c in cmds.into_inner() {
        app.send(c);
    }

    // --- the silverbox: full-size image + workflow/params, one overlay ---
    // grabbed before the &mut app.lightbox borrow below (config is on app too)
    let comfy_vault = app.config.vault_root.clone();
    let comfy_nvme = app.config.nvme_root.clone();
    let mut close_lb = false;
    if let Some(lb) = app.lightbox.as_mut() {
        let fname = std::path::Path::new(&lb.image_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("image")
            .to_string();
        let mut open = true;
        egui::Window::new(format!("🖼  {fname}"))
            .id(egui::Id::new(("lightbox", lb.model_id)))
            .open(&mut open)
            .default_size([1040.0, 760.0])
            .resizable(true)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut lb.show_info, false, "🖼 Image");
                    ui.selectable_value(&mut lb.show_info, true, "ⓘ Workflow + Params");
                    ui.separator();
                    let has_wf = lb.wf_path.is_some();
                    if ui
                        .add_enabled(has_wf, egui::Button::new("Open workflow in ComfyUI"))
                        .on_hover_text(
                            "Embed the workflow in the image, hand it to the running ComfyUI \
                             (:8188), and open it — drops the image straight onto the canvas",
                        )
                        .on_disabled_hover_text("no workflow for this image")
                        .clicked()
                    {
                        let img = lb.image_path.clone();
                        let wf = lb
                            .wf_path
                            .clone()
                            .and_then(|p| std::fs::read_to_string(p).ok());
                        let vault = comfy_vault.clone();
                        let nvme = comfy_nvme.clone();
                        std::thread::spawn(move || {
                            if let Err(e) =
                                crate::comfy::open_in_comfy(&img, wf.as_deref(), &vault, &nvme)
                            {
                                eprintln!("[synthetrix] open in ComfyUI: {e}");
                            }
                        });
                        lb.note = Some("Sent to ComfyUI — it should open the workflow.".into());
                    }
                    if let Some(note) = &lb.note {
                        ui.separator();
                        ui.colored_label(egui::Color32::from_rgb(120, 200, 140), note);
                    }
                });
                ui.separator();
                if !lb.show_info {
                    let avail = ui.available_size();
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.add(
                            egui::Image::new(format!("file://{}", lb.image_path))
                                .maintain_aspect_ratio(true)
                                .max_size(avail)
                                .rounding(6.0),
                        );
                    });
                } else {
                    ui.columns(2, |c| {
                        c[0].strong("Workflow");
                        c[0].weak("drag = pan · scroll = zoom");
                        match lb.wf_graph.as_ref().filter(|g| !g.nodes.is_empty()) {
                            Some(g) => crate::wfgraph::show(&mut c[0], g, &mut lb.wf_view),
                            None => {
                                c[0].weak("No workflow graph.");
                            }
                        }
                        c[1].horizontal(|ui| {
                            ui.strong("A1111 parameters");
                            if ui.button("📋 Copy").clicked() {
                                let t = lb.params_text.clone();
                                ui.output_mut(|o| o.copied_text = t);
                            }
                        });
                        egui::ScrollArea::vertical()
                            .id_source("lb_params")
                            .show(&mut c[1], |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut lb.params_text)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(20)
                                        .font(egui::TextStyle::Monospace),
                                );
                            });
                    });
                }
            });
        if !open {
            close_lb = true;
        }
    }
    if close_lb {
        app.lightbox = None;
    }
}

// ---- Settings --------------------------------------------------------------

pub fn settings(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    let mut save = false;
    let mut dark_changed = false;
    {
        let cfg = &mut app.config;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading("Settings");
            ui.separator();

            ui.label("CivitAI token (leave blank to use $CIVITAI_TOKEN):");
            ui.add(
                egui::TextEdit::singleline(&mut cfg.token)
                    .desired_width(440.0)
                    .password(true),
            );

            ui.add_space(8.0);
            ui.label("Storage tiers");
            egui::Grid::new("paths").num_columns(2).show(ui, |ui| {
                ui.label("Vault root (HDD)");
                ui.add(egui::TextEdit::singleline(&mut cfg.vault_root).desired_width(440.0));
                ui.end_row();
                ui.label("Catalog dir");
                ui.add(egui::TextEdit::singleline(&mut cfg.catalog_dir).desired_width(440.0));
                ui.end_row();
                ui.label("Gallery root");
                ui.add(egui::TextEdit::singleline(&mut cfg.gallery_root).desired_width(440.0));
                ui.end_row();
                ui.label("NVMe root");
                ui.add(egui::TextEdit::singleline(&mut cfg.nvme_root).desired_width(440.0));
                ui.end_row();
            });

            ui.add_space(8.0);
            ui.label("Crawl");
            ui.add(egui::Slider::new(&mut cfg.top_n, 10..=500).text("top N per combo"));
            ui.add(
                egui::Slider::new(&mut cfg.per_model, 1..=40).text("images per model on download"),
            );
            ui.checkbox(&mut cfg.nsfw, "Include NSFW (requires Red-opted-in token)");
            ui.checkbox(
                &mut cfg.include_video,
                "Include video clips in image harvest",
            );

            ui.add_space(8.0);
            if ui.checkbox(&mut cfg.dark_mode, "Dark mode").changed() {
                dark_changed = true;
            }

            ui.add_space(12.0);
            if ui
                .add(egui::Button::new("💾 Save & apply").min_size(egui::vec2(160.0, 30.0)))
                .clicked()
            {
                save = true;
            }
            ui.weak("Saving reopens the catalog and reloads the token.");
        });
    }

    if dark_changed {
        ui.ctx().set_visuals(if app.config.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
    }
    if save {
        app.config.save();
        let cfg: Config = app.config.clone();
        app.send(Cmd::Reconfigure(cfg));
        app.status = Some("settings saved".into());
    }
}
