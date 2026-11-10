//! Alles, was Windows-spezifisch ist: Desktop-Einbettung hinter den Symbolen, Monitore,
//! Tray-Icon, Autostart, Pause-Erkennung und die Nachrichtenschleife.
//!
//! Hinweis: Die Einbettung nutzt das (undokumentierte, aber seit Windows 8 stabile) WorkerW-Verfahren.
//! Ab Windows 11 24H2 ("raised desktop") liegen Symbole und Hintergrund anders – beides wird unterstützt.

use anyhow::{bail, Context, Result};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;
use windows::core::{w, Interface, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY, POWERBROADCAST_SETTING,
    SYSTEM_POWER_STATUS,
};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::System::Registry::*;
use windows::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows::Win32::System::Threading::{
    CreateMutexW, CreateWaitableTimerExW, OpenMutexW, SetWaitableTimer, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE,
    SYNCHRONIZATION_SYNCHRONIZE, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub use wgpu::rwh::{RawDisplayHandle, RawWindowHandle};

const WM_TRAY: u32 = WM_APP + 1;
const WTS_SESSION_LOCK: usize = 7;
const WTS_SESSION_UNLOCK: usize = 8;
const APP_NAME: PCWSTR = w!("Veyra");
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const PBT_POWERSETTINGCHANGE: usize = 0x8013;
/// Kommandozeilen-Schalter, mit dem Windows Veyra beim Anmelden startet.
pub const AUTOSTART_ARG: &str = "--autostart";
/// Kommandozeilen-Schalter für das Einstellungsfenster.
pub const SETTINGS_ARG: &str = "--settings";
pub const SETTINGS_TITLE: &str = "Veyra – Einstellungen";
/// Gemeinsame App-Kennung aller Veyra-Prozesse (Taskleiste, Startmenü, Anheften).
const APP_ID: &str = "w3ssy.Veyra";

/// Ereignisse aus den Fensterprozeduren an die App.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    TrayMenu,
    TrayActivate,
    /// Explorer wurde neu gestartet → Fenster und Tray-Icon neu anlegen.
    ShellRestarted,
    DisplayChanged,
    /// Hell/Dunkel-Modus der Taskleiste geändert → Tray-Icon tauschen.
    ThemeChanged,
    /// Bildschirm an (`true`) oder aus/gedimmt (`false`).
    DisplayPower(bool),
    Locked(bool),
    Quit,
}

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

fn push(e: Event) {
    EVENTS.with(|q| q.borrow_mut().push(e));
}

pub fn take_events() -> Vec<Event> {
    EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

// ───────────────────────────── Prozess ─────────────────────────────

pub fn init_process() {
    unsafe {
        // Echte Pixel statt hochskalierter Fenster auf 4K/HiDPI-Monitoren.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// `None`, wenn Veyra bereits läuft. Das Handle muss bis Programmende gehalten werden.
pub fn single_instance() -> Option<HANDLE> {
    named_instance(w!("Local\\Veyra.SingleInstance"))
}

/// Wie [`single_instance`], aber für das Einstellungsfenster.
pub fn settings_instance() -> Option<HANDLE> {
    named_instance(w!("Local\\Veyra.Settings"))
}

/// Läuft der Hintergrund-Prozess (Wallpaper)?
pub fn wallpaper_running() -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\Veyra.SingleInstance")) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

fn named_instance(name: PCWSTR) -> Option<HANDLE> {
    unsafe {
        let h = CreateMutexW(None, true, name).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(h);
            return None;
        }
        Some(h)
    }
}

pub fn error_box(text: &str) {
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), APP_NAME, MB_OK | MB_ICONERROR);
    }
}

/// Veyra (diese EXE) mit Argumenten als eigenen Prozess starten.
pub fn spawn_self(args: &[&str]) {
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).args(args).spawn()) {
        Ok(_) => {}
        Err(e) => log::error!("Veyra {args:?} starten: {e}"),
    }
}

/// Einstellungsfenster öffnen – oder ein bereits offenes nach vorne holen.
pub fn open_settings() {
    if !focus_settings() {
        spawn_self(&[SETTINGS_ARG]);
    }
}

/// Holt ein offenes Einstellungsfenster nach vorne. `false`, wenn keins offen ist.
pub fn focus_settings() -> bool {
    let Some(h) = settings_window() else { return false };
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
    true
}

pub fn settings_window() -> Option<HWND> {
    unsafe { FindWindowW(PCWSTR::null(), &HSTRING::from(SETTINGS_TITLE)).ok().filter(|h| !h.is_invalid()) }
}

// ───────────────────────────── Startmenü & Anheften ─────────────────────────────

/// Alle Veyra-Prozesse unter einer App-Kennung – sonst ordnet Windows Fenster, angeheftete Kacheln
/// und den Startmenü-Eintrag nicht einander zu.
pub fn set_app_id() {
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID));
    }
}

/// Beim Anheften des Einstellungsfensters merkt sich Windows sonst nur `veyra.exe` (ohne `--settings`) –
/// ein Klick auf die Kachel würde dann kein Fenster öffnen. Diese Fenster-Eigenschaften legen fest,
/// was die angeheftete Kachel startet.
pub fn set_relaunch_command(hwnd: HWND) -> Result<()> {
    use windows::Win32::Storage::EnhancedStorage::*;
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
    let exe = std::env::current_exe()?;
    let command = format!("\"{}\" {SETTINGS_ARG}", exe.display());
    let icon = format!("{},0", exe.display());
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd)?;
        // Die Kennung muss zuerst gesetzt sein, sonst ignoriert Windows den Neustart-Befehl.
        store.SetValue(&PKEY_AppUserModel_ID, &lpwstr(APP_ID))?;
        store.SetValue(&PKEY_AppUserModel_RelaunchCommand, &lpwstr(command.as_str()))?;
        store.SetValue(&PKEY_AppUserModel_RelaunchDisplayNameResource, &lpwstr("Veyra"))?;
        store.SetValue(&PKEY_AppUserModel_RelaunchIconResource, &lpwstr(icon.as_str()))?;
        store.Commit()?;
    }
    Ok(())
}

/// Shell-Eigenschaften (App-Kennung, Neustart-Befehl) müssen VT_LPWSTR sein – `PROPVARIANT::from(&str)`
/// erzeugt einen BSTR, den die Shell mit „Falscher Parameter“ ablehnt.
fn lpwstr(s: &str) -> windows::Win32::System::Com::StructuredStorage::PROPVARIANT {
    use windows::Win32::System::Variant::VT_LPWSTR;
    let mut pv = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::default();
    // SHStrDupW reserviert mit CoTaskMemAlloc – genau das gibt PropVariantClear beim Drop wieder frei.
    if let Ok(p) = unsafe { SHStrDupW(&HSTRING::from(s)) } {
        unsafe {
            (*pv.Anonymous.Anonymous).vt = VT_LPWSTR;
            (*pv.Anonymous.Anonymous).Anonymous.pwszVal = p;
        }
    }
    pv
}

fn start_menu_link() -> Option<std::path::PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(std::path::PathBuf::from(appdata).join("Microsoft\\Windows\\Start Menu\\Programs\\Veyra.lnk"))
}

/// Gibt es den Startmenü-Eintrag?
pub fn start_menu_entry() -> bool {
    start_menu_link().is_some_and(|p| p.is_file())
}

/// Startmenü-Eintrag „Veyra“ (öffnet die Einstellungen) anlegen bzw. entfernen. Von dort lässt sich Veyra
/// an Start oder an die Taskleiste anheften. Liegt nur im eigenen Benutzerprofil, keine Adminrechte.
pub fn set_start_menu_entry(on: bool) -> Result<()> {
    let link = start_menu_link().context("APPDATA nicht gesetzt")?;
    if !on {
        if link.is_file() {
            std::fs::remove_file(&link).context("Startmenü-Eintrag entfernen")?;
        }
        return Ok(());
    }
    write_shortcut(&link, &std::env::current_exe()?)
}

/// Verknüpfung `link` → `exe --settings` mit Veyras App-Kennung schreiben.
fn write_shortcut(link: &Path, exe: &Path) -> Result<()> {
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).context("CoCreateInstance")?;
        sl.SetPath(&HSTRING::from(exe.as_os_str())).context("SetPath")?;
        sl.SetArguments(&HSTRING::from(SETTINGS_ARG)).context("SetArguments")?;
        sl.SetDescription(w!("Veyra – Video-Hintergründe für den Desktop")).context("SetDescription")?;
        sl.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0).context("SetIconLocation")?;
        if let Some(dir) = exe.parent() {
            sl.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())).context("SetWorkingDirectory")?;
        }
        let store: IPropertyStore = sl.cast().context("cast")?;
        store.SetValue(&PKEY_AppUserModel_ID, &lpwstr(APP_ID)).context("SetValue")?;
        store.Commit().context("Commit")?;
        let file: IPersistFile = sl.cast().context("cast")?;
        file.Save(&HSTRING::from(link.as_os_str()), true).context("Startmenü-Eintrag speichern")?;
    }
    Ok(())
}

/// Beim Autostart: warten, bis Explorer Taskleiste und Desktop fertig aufgebaut hat (höchstens `max`).
/// Ohne das hängt sich Veyra direkt nach der Anmeldung u. U. an eine Desktop-Ebene,
/// die Explorer kurz darauf noch einmal umbaut.
pub fn wait_for_shell(max: Duration) {
    let start = std::time::Instant::now();
    let ready = || unsafe {
        FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).is_ok()
            && FindWindowW(w!("Progman"), PCWSTR::null())
                .is_ok_and(|p| FindWindowExW(Some(p), None, w!("SHELLDLL_DefView"), PCWSTR::null()).is_ok() || find_legacy_workerw().is_some())
    };
    while !ready() && start.elapsed() < max {
        std::thread::sleep(Duration::from_millis(500));
    }
    // Explorer räumt nach dem ersten Erscheinen noch kurz auf.
    std::thread::sleep(Duration::from_secs(3));
    log::info!("Desktop bereit nach {:.1} s", start.elapsed().as_secs_f32());
}

pub fn open_path(path: &Path) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Nach dem Beenden das normale Hintergrundbild neu zeichnen lassen.
pub fn refresh_desktop() {
    unsafe {
        let mut buf = [0u16; 260];
        if SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, Some(buf.as_mut_ptr().cast()), Default::default()).is_ok() {
            let _ = SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, Some(buf.as_mut_ptr().cast()), Default::default());
        }
    }
}

// ───────────────────────────── Nachrichtenschleife ─────────────────────────────

/// Alle anstehenden Nachrichten verarbeiten. `false` bei WM_QUIT.
pub fn pump() -> bool {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        true
    }
}

thread_local! {
    /// Hochauflösender Timer (Windows 10 1803+). Der normale Windows-Takt ist 15,6 ms: Ein Schlaf von
    /// 16 ms wird so oft zu 31 ms, und bei 60-fps-Videos fallen Bilder aus – außer, irgendein anderes
    /// Programm (Browser, VS Code …) hat den Takt gerade systemweit verfeinert.
    static TIMER: Option<HANDLE> = unsafe {
        CreateWaitableTimerExW(None, PCWSTR::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0).ok()
    };
}

/// Schlafen, bis die Zeit um ist oder eine Nachricht eintrifft (0 % CPU im Leerlauf).
/// Genau auf ~1 ms, ohne den Takt des ganzen Systems zu verändern (`timeBeginPeriod` wäre schlechter für den Akku).
pub fn wait(timeout: Duration) {
    unsafe {
        let precise = TIMER.with(|t| {
            let t = (*t)?;
            // Negativ = relativ, in 100-ns-Einheiten.
            let due = -(timeout.as_nanos().min(i64::MAX as u128 / 2) as i64 / 100).max(1);
            SetWaitableTimer(t, &due, 0, None, None, false).ok()?;
            Some(t)
        });
        match precise {
            Some(t) => {
                MsgWaitForMultipleObjectsEx(Some(&[t]), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
            None => {
                MsgWaitForMultipleObjectsEx(None, timeout.as_millis().min(u32::MAX as u128) as u32, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
        }
    }
}

// ───────────────────────────── Monitore ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
    pub name: String,
    /// Abstand des Arbeitsbereichs (ohne Taskleiste) zum Monitorrand: links, oben, rechts, unten.
    pub work_insets: [i32; 4],
}

impl Monitor {
    fn rect(&self) -> RECT {
        RECT { left: self.x, top: self.y, right: self.x + self.width, bottom: self.y + self.height }
    }
}

/// Alle Monitore, sortiert von links nach rechts (das ist die Nummerierung in der Konfiguration).
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<Monitor>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            let r = info.monitorInfo.rcMonitor;
            let w = info.monitorInfo.rcWork;
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            list.push(Monitor {
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                name: String::from_utf16_lossy(&info.szDevice[..len]),
                work_insets: [w.left - r.left, w.top - r.top, r.right - w.right, r.bottom - w.bottom],
            });
        }
        TRUE
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut list as *mut _ as isize));
    }
    list.sort_by_key(|m| (m.x, m.y));
    list
}

// ───────────────────────────── Desktop-Einbettung ─────────────────────────────

/// Das Fenster, unter dem unsere Wallpaper-Fenster hängen.
pub struct DesktopHost {
    parent: HWND,
    /// Nur beim "raised desktop" (24H2+): unser Fenster kommt direkt unter die Symbole.
    icons: Option<HWND>,
}

impl DesktopHost {
    pub fn find() -> Result<Self> {
        unsafe {
            let progman = FindWindowW(w!("Progman"), PCWSTR::null()).context("Progman (Desktop) nicht gefunden")?;
            // Explorer bitten, eine WorkerW-Ebene zwischen Symbolen und Hintergrund anzulegen.
            for (wp, lp) in [(0xD, 1), (0, 0)] {
                let mut res = 0usize;
                SendMessageTimeoutW(progman, 0x052C, WPARAM(wp), LPARAM(lp), SMTO_NORMAL, 1000, Some(&mut res));

                // Klassisch (Windows 10, Windows 11 bis 23H2): Symbole wandern in ein Top-Level-WorkerW,
                // das Hintergrund-WorkerW ist dessen nächstes Geschwister.
                if let Some(worker) = find_legacy_workerw() {
                    log::info!("Desktop-Einbettung: klassisches WorkerW");
                    return Ok(Self { parent: worker, icons: None });
                }
                // Raised desktop (24H2+): Symbole und WorkerW sind Kinder von Progman.
                if let Ok(icons) = FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), PCWSTR::null()) {
                    if FindWindowExW(Some(progman), None, w!("WorkerW"), PCWSTR::null()).is_ok() {
                        log::info!("Desktop-Einbettung: Progman (raised desktop, 24H2+)");
                        return Ok(Self { parent: progman, icons: Some(icons) });
                    }
                }
            }
            bail!("Desktop-Ebene hinter den Symbolen nicht gefunden")
        }
    }
}

/// Ergebnis der regelmäßigen Prüfung der Desktop-Einbettung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Ok,
    /// Explorer hat die Reihenfolge geändert, unsere Fenster wurden wieder unter die Symbole sortiert.
    Reordered,
    /// Elternfenster/Symbole weg oder ersetzt → alles neu aufbauen.
    Broken,
}

impl DesktopHost {
    /// Prüft, ob unsere Fenster noch richtig eingebettet sind (billig, alle 0,5 s).
    pub fn check(&self, windows: &[isize]) -> HostState {
        unsafe {
            if !IsWindow(Some(self.parent)).as_bool() {
                return HostState::Broken;
            }
            let ours: Vec<HWND> = windows.iter().map(|&h| HWND(h as *mut _)).collect();
            if ours.iter().any(|&h| !IsWindow(Some(h)).as_bool() || GetParent(h).ok() != Some(self.parent)) {
                return HostState::Broken;
            }
            let Some(icons) = self.icons else { return HostState::Ok };
            // Raised desktop: Symbole müssen noch dieselben sein …
            if FindWindowExW(Some(self.parent), None, w!("SHELLDLL_DefView"), PCWSTR::null()).ok() != Some(icons) {
                return HostState::Broken;
            }
            // … und zwischen Symbolen und unseren Fenstern darf kein WorkerW (statisches Hintergrundbild) liegen.
            let mut reordered = false;
            for &h in &ours {
                let mut cur = GetWindow(icons, GW_HWNDNEXT).ok();
                while let Some(c) = cur {
                    if c == h {
                        break;
                    }
                    if class_name(c) == "WorkerW" {
                        let _ = SetWindowPos(h, Some(icons), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                        reordered = true;
                        break;
                    }
                    cur = GetWindow(c, GW_HWNDNEXT).ok();
                }
            }
            if reordered {
                HostState::Reordered
            } else {
                HostState::Ok
            }
        }
    }
}

unsafe fn find_legacy_workerw() -> Option<HWND> {
    unsafe extern "system" fn cb(top: HWND, data: LPARAM) -> BOOL {
        // Nur wenn das Symbol-Fenster selbst ein WorkerW ist (nicht Progman) – sonst würde
        // auf 24H2 ein fremdes WorkerW gefunden.
        if class_name(top) == "WorkerW" && FindWindowExW(Some(top), None, w!("SHELLDLL_DefView"), PCWSTR::null()).is_ok() {
            if let Ok(worker) = FindWindowExW(None, Some(top), w!("WorkerW"), PCWSTR::null()) {
                *(data.0 as *mut Option<HWND>) = Some(worker);
                return FALSE;
            }
        }
        TRUE
    }
    let mut found: Option<HWND> = None;
    let _ = EnumWindows(Some(cb), LPARAM(&mut found as *mut _ as isize));
    found
}

pub struct WallpaperWindow {
    hwnd: HWND,
    pub width: u32,
    pub height: u32,
}

impl WallpaperWindow {
    pub fn create(host: &DesktopHost, mon: &Monitor) -> Result<Self> {
        unsafe {
            let hinst = GetModuleHandleW(None)?;
            register_class(w!("VeyraWallpaper"), Some(wallpaper_proc))?;

            // Bildschirmkoordinaten → Koordinaten relativ zum Elternfenster.
            let mut pts = [POINT { x: mon.x, y: mon.y }];
            MapWindowPoints(None, Some(host.parent), &mut pts);

            let create = |ex: WINDOW_EX_STYLE| {
                // Alten Fehlercode löschen, sonst meldet ein stilles Scheitern einen veralteten Fehler.
                SetLastError(WIN32_ERROR(0));
                CreateWindowExW(
                    ex,
                    w!("VeyraWallpaper"),
                    w!("Veyra"),
                    WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                    pts[0].x,
                    pts[0].y,
                    mon.width,
                    mon.height,
                    Some(host.parent),
                    None,
                    Some(hinst.into()),
                    None,
                )
            };
            let hwnd = if host.icons.is_some() {
                // Layered-Kindfenster brauchen das Windows-8+-Manifest (veyra.manifest, via build.rs).
                match create(WS_EX_LAYERED | WS_EX_NOACTIVATE) {
                    Ok(h) => h,
                    Err(e) => {
                        log::warn!("Layered-Fenster abgelehnt ({e}) – ist das Manifest eingebettet? Versuche ohne.");
                        create(WS_EX_NOACTIVATE).context("CreateWindowEx (auch ohne WS_EX_LAYERED)")?
                    }
                }
            } else {
                create(WS_EX_NOACTIVATE).context("CreateWindowEx")?
            };
            if let Some(icons) = host.icons {
                // Voll deckend, aber als Layered-Fenster, sonst zeichnet DWM es beim raised desktop nicht.
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                // Direkt unter die Desktop-Symbole einsortieren.
                let _ = SetWindowPos(hwnd, Some(icons), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            Ok(Self { hwnd, width: mon.width as u32, height: mon.height as u32 })
        }
    }

    pub fn raw_handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        let mut h = wgpu::rwh::Win32WindowHandle::new(std::num::NonZeroIsize::new(self.hwnd.0 as isize).expect("HWND"));
        // Vulkan braucht zusätzlich die Modul-Instanz (DX12 kommt ohne aus).
        h.hinstance = unsafe { GetModuleHandleW(None) }.ok().and_then(|m| std::num::NonZeroIsize::new(m.0 as isize));
        (RawDisplayHandle::Windows(wgpu::rwh::WindowsDisplayHandle::new()), RawWindowHandle::Win32(h))
    }

    pub fn id(&self) -> isize {
        self.hwnd.0 as isize
    }
}

impl Drop for WallpaperWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe fn register_class(name: PCWSTR, proc_: WNDPROC) -> Result<()> {
    let hinst = GetModuleHandleW(None)?;
    let wc = WNDCLASSW { lpfnWndProc: proc_, hInstance: hinst.into(), lpszClassName: name, ..Default::default() };
    if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
        bail!("Fensterklasse registrieren fehlgeschlagen");
    }
    Ok(())
}

unsafe extern "system" fn wallpaper_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1), // kein Flackern – wir zeichnen selbst
        WM_PAINT => {
            let _ = ValidateRect(Some(hwnd), None);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ───────────────────────────── Steuerfenster + Tray ─────────────────────────────

/// Unsichtbares Top-Level-Fenster: empfängt Tray-, Anzeige- und Sperr-Nachrichten.
pub struct ControlWindow {
    hwnd: HWND,
    power: Option<HPOWERNOTIFY>,
}

impl ControlWindow {
    pub fn new() -> Result<Self> {
        unsafe {
            register_class(w!("VeyraControl"), Some(control_proc))?;
            TASKBAR_CREATED.with(|c| c.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
            let hinst = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("VeyraControl"),
                w!("Veyra"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinst.into()),
                None,
            )?;
            let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
            // Bildschirm an/aus (meldet den aktuellen Zustand sofort einmal).
            let power = RegisterPowerSettingNotification(HANDLE(hwnd.0), &GUID_CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE).ok();
            Ok(Self { hwnd, power })
        }
    }

    pub fn id(&self) -> isize {
        self.hwnd.0 as isize
    }
}

impl Drop for ControlWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = WTSUnRegisterSessionNotification(self.hwnd);
            if let Some(p) = self.power.take() {
                let _ = UnregisterPowerSettingNotification(p);
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn control_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg != 0 && msg == TASKBAR_CREATED.with(|c| c.get()) {
        push(Event::ShellRestarted);
        return LRESULT(0);
    }
    match msg {
        WM_TRAY => {
            match (lp.0 as u32) & 0xFFFF {
                WM_LBUTTONDBLCLK => push(Event::TrayActivate),
                WM_RBUTTONUP | WM_CONTEXTMENU => push(Event::TrayMenu),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            push(Event::DisplayChanged);
            LRESULT(0)
        }
        WM_POWERBROADCAST if wp.0 == PBT_POWERSETTINGCHANGE && lp.0 != 0 => {
            let setting = &*(lp.0 as *const POWERBROADCAST_SETTING);
            if setting.PowerSetting == GUID_CONSOLE_DISPLAY_STATE && setting.DataLength >= 1 {
                // 0 = aus, 1 = an, 2 = gedimmt
                push(Event::DisplayPower(setting.Data[0] == 1));
            }
            LRESULT(1)
        }
        WM_SETTINGCHANGE => {
            let area = PCWSTR(lp.0 as *const u16);
            if !area.is_null() && area.to_string().is_ok_and(|a| a == "ImmersiveColorSet") {
                push(Event::ThemeChanged);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_WTSSESSION_CHANGE => {
            match wp.0 {
                WTS_SESSION_LOCK => push(Event::Locked(true)),
                WTS_SESSION_UNLOCK => push(Event::Locked(false)),
                _ => {}
            }
            LRESULT(0)
        }
        WM_ENDSESSION | WM_CLOSE => {
            push(Event::Quit);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

pub enum MenuItem<'a> {
    Item { id: u32, label: &'a str, checked: bool },
    Separator,
}

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
}

impl Tray {
    pub fn new(control: &ControlWindow) -> Self {
        let tray = Self { hwnd: control.hwnd, icon: load_tray_icon() };
        tray.add();
        tray
    }

    /// Icon neu laden (Theme-Wechsel, Explorer-Neustart mit anderer DPI).
    pub fn refresh_icon(&mut self) {
        let old = std::mem::replace(&mut self.icon, load_tray_icon());
        let mut nid = self.data();
        nid.uFlags = NIF_ICON;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            let _ = DestroyIcon(old);
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        let tip: Vec<u16> = "Veyra".encode_utf16().collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        nid
    }

    /// Auch nach Explorer-Neustart aufrufen.
    pub fn add(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.data());
        }
    }

    pub fn set_tooltip(&self, text: &str) {
        let mut nid = self.data();
        nid.uFlags = NIF_TIP;
        let tip: Vec<u16> = text.encode_utf16().take(nid.szTip.len() - 1).collect();
        nid.szTip = [0; 128];
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    /// Kontextmenü an der Mausposition zeigen; liefert die gewählte ID (0 = abgebrochen).
    pub fn show_menu(&self, items: &[MenuItem]) -> u32 {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return 0 };
            for it in items {
                match it {
                    MenuItem::Separator => {
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                    }
                    MenuItem::Item { id, label, checked } => {
                        let flags = if *checked { MF_STRING | MF_CHECKED } else { MF_STRING };
                        let _ = AppendMenuW(menu, flags, *id as usize, &HSTRING::from(*label));
                    }
                }
            }
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // Ohne SetForegroundWindow schließt sich das Menü beim Klick daneben nicht.
            let _ = SetForegroundWindow(self.hwnd);
            let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, self.hwnd, None);
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            id.0 as u32
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            let _ = DestroyIcon(self.icon);
        }
    }
}

/// Ressourcen-IDs aus `build.rs`.
const ICON_TRAY_LIGHT: u16 = 2;
const ICON_TRAY_DARK: u16 = 3;

/// Tray-Icon aus der EXE laden: weißes Symbol auf dunkler, dunkles Symbol auf heller Taskleiste.
/// Größe = kleines System-Icon (16 px bei 100 %, 24 px bei 150 % …); Windows wählt die passende Stufe.
fn load_tray_icon() -> HICON {
    let id = if taskbar_is_light() { ICON_TRAY_DARK } else { ICON_TRAY_LIGHT };
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON);
        let hinst = GetModuleHandleW(None).ok().map(|h| h.into());
        LoadImageW(hinst, PCWSTR(id as usize as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default())
    }
}

fn taskbar_is_light() -> bool {
    let mut value = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut len),
        )
        .is_ok()
            && value != 0
    }
}

// ───────────────────────────── Autostart ─────────────────────────────

pub fn set_autostart(on: bool) -> Result<()> {
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key).ok()?;
        let r = if on {
            let value: Vec<u16> = autostart_command()?.encode_utf16().chain([0]).collect();
            let bytes = std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2);
            RegSetValueExW(key, APP_NAME, None, REG_SZ, Some(bytes)).ok()
        } else {
            match RegDeleteValueW(key, APP_NAME) {
                e if e == ERROR_FILE_NOT_FOUND => Ok(()),
                e => e.ok(),
            }
        };
        let _ = RegCloseKey(key);
        r.context("Autostart-Eintrag setzen")
    }
}

fn autostart_command() -> Result<String> {
    Ok(format!("\"{}\" {AUTOSTART_ARG}", std::env::current_exe()?.display()))
}

/// Aktueller Registry-Eintrag (falls vorhanden).
fn autostart_value() -> Option<String> {
    unsafe {
        let mut buf = [0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len)).ok().ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

pub fn autostart_enabled() -> bool {
    autostart_value().is_some()
}

/// Eintrag vorhanden und zeigt auf diese EXE mit dem Autostart-Schalter (sonst neu schreiben,
/// z. B. nach dem Verschieben der EXE oder von älteren Versionen ohne Schalter).
pub fn autostart_current() -> bool {
    autostart_value().is_some_and(|v| autostart_command().is_ok_and(|c| v.eq_ignore_ascii_case(&c)))
}

// ───────────────────────────── Dateidialoge (Einstellungsfenster) ─────────────────────────────

/// Windows-Dialog „Öffnen“ für Videos bzw. (bei `folder`) für einen Ordner. `None` = abgebrochen.
pub fn pick_path(folder: bool, start: Option<&Path>) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        if folder {
            let _ = dlg.SetOptions(dlg.GetOptions().unwrap_or_default() | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
            let _ = dlg.SetTitle(w!("Ordner für die Video-Bibliothek wählen"));
        } else {
            let pattern = crate::config::VIDEO_EXTENSIONS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";");
            let (name, spec) = (HSTRING::from("Videos"), HSTRING::from(pattern));
            let (all_name, all_spec) = (HSTRING::from("Alle Dateien"), HSTRING::from("*.*"));
            let filters = [
                COMDLG_FILTERSPEC { pszName: PCWSTR(name.as_ptr()), pszSpec: PCWSTR(spec.as_ptr()) },
                COMDLG_FILTERSPEC { pszName: PCWSTR(all_name.as_ptr()), pszSpec: PCWSTR(all_spec.as_ptr()) },
            ];
            let _ = dlg.SetFileTypes(&filters);
            let _ = dlg.SetOptions(dlg.GetOptions().unwrap_or_default() | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
            let _ = dlg.SetTitle(w!("Video wählen"));
        }
        if let Some(dir) = start.filter(|d| d.is_dir()) {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None) {
                let _ = dlg.SetFolder(&item);
            }
        }
        let owner = GetForegroundWindow();
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
        s.map(std::path::PathBuf::from)
    }
}

// ───────────────────────────── Pause-Erkennung ─────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct SystemState {
    /// Netzstecker gezogen oder Energiesparmodus aktiv.
    pub on_battery: bool,
    /// Exklusives Vollbild (D3D) oder Präsentationsmodus.
    pub exclusive_fullscreen: bool,
    /// Monitor (Index in `monitors`), auf dem das Vordergrundfenster liegt …
    pub fg_monitor: Option<usize>,
    /// … und ob es diesen Monitor vollständig bedeckt bzw. maximiert ist.
    pub fg_fullscreen: bool,
    pub fg_maximized: bool,
}

pub fn system_state(monitors: &[Monitor], own: &[isize]) -> SystemState {
    let mut st = SystemState::default();
    unsafe {
        let mut ps = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut ps).is_ok() {
            st.on_battery = ps.ACLineStatus == 0 || ps.SystemStatusFlag == 1;
        }
        if let Ok(q) = SHQueryUserNotificationState() {
            st.exclusive_fullscreen = q == QUNS_RUNNING_D3D_FULL_SCREEN || q == QUNS_PRESENTATION_MODE;
        }

        let fg = GetForegroundWindow();
        if fg.is_invalid() || own.contains(&(fg.0 as isize)) || is_shell_window(fg) {
            return st;
        }
        let mut r = RECT::default();
        if DwmGetWindowAttribute(fg, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32).is_err() {
            let _ = GetWindowRect(fg, &mut r);
        }
        let hmon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return st;
        }
        let m = mi.rcMonitor;
        st.fg_monitor = monitors.iter().position(|x| x.rect() == m);
        st.fg_fullscreen = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
        st.fg_maximized = IsZoomed(fg).as_bool();
    }
    st
}

// ───────────────────────────── Zeit ─────────────────────────────

/// Ortszeit (Zeitzone und Sommerzeit von Windows) für einen Unix-Zeitpunkt in Sekunden.
pub fn local_time(unix: i64) -> crate::overlay::time::DateTime {
    use crate::overlay::time::DateTime;
    use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;
    let utc = DateTime::from_unix(unix);
    let st = SYSTEMTIME {
        wYear: utc.year as u16,
        wMonth: utc.month as u16,
        wDayOfWeek: utc.weekday as u16,
        wDay: utc.day as u16,
        wHour: utc.hour as u16,
        wMinute: utc.minute as u16,
        wSecond: utc.second as u16,
        wMilliseconds: 0,
    };
    let mut local = SYSTEMTIME::default();
    // SAFETY: beide Zeiger zeigen auf gültige Strukturen; None = aktuelle Zeitzone des Systems.
    if unsafe { SystemTimeToTzSpecificLocalTime(None, &st, &mut local) }.is_err() {
        return utc;
    }
    DateTime::new(local.wYear as i32, local.wMonth as u32, local.wDay as u32, local.wHour as u32, local.wMinute as u32, local.wSecond as u32)
}

// ───────────────────────────── Bilder ─────────────────────────────

/// JPEG oder PNG aus dem Speicher dekodieren (Windows Imaging Component, also wie bei den Videos ein
/// von Windows Update gepflegter Parser). Andere Formate werden abgelehnt, und es werden ausdrücklich die
/// eingebauten Decoder gewählt – nachinstallierte Codecs kommen nie zum Zug.
/// Liefert Breite, Höhe und RGBA mit vormultipliziertem Alpha, verkleinert auf höchstens `max_side`.
pub fn decode_image(bytes: &[u8], max_side: u32) -> Result<(u32, u32, Vec<u8>)> {
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

    let container = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        GUID_ContainerFormatJpeg
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        GUID_ContainerFormatPng
    } else {
        bail!("weder JPEG noch PNG");
    };
    // SAFETY: COM-Aufrufe mit gültigen Zeigern; `bytes` lebt länger als der Stream.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).context("WIC")?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let decoder = factory.CreateDecoder(&container, &GUID_VendorMicrosoftBuiltIn)?;
        decoder.Initialize(&stream, WICDecodeMetadataCacheOnDemand).context("Bild lesen")?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;
        if w == 0 || h == 0 || w > 16_384 || h > 16_384 {
            bail!("unplausible Bildgröße {w}x{h}");
        }
        let k = (max_side as f32 / w.max(h) as f32).min(1.0);
        let (tw, th) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
        let source: IWICBitmapSource = if (tw, th) != (w, h) {
            let scaler = factory.CreateBitmapScaler()?;
            scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant)?;
            scaler.cast()?
        } else {
            frame.cast()?
        };
        let conv = factory.CreateFormatConverter()?;
        conv.Initialize(&source, &GUID_WICPixelFormat32bppPRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let mut rgba = vec![0u8; (tw * th * 4) as usize];
        conv.CopyPixels(std::ptr::null(), tw * 4, &mut rgba).context("Pixel kopieren")?;
        Ok((tw, th, rgba))
    }
}

// ───────────────────────────── HTTPS ─────────────────────────────

/// Einfacher HTTPS-GET über WinHTTP (in Windows enthalten: Proxy-Einstellungen und Zertifikatsprüfung
/// des Systems, keine eigene TLS-Bibliothek). Liefert Statuscode und höchstens `max_bytes` Antwort.
pub fn https_get(host: &str, path: &str, max_bytes: usize) -> Result<(u16, Vec<u8>)> {
    use windows::Win32::Networking::WinHttp::*;

    /// Schließt ein WinHTTP-Handle beim Verlassen des Gültigkeitsbereichs.
    struct H(*mut core::ffi::c_void);
    impl Drop for H {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: Handle stammt von WinHTTP und wird genau einmal geschlossen.
                let _ = unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }
    let check = |h: *mut core::ffi::c_void, what: &str| -> Result<H> {
        if h.is_null() {
            Err(windows::core::Error::from_thread()).context(what.to_string())
        } else {
            Ok(H(h))
        }
    };

    let agent = HSTRING::from(format!("Veyra/{} (+https://github.com/w3ssy/veyra)", env!("CARGO_PKG_VERSION")));
    // SAFETY: alle Zeichenketten leben bis zum Ende der Funktion, Handles werden über `H` geschlossen.
    unsafe {
        let session = check(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0), "WinHttpOpen")?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 10_000, 20_000).context("WinHttpSetTimeouts")?;
        let connect = check(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0), "Verbindung")?;
        let request = check(
            WinHttpOpenRequest(connect.0, w!("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE),
            "Anfrage",
        )?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).with_context(|| format!("{host} nicht erreichbar"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).context("keine Antwort")?;

        let mut status = 0u32;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .context("Statuscode lesen")?;

        let mut body = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read).context("Antwort lesen")?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
            if body.len() > max_bytes {
                bail!("Antwort größer als {max_bytes} Bytes");
            }
        }
        Ok((status as u16, body))
    }
}

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

unsafe fn is_shell_window(hwnd: HWND) -> bool {
    matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "NotifyIconOverflowWindow")
}

#[cfg(test)]
mod tests {
    #[test]
    fn shortcut_is_written() {
        let dir = std::env::temp_dir().join(format!("veyra-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("Veyra.lnk");
        // SetPath verlangt eine vorhandene Datei – die Test-EXE selbst.
        let exe = std::env::current_exe().unwrap();
        super::write_shortcut(&link, &exe).unwrap();
        assert!(link.metadata().unwrap().len() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
