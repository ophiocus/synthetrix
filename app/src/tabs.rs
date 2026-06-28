//! The three workflow tabs + settings. Rendering reads app state via shared
//! borrows and buffers any actions in RefCells, which are drained into worker
//! commands after the UI closures close (keeps the borrow checker happy).

use crate::app::SynthetrixApp;
use crate::config::Config;
use crate::worker::Cmd;
use eframe::egui;
use std::cell::RefCell;

fn gb(size_kb: f64) -> f64 {
    size_kb / 1_048_576.0
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
    let picks = &app.picks;
    let selected = &app.selected;
    let per = app.config.per_model;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for row in picks {
            ui.horizontal(|ui| {
                let mut sel = selected.contains(&row.file_id);
                if ui.checkbox(&mut sel, "").changed() {
                    sel_toggle.borrow_mut().push(row.file_id);
                }
                if let Some(url) = &row.cover_url {
                    ui.add(
                        egui::Image::new(url)
                            .fit_to_exact_size(egui::vec2(48.0, 48.0))
                            .maintain_aspect_ratio(true),
                    );
                }
                let (col, badge) = state_badge(&row.status, row.locked);
                ui.colored_label(col, badge);
                if row.nsfw {
                    ui.colored_label(egui::Color32::from_rgb(220, 100, 100), "🔞");
                }
                ui.label(egui::RichText::new(&row.model_name).strong())
                    .on_hover_text(format!("https://civitai.com/models/{}", row.model_id));
                ui.weak(format!(
                    "[{}·{}] {} dl ⭐{:.1} {:.2}GB",
                    row.model_type, row.base_model, row.downloads, row.rating, gb(row.size_kb)
                ));
                let tw = row.trained_words.trim_matches(|c| c == '[' || c == ']');
                if !tw.is_empty() && tw != "" {
                    ui.weak(format!("triggers: {}", tw.replace('"', "")));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if row.status == "indexed" {
                        if ui.small_button("Download").clicked() {
                            cmds.borrow_mut().push(Cmd::Download {
                                file_id: row.file_id,
                                promote: false,
                                images: per,
                            });
                        }
                    } else if ui.small_button("Hotload").clicked() {
                        cmds.borrow_mut().push(Cmd::Promote(row.file_id));
                    }
                });
            });
            ui.separator();
        }
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
    for id in sel_toggle.into_inner() {
        if !app.selected.remove(&id) {
            app.selected.insert(id);
        }
    }
    for c in cmds.into_inner() {
        app.send(c);
    }
}

// ---- Manifest --------------------------------------------------------------

pub fn manifest(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    let cmds: RefCell<Vec<Cmd>> = RefCell::new(Vec::new());
    let mut do_audit = false;
    let mut do_heal = false;
    let mut do_refresh = false;

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

    let rows = &app.manifest;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for r in rows {
            ui.horizontal(|ui| {
                let (col, badge) = state_badge(&r.status, r.locked);
                ui.colored_label(col, badge);
                ui.label(egui::RichText::new(&r.model_name).strong());
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
            ui.separator();
        }
    });

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
    for c in cmds.into_inner() {
        app.send(c);
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
