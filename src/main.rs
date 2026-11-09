//! Veyra – schlanke, sichere Video-Hintergründe.
//!
//! Kein Browser, keine Skripte, kein Netzwerk, keine Adminrechte.

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

fn main() {
    logger::init(&config::config_dir().join("veyra.log"));
    log::info!("Veyra {} startet", env!("CARGO_PKG_VERSION"));

    #[cfg(windows)]
    if let Err(e) = app::run() {
        log::error!("{e:?}");
        platform::sys::error_box(&format!("Veyra konnte nicht starten:\n\n{e:#}"));
        std::process::exit(1);
    }

    #[cfg(not(windows))]
    {
        eprintln!("Veyra unterstützt bisher nur Windows. Linux-Backend: siehe docs/ARCHITEKTUR.md");
        std::process::exit(1);
    }
}
