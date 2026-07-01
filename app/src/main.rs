#![windows_subsystem = "windows"]
// UI action buffers and DB-row constructors legitimately carry wide tuples /
// argument lists; allow those two style lints crate-wide so the strict
// `-D warnings` CI gate still polices everything else.
#![allow(clippy::type_complexity, clippy::too_many_arguments)]

mod app;
mod backends;
mod civitai;
mod comfy;
mod config;
mod convert;
mod db;
mod git_update;
mod lore;
mod pipelines;
mod pngmeta;
mod project;
mod release;
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
    harden_graphics_env();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title(APP_WINDOW_TITLE),
        // Render through wgpu → Vulkan (see harden_graphics_env): the OpenGL
        // path is hooked by OBS/NVIDIA capture layers that overflow the stack.
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(|cc| Ok(Box::new(app::SynthetrixApp::new(cc)))),
    )
}

/// Steer graphics init away from third-party capture hooks that crash us.
///
/// Two capture layers on this class of machine inject into the GPU init path and
/// overflow the stack (exception 0xC00000FD) before the first frame:
///  * OBS's Vulkan implicit layer `VK_LAYER_OBS_HOOK`
///    (`C:\ProgramData\obs-studio-hook\graphics-hook64.dll`), and
///  * the OBS/NVIDIA OpenGL game-capture hook (surfaces as `nvoglv64.dll`).
///
/// We can't uninstall those, but we can opt our own process out: disable the OBS
/// Vulkan layer via the manifest's `disable_environment` key, and pin wgpu to the
/// Vulkan backend (DX12 surface creation fails on this driver; GL is hooked).
/// Both are set only when unset, so a user can still override on the command line.
fn harden_graphics_env() {
    if std::env::var_os("DISABLE_VULKAN_OBS_CAPTURE").is_none() {
        std::env::set_var("DISABLE_VULKAN_OBS_CAPTURE", "1");
    }
    if std::env::var_os("WGPU_BACKEND").is_none() {
        std::env::set_var("WGPU_BACKEND", "vulkan");
    }
}
