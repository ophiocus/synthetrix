#![windows_subsystem = "windows"]

mod app;
mod civitai;
mod config;
mod convert;
mod db;
mod git_update;
mod pngmeta;
mod tabs;
mod wfgraph;
mod worker;

use eframe::egui;

// These constants are the single source of truth for app identity.
// The bootstrap script (scripts/new_app.ps1) rewrites them for a new app.
pub const APP_NAME: &str = "Synthetrix";
pub const APP_WINDOW_TITLE: &str = "Synthetrix";
// GitHub repo in "owner/repo" form — used by the update checker.
pub const APP_GH_REPO: &str = "ophiocus/synthetrix";

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title(APP_WINDOW_TITLE),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(|cc| Ok(Box::new(app::SynthetrixApp::new(cc)))),
    )
}
