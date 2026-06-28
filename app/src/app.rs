use crate::config::Config;
use crate::db;
use crate::git_update::UpdateState;
use crate::worker::{Cmd, Event, Worker};
use eframe::egui;
use std::collections::HashSet;
use std::sync::mpsc;

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Fetcher,
    Picker,
    Manifest,
    Settings,
}

/// Picker filter inputs (UI-side; sent to the worker as a db::PickFilter).
pub struct PickerUi {
    pub type_idx: usize, // 0 = All, else config.types[idx-1]
    pub base: String,
    pub search: String,
    pub only_downloaded: bool,
    pub min_downloads: String,
    pub limit: String,
}

impl Default for PickerUi {
    fn default() -> Self {
        Self {
            type_idx: 0,
            base: String::new(),
            search: String::new(),
            only_downloaded: false,
            min_downloads: String::new(),
            limit: "200".into(),
        }
    }
}

pub struct SynthetrixApp {
    pub config: Config,
    pub worker: Worker,
    pub tab: Tab,

    pub status: Option<String>,
    pub busy: bool,
    /// Sync progress snapshot: (combos_done, combos_total, rows, unique_models).
    pub sync: Option<(usize, usize, usize, usize)>,
    pub log: Vec<String>,

    pub picks: Vec<db::PickRow>,
    pub manifest: Vec<db::ManifestRow>,
    pub audit: Option<db::AuditReport>,
    pub selected: HashSet<i64>,
    pub picker_ui: PickerUi,

    // Self-update plumbing (inherited from the skeleton).
    pub update_state: UpdateState,
    pub update_error: Option<String>,
    pub update_rx: Option<mpsc::Receiver<Option<crate::git_update::UpdateAvailable>>>,
}

impl SynthetrixApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        cc.egui_ctx.set_visuals(if config.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        cc.egui_ctx.set_zoom_factor(config.zoom);
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let worker = Worker::spawn(config.clone(), cc.egui_ctx.clone());
        let _ = worker.tx.send(Cmd::QueryManifest);

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git_update::check_latest_release());
        });

        Self {
            config,
            worker,
            tab: Tab::Fetcher,
            status: None,
            busy: false,
            sync: None,
            log: Vec::new(),
            picks: Vec::new(),
            manifest: Vec::new(),
            audit: None,
            selected: HashSet::new(),
            picker_ui: PickerUi::default(),
            update_state: UpdateState::Checking,
            update_error: None,
            update_rx: Some(rx),
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.worker.tx.send(cmd);
    }

    /// Build a db::PickFilter from the current picker inputs.
    pub fn current_filter(&self) -> db::PickFilter {
        db::PickFilter {
            model_type: if self.picker_ui.type_idx == 0 {
                None
            } else {
                self.config.types.get(self.picker_ui.type_idx - 1).cloned()
            },
            base: non_empty(&self.picker_ui.base),
            search: non_empty(&self.picker_ui.search),
            status: if self.picker_ui.only_downloaded {
                Some("downloaded".into())
            } else {
                None
            },
            min_downloads: self.picker_ui.min_downloads.parse().unwrap_or(0),
            limit: self.picker_ui.limit.parse().unwrap_or(200),
        }
    }

    pub fn refresh_picks(&self) {
        self.send(Cmd::QueryPicks(self.current_filter()));
    }

    fn pump_events(&mut self) {
        while let Ok(ev) = self.worker.rx.try_recv() {
            match ev {
                Event::Status(s) => self.status = Some(s),
                Event::Busy(b) => self.busy = b,
                Event::Progress(d, t, k, u) => self.sync = Some((d, t, k, u)),
                Event::Log(s) => {
                    self.log.push(s);
                    if self.log.len() > 400 {
                        let cut = self.log.len() - 400;
                        self.log.drain(0..cut);
                    }
                }
                Event::Picks(v) => self.picks = v,
                Event::Manifest(v) => self.manifest = v,
                Event::Audit(r) => self.audit = Some(r),
                Event::Error(e) => {
                    let msg = format!("⚠ {e}");
                    self.log.push(msg.clone());
                    self.status = Some(msg);
                }
            }
        }
    }
}

pub fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

impl eframe::App for SynthetrixApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_events();

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.heading("Synthetrix");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Fetcher, "⬇ Fetcher");
                ui.selectable_value(&mut self.tab, Tab::Picker, "☑ Picker");
                ui.selectable_value(&mut self.tab, Tab::Manifest, "🗂 Manifest");
                ui.selectable_value(&mut self.tab, Tab::Settings, "⚙ Settings");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.busy {
                        ui.spinner();
                    }
                });
            });
        });

        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                crate::git_update::render(
                    ui,
                    &mut self.update_state,
                    &mut self.update_error,
                    &mut self.update_rx,
                );
                ui.separator();
                if let Some(s) = self.status.as_ref() {
                    ui.label(s);
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Fetcher => crate::tabs::fetcher(self, ui),
            Tab::Picker => crate::tabs::picker(self, ui),
            Tab::Manifest => crate::tabs::manifest(self, ui),
            Tab::Settings => crate::tabs::settings(self, ui),
        });
    }
}
