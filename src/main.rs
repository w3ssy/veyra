//! Veyra – schlanke, sichere Video-Hintergründe.
//!
//! Kein Browser, keine Skripte, kein Netzwerk, keine Adminrechte.
//!
//! Aufruf:
//! - `veyra.exe`              – Hintergrund-Prozess (Wallpaper + Tray)
//! - `veyra.exe --autostart`  – wie oben, wartet aber nach der Anmeldung, bis Explorer bereit ist
//! - `veyra.exe --settings`   – Einstellungsfenster (eigener Prozess, schreibt nur config.toml)

// Im Release-Build kein Konsolenfenster.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod audio;
mod config;
mod logger;
mod render;
mod video;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod settings;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |a: &str| args.iter().any(|x| x.eq_ignore_ascii_case(a));

    #[cfg(windows)]
    if has(platform::sys::SETTINGS_ARG) {
        logger::init(&config::config_dir().join("settings.log"));
        if let Err(e) = settings::run() {
            log::error!("Einstellungen: {e:?}");
            platform::sys::error_box(&format!("Das Einstellungsfenster konnte nicht geöffnet werden:\n\n{e:#}"));
        }
        return;
    }

    logger::init(&config::config_dir().join("veyra.log"));
    log::info!("Veyra {} startet", env!("CARGO_PKG_VERSION"));

    #[cfg(windows)]
    if let Err(e) = app::run(has(platform::sys::AUTOSTART_ARG)) {
        log::error!("{e:?}");
        platform::sys::error_box(&format!("Veyra konnte nicht starten:\n\n{e:#}"));
        std::process::exit(1);
    }

    #[cfg(not(windows))]
    {
        let _ = has;
        eprintln!("Veyra unterstützt bisher nur Windows. Linux-Backend: siehe docs/ARCHITEKTUR.md");
        std::process::exit(1);
    }
}
