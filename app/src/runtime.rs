//! Runtime tab — Synthetrix's control surface for the *managed* ComfyUI.
//!
//! The app already talks to a running ComfyUI (`comfy.rs`); this tab manages its
//! lifecycle by shelling out to `comfyctl.py`, which owns the venv, the
//! hardware-derived launch flags, and the detached-spawn semantics. We never
//! reimplement that here — we run it and render its verdict.
//!
//! Long operations (a cold `launch` warms up for up to ~240s) run on a worker
//! thread; the single result is drained back into `RuntimeUi` each frame.

use crate::app::SynthetrixApp;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// One preflight check, mirroring `synthetrix.comfy.checks.CheckResult`.
#[derive(serde::Deserialize, Clone)]
pub struct CheckResult {
    pub title: String,
    pub status: String, // OK | WARN | FAIL | SKIP
    pub message: String,
    #[serde(default)]
    pub fix: String,
    #[serde(default)]
    pub blocking: bool,
}

/// The `comfyctl doctor --json` report, mirroring `PreflightReport`.
#[derive(serde::Deserialize, Clone)]
pub struct PreflightReport {
    pub overall: String, // OK | WARN | FAIL | BLOCKED
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub results: Vec<CheckResult>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub elapsed_s: f64,
}

/// A worker-thread result. Exactly one is sent per spawned command.
pub enum RuntimeMsg {
    Report(Result<PreflightReport, String>),
    Text(String),
    Running(bool),
}

#[derive(Default)]
pub struct RuntimeUi {
    pub report: Option<PreflightReport>,
    pub console: String,
    pub running: Option<bool>,
    pub busy: bool,
    pub status_checked: bool,
    pub rx: Option<mpsc::Receiver<RuntimeMsg>>,
}

/// Locate the directory holding `comfyctl.py`: an explicit config path, else walk
/// up from the cwd and the exe dir (covers `cargo run` from `app/` and running
/// from the repo root). None => the tab prompts the user to set it.
fn find_manager(cfg_root: &str) -> Option<PathBuf> {
    let has = |d: &Path| d.join("comfyctl.py").is_file();
    let root = cfg_root.trim();
    if !root.is_empty() {
        let p = PathBuf::from(root);
        return has(&p).then_some(p);
    }
    let bases = [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf)),
    ];
    for base in bases.into_iter().flatten() {
        let mut cur = Some(base);
        while let Some(d) = cur {
            if has(&d) {
                return Some(d);
            }
            cur = d.parent().map(Path::to_path_buf);
        }
    }
    None
}

fn python_exe(app: &SynthetrixApp) -> String {
    let p = app.config.python_exe.trim();
    if p.is_empty() {
        "python".into()
    } else {
        p.to_string()
    }
}

/// Spawn `comfyctl <args>` on a thread. `json` => parse stdout as a PreflightReport.
fn spawn_cmd(app: &mut SynthetrixApp, args: &[&str], json: bool) {
    let Some(dir) = find_manager(&app.config.comfy_manager_root) else {
        app.runtime_ui.console =
            "comfyctl.py not found — set the manager path at the bottom of this tab.".into();
        return;
    };
    let py = python_exe(app);
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = mpsc::channel();
    app.runtime_ui.rx = Some(rx);
    app.runtime_ui.busy = true;
    std::thread::spawn(move || {
        let out = std::process::Command::new(&py)
            .arg("comfyctl.py")
            .args(&owned)
            .current_dir(&dir)
            .output();
        let msg = match out {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                if json {
                    match serde_json::from_str::<PreflightReport>(stdout.trim()) {
                        Ok(r) => RuntimeMsg::Report(Ok(r)),
                        Err(e) => RuntimeMsg::Report(Err(format!(
                            "couldn't parse comfyctl output: {e}\n{}",
                            if stderr.trim().is_empty() {
                                stdout
                            } else {
                                stderr
                            }
                        ))),
                    }
                } else {
                    let t = if stdout.trim().is_empty() {
                        stderr
                    } else {
                        stdout
                    };
                    RuntimeMsg::Text(t.trim().to_string())
                }
            }
            Err(e) => {
                let m = format!("failed to run `{py} comfyctl.py`: {e}");
                if json {
                    RuntimeMsg::Report(Err(m))
                } else {
                    RuntimeMsg::Text(m)
                }
            }
        };
        let _ = tx.send(msg);
    });
}

/// Quick, non-blocking up/down probe (Rust-native, no Python).
fn spawn_status(app: &mut SynthetrixApp) {
    let (tx, rx) = mpsc::channel();
    app.runtime_ui.rx = Some(rx);
    app.runtime_ui.busy = true;
    std::thread::spawn(move || {
        let _ = tx.send(RuntimeMsg::Running(crate::comfy::is_running()));
    });
}

fn status_color(status: &str) -> egui::Color32 {
    match status {
        "OK" => egui::Color32::from_rgb(120, 200, 140),
        "WARN" => egui::Color32::from_rgb(220, 190, 90),
        "FAIL" => egui::Color32::from_rgb(220, 110, 90),
        "BLOCKED" => egui::Color32::from_rgb(230, 90, 90),
        _ => egui::Color32::GRAY,
    }
}

fn glyph(status: &str) -> &'static str {
    match status {
        "OK" => "✔",
        "WARN" => "▲",
        "FAIL" => "✖",
        "SKIP" => "–",
        _ => "?",
    }
}

pub fn runtime(app: &mut SynthetrixApp, ui: &mut egui::Ui) {
    // Auto-probe once when the tab first opens.
    if !app.runtime_ui.status_checked && !app.runtime_ui.busy {
        app.runtime_ui.status_checked = true;
        spawn_status(app);
    }

    // Drain the worker result (one message per spawn).
    if let Some(rx) = app.runtime_ui.rx.take() {
        match rx.try_recv() {
            Ok(msg) => {
                app.runtime_ui.busy = false;
                match msg {
                    // Actions that may change the up/down state → re-probe next frame.
                    RuntimeMsg::Report(Ok(r)) => {
                        app.runtime_ui.report = Some(r);
                        app.runtime_ui.status_checked = false;
                    }
                    RuntimeMsg::Report(Err(e)) => app.runtime_ui.console = e,
                    RuntimeMsg::Text(t) => {
                        app.runtime_ui.console = t;
                        app.runtime_ui.status_checked = false;
                    }
                    // A plain status probe must NOT re-arm the auto-probe (would loop).
                    RuntimeMsg::Running(b) => app.runtime_ui.running = Some(b),
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                app.runtime_ui.rx = Some(rx);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(200));
            }
            Err(mpsc::TryRecvError::Disconnected) => app.runtime_ui.busy = false,
        }
    }

    let busy = app.runtime_ui.busy;

    ui.horizontal(|ui| {
        ui.heading("ComfyUI Runtime");
        ui.add_space(8.0);
        match app.runtime_ui.running {
            Some(true) => ui.colored_label(status_color("OK"), "● running :8188"),
            Some(false) => ui.colored_label(status_color("FAIL"), "● stopped"),
            None => ui.weak("● unknown"),
        };
        if busy {
            ui.add_space(6.0);
            ui.spinner();
        }
    });
    ui.label(
        egui::RichText::new(
            "Synthetrix manages ComfyUI's lifecycle here via comfyctl (venv, \
             hardware-tuned launch flags, model-path healing).",
        )
        .weak(),
    );
    ui.separator();

    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(!busy, egui::Button::new("↻ Status"))
            .on_hover_text("Probe :8188 for a running server")
            .clicked()
        {
            spawn_status(app);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("🩺 Doctor"))
            .on_hover_text("Full preflight checklist, read-only (never launches)")
            .clicked()
        {
            spawn_cmd(app, &["doctor", "--json"], true);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("✈ Preflight + launch"))
            .on_hover_text("Run the checklist and warm up ComfyUI if fundamentals pass")
            .clicked()
        {
            spawn_cmd(app, &["preflight", "--json"], true);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("▶ Launch"))
            .on_hover_text("Start ComfyUI on the managed venv (cold boot can take minutes)")
            .clicked()
        {
            spawn_cmd(app, &["launch"], false);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("■ Stop"))
            .on_hover_text("Stop the managed ComfyUI server")
            .clicked()
        {
            spawn_cmd(app, &["stop"], false);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("🔧 Heal paths"))
            .on_hover_text("Rewrite extra_model_paths.yaml to the vault/NVMe source of truth")
            .clicked()
        {
            spawn_cmd(app, &["heal", "--paths"], false);
        }
    });

    ui.add_space(6.0);

    // Report card.
    if let Some(rep) = app.runtime_ui.report.clone() {
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong("Preflight:");
            ui.colored_label(
                status_color(&rep.overall),
                egui::RichText::new(&rep.overall).strong(),
            );
            ui.weak(format!("({:.1}s)", rep.elapsed_s));
        });
        if rep.blocked && !rep.blockers.is_empty() {
            ui.colored_label(
                status_color("BLOCKED"),
                "Blocked — resolve these before ComfyUI-dependent features work:",
            );
            for b in &rep.blockers {
                ui.label(format!("   • {b}"));
            }
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for r in &rep.results {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(status_color(&r.status), glyph(&r.status));
                        let mut t = egui::RichText::new(&r.title).strong();
                        if r.blocking && r.status == "FAIL" {
                            t = t.color(status_color("BLOCKED"));
                        }
                        ui.label(t);
                        ui.weak(&r.message);
                    });
                    if !r.fix.is_empty() && (r.status == "FAIL" || r.status == "WARN") {
                        ui.label(
                            egui::RichText::new(format!("      fix: {}", r.fix))
                                .weak()
                                .italics(),
                        );
                    }
                }
            });
    }

    // Console output (launch/stop/heal text).
    if !app.runtime_ui.console.is_empty() {
        ui.separator();
        ui.strong("comfyctl");
        egui::ScrollArea::vertical()
            .id_source("runtime_console")
            .max_height(160.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&app.runtime_ui.console).monospace())
                        .wrap(),
                );
            });
    }

    // Provision: base program + post-install accessories.
    ui.separator();
    egui::CollapsingHeader::new("Provision (base program + accessories)")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Bring up or repair the runtime. Base = ComfyUI + venv + torch; \
                     accessories = ComfyUI-Manager + custom-node packs. Each step is \
                     idempotent; heavy (git clone + pip), so it runs in the background.",
                )
                .weak(),
            );
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!busy, egui::Button::new("⤓ Provision all"))
                    .on_hover_text("Base then accessories, in order, then heal paths")
                    .clicked()
                {
                    spawn_cmd(app, &["provision", "--all", "--apply"], false);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("👁 Dry-run all"))
                    .on_hover_text("Show the full plan without changing anything")
                    .clicked()
                {
                    spawn_cmd(app, &["provision", "--all"], false);
                }
                ui.separator();
                if ui
                    .add_enabled(!busy, egui::Button::new("🧩 ComfyUI-Manager"))
                    .on_hover_text("Install the in-UI node install/update accessory")
                    .clicked()
                {
                    spawn_cmd(app, &["provision", "--manager", "--apply"], false);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("🧩 Node packs"))
                    .on_hover_text("Clone the recommended custom-node packs that are missing")
                    .clicked()
                {
                    spawn_cmd(app, &["provision", "--nodes", "--apply"], false);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("🔩 Torch"))
                    .on_hover_text("Reinstall torch from the hardware-selected CUDA channel")
                    .clicked()
                {
                    spawn_cmd(app, &["provision", "--torch", "--apply"], false);
                }
            });
        });

    // Manager location config.
    ui.separator();
    egui::CollapsingHeader::new("Manager location")
        .default_open(find_manager(&app.config.comfy_manager_root).is_none())
        .show(ui, |ui| {
            match find_manager(&app.config.comfy_manager_root) {
                Some(d) => ui.weak(format!("comfyctl.py resolved at {}", d.display())),
                None => ui.colored_label(
                    status_color("FAIL"),
                    "comfyctl.py not found — set the Synthetrix repo path below.",
                ),
            };
            ui.horizontal(|ui| {
                ui.label("Manager root:");
                ui.text_edit_singleline(&mut app.config.comfy_manager_root);
                if ui.button("📁").on_hover_text("Pick folder").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        app.config.comfy_manager_root = p.to_string_lossy().into_owned();
                        app.config.save();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Python:");
                if ui
                    .text_edit_singleline(&mut app.config.python_exe)
                    .lost_focus()
                {
                    app.config.save();
                }
            });
            if ui.button("Save").clicked() {
                app.config.save();
            }
        });
}
