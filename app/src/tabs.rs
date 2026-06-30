//! The three workflow tabs + settings. Rendering reads app state via shared
//! borrows and buffers any actions in RefCells, which are drained into worker
//! commands after the UI closures close (keeps the borrow checker happy).

use crate::app::{CoverState, SynthetrixApp};
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
    ui.painter().rect_filled(rect, egui::Rounding::same(4.0), egui::Color32::from_gray(38));
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
            ui.colored_label(egui::Color32::from_rgb(240, 120, 120), "✘ missing — set it in Settings");
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
                .add(egui::Button::new("⬇  Sync from CivitAI Red").min_size(egui::vec2(220.0, 34.0)))
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
        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
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
                .selected_text(if pu.sort_idx == 1 { "👍 Likes" } else { "⬇ Downloads" })
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut pu.sort_idx, 0, "⬇ Downloads").clicked() {
                        do_refresh = true;
                    }
                    if ui.selectable_value(&mut pu.sort_idx, 1, "👍 Likes").clicked() {
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
        if ui.add_enabled(en, egui::Button::new("⬇ Download selected")).clicked() {
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
                    frame = frame.stroke(egui::Stroke::new(1.5, egui::Color32::from_rgb(80, 200, 120)));
                } else if downloaded {
                    frame = frame.stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 150, 90)));
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
                                egui::Label::new(
                                    egui::RichText::new(&row.model_name).strong(),
                                )
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
            let _ = app.covers_pool.tx.send(CoverReq { model_id: mid, url: u });
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
            if ui.add_enabled(!app.busy, egui::Button::new("🩺 Audit")).clicked() {
                do_audit = true;
            }
            if app.audit.is_some() && ui.button("🛠 Heal").clicked() {
                do_heal = true;
            }
            // recover unmatched orphans by hash via CivitAI
            let has_orphans = app.audit.as_ref().map_or(false, |a| !a.orphans.is_empty());
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

    let active = app.manifest.iter().filter(|r| r.status == "promoted").count();
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
                        egui::ScrollArea::vertical().id_source("lb_params").show(&mut c[1], |ui| {
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
            ui.add(egui::TextEdit::singleline(&mut cfg.token).desired_width(440.0).password(true));

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
            ui.add(egui::Slider::new(&mut cfg.per_model, 1..=40).text("images per model on download"));
            ui.checkbox(&mut cfg.nsfw, "Include NSFW (requires Red-opted-in token)");
            ui.checkbox(&mut cfg.include_video, "Include video clips in image harvest");

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
