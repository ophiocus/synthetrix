//! Self-update via GitHub releases.
//!
//! Checks the latest release of `APP_GH_REPO`, compares 4-part semver against
//! `APP_VERSION` (set by build.rs from the git tag), and, if newer, downloads the
//! first `.msi` asset and launches it elevated through PowerShell.
//!
//! On a successful installer launch, [`render`] returns `true` so the caller can
//! close the eframe window cleanly (Drops run, config saves) instead of a raw
//! `process::exit` — mirrors the TinyBooth updater.

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Minimum gap between background re-checks of the GitHub releases endpoint.
/// 5 min matches the CI build/publish window — by the time it elapses, a tag
/// pushed when the app opened should have produced an MSI on `releases/latest`.
/// Without this the version label could stay stale for the whole session
/// (the check otherwise fires only once at startup).
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct UpdateAvailable {
    pub version: String,
    pub url: String,
}

pub enum UpdateState {
    Idle,
    Checking,
    Available(UpdateAvailable),
    Downloading(mpsc::Receiver<Result<PathBuf, String>>),
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32, u32) {
        let mut p = s.split('.');
        let a = p.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let b = p.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let c = p.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let d = p.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        (a, b, c, d)
    };
    parse(latest) > parse(current)
}

/// Fire a background `check_latest_release()` thread iff the updater is idle
/// (state Idle, no in-flight rx) and either `force_now` is set or the last check
/// is older than [`RECHECK_INTERVAL`] (or never ran). Caller sets
/// `last_check_at = Some(Instant::now())` when this returns `Some`.
pub fn maybe_spawn_recheck(
    state: &UpdateState,
    rx: &Option<mpsc::Receiver<Option<UpdateAvailable>>>,
    last_check_at: Option<Instant>,
    force_now: bool,
) -> Option<mpsc::Receiver<Option<UpdateAvailable>>> {
    if !matches!(state, UpdateState::Idle) || rx.is_some() {
        return None;
    }
    let should_run = force_now
        || match last_check_at {
            None => true,
            Some(t) => t.elapsed() >= RECHECK_INTERVAL,
        };
    if !should_run {
        return None;
    }
    let (tx, r) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(check_latest_release());
    });
    Some(r)
}

pub fn check_latest_release() -> Option<UpdateAvailable> {
    let ua = format!("{}/{}", crate::APP_NAME, env!("APP_VERSION"));
    let client = reqwest::blocking::Client::builder()
        .user_agent(ua)
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        crate::APP_GH_REPO
    );
    let resp: serde_json::Value = client.get(url).send().ok()?.json().ok()?;
    let tag = resp["tag_name"]
        .as_str()?
        .trim_start_matches('v')
        .to_string();
    if !is_newer(&tag, env!("APP_VERSION")) {
        return None;
    }
    let dl = resp["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str().unwrap_or("").ends_with(".msi"))?["browser_download_url"]
        .as_str()?
        .to_string();
    Some(UpdateAvailable {
        version: tag,
        url: dl,
    })
}

fn download_and_install(url: &str, version: &str) -> Result<PathBuf, String> {
    let ua = format!("{}/{}", crate::APP_NAME, env!("APP_VERSION"));
    let client = reqwest::blocking::Client::builder()
        .user_agent(ua)
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    let bytes = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| format!("Download: {e}"))?;

    let path = std::env::temp_dir().join(format!("{}-{version}.msi", crate::APP_NAME));
    std::fs::write(&path, &bytes).map_err(|e| format!("Write MSI: {e}"))?;

    let msi = path.to_string_lossy();
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Start-Process msiexec -ArgumentList '/i \"{msi}\" /passive /norestart' -Verb RunAs"
            ),
        ])
        .spawn()
        .map_err(|e| format!("Launch installer: {e}"))?;

    Ok(path)
}

/// Drive the version-label widget. Returns `true` exactly once, in the frame
/// where an installer launch has succeeded — the caller should respond by closing
/// the eframe window so Drop impls (config save) run cleanly.
#[must_use = "the bool means the app should close so Drop/config-save run before the installer swaps the exe"]
pub fn render(
    ui: &mut egui::Ui,
    state: &mut UpdateState,
    error: &mut Option<String>,
    rx: &mut Option<mpsc::Receiver<Option<UpdateAvailable>>>,
) -> bool {
    let mut should_close = false;

    // Drain background check result.
    if let Some(r) = rx.as_ref() {
        if let Ok(result) = r.try_recv() {
            *state = match result {
                Some(av) => UpdateState::Available(av),
                None => UpdateState::Idle,
            };
            *rx = None;
        }
    }
    // Drain download result. On Ok, signal a clean close; on Err, surface it.
    if let UpdateState::Downloading(r) = state {
        if let Ok(res) = r.try_recv() {
            match res {
                Ok(_) => should_close = true,
                Err(e) => {
                    *error = Some(format!("Update failed: {e}"));
                    *state = UpdateState::Idle;
                }
            }
        }
    }

    let label = format!("v{}", env!("APP_VERSION"));
    let response = ui
        .add(egui::Label::new(label).sense(egui::Sense::click()))
        .on_hover_text("Installed version. Click to re-check GitHub for a newer release.");

    // A click always forces a fresh round trip (even when an update is already
    // known); skip only while a check/download is mid-flight.
    let allow_recheck = !matches!(state, UpdateState::Checking | UpdateState::Downloading(_));
    if response.clicked() && allow_recheck {
        *state = UpdateState::Checking;
        let (tx, r) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(check_latest_release());
        });
        *rx = Some(r);
    }

    match state {
        UpdateState::Idle => {
            if let Some(e) = error.as_ref() {
                ui.colored_label(egui::Color32::LIGHT_RED, e);
            }
        }
        UpdateState::Checking => {
            ui.label("checking…");
        }
        UpdateState::Available(av) => {
            let msg = format!("v{} available — click to install", av.version);
            if ui.add(egui::Button::new(msg)).clicked() {
                let (tx, r) = mpsc::channel();
                let url = av.url.clone();
                let ver = av.version.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(download_and_install(&url, &ver));
                });
                *state = UpdateState::Downloading(r);
            }
        }
        UpdateState::Downloading(_) => {
            ui.label("downloading…");
        }
    }

    should_close
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn three_part_basic() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.1"));
    }

    #[test]
    fn four_part_subtag() {
        assert!(is_newer("0.1.0.1", "0.1.0"));
        assert!(is_newer("0.1.0.10", "0.1.0.9"));
        assert!(!is_newer("0.1.0.0", "0.1.0"));
    }

    #[test]
    fn malformed_and_empty_default_to_zero() {
        assert!(!is_newer("garbage", "0.0.1"));
        assert!(is_newer("0.0.1", "garbage"));
        assert!(!is_newer("", "0.0.1"));
        assert!(is_newer("0.0.1", ""));
    }
}
