use crate::config::Config;
use crate::db;
use crate::git_update::UpdateState;
use crate::worker::{Cmd, CoverFetcher, Event, Worker};
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

/// Per-model cover thumbnail state (resolved against the on-disk cache).
pub enum CoverState {
    Requested,
    Ready(String),
    Missing,
}

/// Picker filter inputs (UI-side; sent to the worker as a db::PickFilter).
pub struct PickerUi {
    pub type_idx: usize, // 0 = All, else config.types[idx-1]
    pub base: String,
    pub search: String,
    pub only_downloaded: bool,
    pub min_downloads: String,
    pub limit: String,
    pub cover_px: f32,   // cover thumbnail height in the list
    pub sort_idx: usize, // 0 = downloads, 1 = likes
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
            cover_px: 220.0,
            sort_idx: 0,
        }
    }
}

/// Full-size image "silverbox" overlay. Opened by clicking a captured image
/// (image view) or its ⓘ button (info view). On open it loads the image's
/// workflow + A1111 params, and if exactly one side is present it synthesizes the
/// other via `convert` and caches it next to the image.
pub struct Lightbox {
    pub model_id: i64,
    pub image_path: String,
    pub wf_path: Option<String>,
    pub pr_path: Option<String>,
    pub wf_graph: Option<crate::wfgraph::WGraph>,
    pub wf_view: crate::wfgraph::WfView,
    pub params_text: String,
    pub show_info: bool,
    pub note: Option<String>,
}

impl Lightbox {
    pub fn open(
        model_id: i64,
        image_path: &str,
        wf: Option<String>,
        pr: Option<String>,
        show_info: bool,
    ) -> Self {
        use std::path::Path;
        let p = Path::new(image_path);
        let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("img")
            .to_string();
        let mut wf_path = wf;
        let mut pr_path = pr;
        let mut note = None;

        let wf_text0 = wf_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok());
        let pr_text0 = pr_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok());

        // synthesize + cache whichever side is missing
        let (wf_text, pr_text) = match (wf_text0, pr_text0) {
            (Some(w), None) => {
                let synth = crate::convert::workflow_to_params(&w);
                if let Some(ptxt) = &synth {
                    let dst = dir.join(format!("{stem}.params.txt"));
                    if std::fs::write(&dst, ptxt).is_ok() {
                        pr_path = Some(dst.to_string_lossy().into_owned());
                        note = Some("Synthesized A1111 params from the workflow (cached).".into());
                    }
                }
                (Some(w), synth)
            }
            (None, Some(p)) => {
                let synth = crate::convert::params_to_workflow(&p);
                if let Some(wtxt) = &synth {
                    let dst = dir.join(format!("{stem}.workflow.json"));
                    if std::fs::write(&dst, wtxt).is_ok() {
                        wf_path = Some(dst.to_string_lossy().into_owned());
                        note =
                            Some("Synthesized ComfyUI workflow from A1111 params (cached).".into());
                    }
                }
                (synth, Some(p))
            }
            (w, p) => (w, p),
        };

        let wf_graph = wf_text.as_deref().and_then(crate::wfgraph::parse);
        Self {
            model_id,
            image_path: image_path.to_string(),
            wf_path,
            pr_path,
            wf_graph,
            wf_view: crate::wfgraph::WfView::default(),
            params_text: pr_text.unwrap_or_default(),
            show_info,
            note,
        }
    }
}

pub struct SynthetrixApp {
    pub config: Config,
    pub worker: Worker,
    pub covers_pool: CoverFetcher,
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
    pub covers: std::collections::HashMap<i64, CoverState>,
    /// Model whose info/trigger-words overlay is open, + its text buffer.
    pub info_open: Option<i64>,
    pub info_text: String,
    /// Manifest rows expanded to show their captured-image strip, + path cache.
    /// Each entry: (image_path, optional workflow.json, optional params.txt).
    pub manifest_expanded: std::collections::HashSet<i64>,
    pub manifest_imgs:
        std::collections::HashMap<i64, Vec<(String, Option<String>, Option<String>)>>,
    /// Full-size image + workflow/params overlay (the "silverbox").
    pub lightbox: Option<Lightbox>,

    // Self-update plumbing (TinyBooth-style: periodic re-check + clean close).
    pub update_state: UpdateState,
    pub update_error: Option<String>,
    pub update_rx: Option<mpsc::Receiver<Option<crate::git_update::UpdateAvailable>>>,
    pub last_update_check: Option<std::time::Instant>,
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
        let covers_pool =
            CoverFetcher::spawn(&config, cc.egui_ctx.clone(), worker.evt_tx.clone(), 6);
        let _ = worker.tx.send(Cmd::QueryManifest);

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git_update::check_latest_release());
        });

        Self {
            config,
            worker,
            covers_pool,
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
            covers: std::collections::HashMap::new(),
            info_open: None,
            info_text: String::new(),
            manifest_expanded: std::collections::HashSet::new(),
            manifest_imgs: std::collections::HashMap::new(),
            lightbox: None,
            update_state: UpdateState::Checking,
            update_error: None,
            update_rx: Some(rx),
            last_update_check: Some(std::time::Instant::now()),
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
            sort: if self.picker_ui.sort_idx == 1 {
                "thumbs".into()
            } else {
                "downloads".into()
            },
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
                Event::CoverReady(id, path) => {
                    self.covers.insert(id, CoverState::Ready(path));
                }
                Event::CoverFailed(id) => {
                    self.covers.insert(id, CoverState::Missing);
                }
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

        // Periodic background re-check so a freshly-published release surfaces
        // mid-session, not only at startup. Wake the loop when idle so the timer
        // fires without requiring user interaction.
        if let Some(r) = crate::git_update::maybe_spawn_recheck(
            &self.update_state,
            &self.update_rx,
            self.last_update_check,
            false,
        ) {
            self.update_rx = Some(r);
            self.last_update_check = Some(std::time::Instant::now());
        }
        ctx.request_repaint_after(crate::git_update::RECHECK_INTERVAL);

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

        let mut should_close = false;
        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                should_close = crate::git_update::render(
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
        // Installer launched — close cleanly so Drop/config-save run before the
        // MSI swaps the running exe.
        if should_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Fetcher => crate::tabs::fetcher(self, ui),
            Tab::Picker => crate::tabs::picker(self, ui),
            Tab::Manifest => crate::tabs::manifest(self, ui),
            Tab::Settings => crate::tabs::settings(self, ui),
        });
    }
}
