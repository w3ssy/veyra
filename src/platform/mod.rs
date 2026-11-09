//! Betriebssystem-Schicht. Die App spricht nur mit `sys::*` – für Linux kommt hier ein
//! zweites Modul mit derselben Schnittstelle hinzu (X11: _NET_WM_WINDOW_TYPE_DESKTOP,
//! Wayland: wlr-layer-shell Ebene "background").

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use self::windows as sys;
