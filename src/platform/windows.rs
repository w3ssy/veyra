//! Alles, was Windows-spezifisch ist: Desktop-Einbettung hinter den Symbolen, Monitore,
//! Tray-Icon, Autostart, Pause-Erkennung und die Nachrichtenschleife.
//!
//! Hinweis: Die Einbettung nutzt das (undokumentierte, aber seit Windows 8 stabile) WorkerW-Verfahren.
//! Ab Windows 11 24H2 ("raised desktop") liegen Symbole und Hintergrund anders – beides wird unterstützt.

use anyhow::{bail, Context, Result};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;
use windows::core::{w, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::*;
use windows::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub use wgpu::rwh::{RawDisplayHandle, RawWindowHandle};

const WM_TRAY: u32 = WM_APP + 1;
const WTS_SESSION_LOCK: usize = 7;
const WTS_SESSION_UNLOCK: usize = 8;
const APP_NAME: PCWSTR = w!("Veyra");
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

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
    unsafe {
        let h = CreateMutexW(None, true, w!("Local\\Veyra.SingleInstance")).ok()?;
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

/// Schlafen, bis die Zeit um ist oder eine Nachricht eintrifft (0 % CPU im Leerlauf).
pub fn wait(timeout: Duration) {
    unsafe {
        MsgWaitForMultipleObjectsEx(None, timeout.as_millis().min(u32::MAX as u128) as u32, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
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
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            list.push(Monitor {
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                name: String::from_utf16_lossy(&info.szDevice[..len]),
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
        let h = wgpu::rwh::Win32WindowHandle::new(std::num::NonZeroIsize::new(self.hwnd.0 as isize).expect("HWND"));
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
            Ok(Self { hwnd })
        }
    }
}

impl Drop for ControlWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = WTSUnRegisterSessionNotification(self.hwnd);
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
            let exe = std::env::current_exe()?;
            let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
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

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME, RRF_RT_REG_SZ, None, None, None).is_ok() }
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

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

unsafe fn is_shell_window(hwnd: HWND) -> bool {
    matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "NotifyIconOverflowWindow")
}
