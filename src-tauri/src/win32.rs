use std::cell::Cell;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::Foundation::GlobalFree;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_HDROP, DROPEFFECT_COPY};
use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop, DROPFILES};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, Once};
use std::thread;
use std::time::Instant;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, GetStockObject, InvalidateRect, SelectObject, SetBkMode, SetDCBrushColor,
    SetTextColor, SetWindowRgn, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DC_BRUSH, DEFAULT_CHARSET, DT_END_ELLIPSIS, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowW, MSGFLT_ALLOW, WM_COPYDATA, DispatchMessageW, GetClientRect, GetMessageW, KillTimer, PostMessageW, PostQuitMessage,
    RegisterClassW, SetLayeredWindowAttributes, SetTimer, ShowWindow, LWA_ALPHA, MSG, SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eConsole, eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, DEVMODEW, ENUM_CURRENT_SETTINGS, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetShellWindow, GetSystemMetrics, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, SM_CXSCREEN, SM_CYSCREEN,
};

const SHELL_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];
// Fullscreen browsers and video players are normal desktop use, and game capture can't hook browsers anyway.
const NOT_GAMES: [&str; 16] = [
    "explorer", "chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "arc", "vlc", "mpv",
    "applicationframehost", "searchhost", "lockapp", "shellexperiencehost", "startmenuexperiencehost", "textinputhost",
];

#[derive(Clone, Debug)]
pub struct FullscreenApp {
    /// The window handle as a number, so this stays Send.
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
    pub window_class: String,
    pub exe: String,
    pub monitor_left: i32,
    pub monitor_top: i32,
    /// True when this process can't inspect the app, which is what running below its privilege level looks like.
    pub blocked_by_elevation: bool,
}

fn to_hwnd(value: isize) -> HWND {
    HWND(value as *mut core::ffi::c_void)
}

fn exe_of(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        Path::new(&full).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Returns whether the token is elevated, or None when it can't be opened.
fn token_elevated(process: HANDLE) -> Option<bool> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        let _ = CloseHandle(token);
        result.ok()?;
        Some(elevation.TokenIsElevated != 0)
    }
}

/// Windows blocks normal programs from messaging an elevated one, so opening Afterclip from the Start menu
/// would do nothing. This lets through the one message that says "open the panel", on the one hidden window
/// that receives it. The message can only open the panel or editor, so it gives a sender nothing else.
pub fn accept_second_instance_message(identifier: &str) {
    let wide = |text: String| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };
    let (class, title) = (wide(format!("{identifier}-sic")), wide(format!("{identifier}-siw")));
    unsafe {
        let Ok(hwnd) = FindWindowW(PCWSTR(class.as_ptr()), PCWSTR(title.as_ptr())) else { return };
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut owner));
        if owner == GetCurrentProcessId() {
            let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
        }
    }
}

/// Drags a file out of the app, as dragging it from File Explorer would. Waits for the drop, so call it on
/// the main thread, which keeps windows running while it waits. Always offers a copy, so the clip stays put.
pub fn drag_file(path: &Path) -> Result<(), String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).map_err(|e| e.to_string())?;
        let data: IDataObject = item.BindToHandler(None, &BHID_DataObject).map_err(|e| e.to_string())?;
        SHDoDragDrop(None, &data, None, DROPEFFECT_COPY).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Puts a file on the clipboard, ready to paste into a chat or a folder. Plain clipboard data, so unlike
/// dragging it works from an elevated app to any other.
pub fn copy_file(path: &Path) -> Result<(), String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let header = std::mem::size_of::<DROPFILES>();
    let bytes = wide.len() * 2;
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, header + bytes).map_err(|e| e.to_string())?;
        let target = GlobalLock(memory) as *mut u8;
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err("Could not copy the file".into());
        }
        let description = DROPFILES { pFiles: header as u32, fWide: true.into(), ..Default::default() };
        std::ptr::write_unaligned(target as *mut DROPFILES, description);
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, target.add(header), bytes);
        let _ = GlobalUnlock(memory);
        if let Err(e) = OpenClipboard(None) {
            let _ = GlobalFree(Some(memory));
            return Err(format!("The clipboard is in use: {e}"));
        }
        let _ = EmptyClipboard();
        // Windows owns the memory once this succeeds.
        let stored = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(memory.0)));
        let _ = CloseClipboard();
        if let Err(e) = stored {
            let _ = GlobalFree(Some(memory));
            return Err(e.to_string());
        }
    }
    Ok(())
}

pub fn system_dpi() -> u32 {
    unsafe { GetDpiForSystem() }
}

pub fn is_self_elevated() -> bool {
    token_elevated(unsafe { GetCurrentProcess() }) == Some(true)
}

fn blocked_by_elevation(pid: u32) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true;
        };
        let elevated = token_elevated(handle);
        let _ = CloseHandle(handle);
        elevated != Some(false)
    }
}

/// The foreground window if it covers its whole monitor. That catches exclusive and borderless fullscreen.
pub fn foreground_fullscreen_app() -> Option<FullscreenApp> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() || hwnd == GetShellWindow() {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            return None;
        }
        let m = info.rcMonitor;
        if rect.left > m.left || rect.top > m.top || rect.right < m.right || rect.bottom < m.bottom {
            return None;
        }

        let mut buf = [0u16; 512];
        let n = GetClassNameW(hwnd, &mut buf);
        let window_class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if SHELL_CLASSES.contains(&window_class.as_str()) {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return None;
        }
        let exe = exe_of(pid);
        let stem = exe.to_lowercase();
        let stem = stem.strip_suffix(".exe").unwrap_or(&stem);
        if NOT_GAMES.contains(&stem) {
            return None;
        }
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);

        Some(FullscreenApp {
            hwnd: hwnd.0 as isize,
            pid,
            title,
            window_class,
            exe,
            monitor_left: m.left,
            monitor_top: m.top,
            blocked_by_elevation: !is_self_elevated() && blocked_by_elevation(pid),
        })
    }
}

#[derive(Clone, Debug)]
pub struct ActiveEndpoint {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Prepares the calling thread for the audio lookups below.
pub fn init_com_for_thread() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

unsafe fn take_pwstr(value: PWSTR) -> windows::core::Result<String> {
    let text = value.to_string().map_err(|_| windows::core::Error::empty());
    CoTaskMemFree(Some(value.0 as *const core::ffi::c_void));
    text
}

/// The output devices this process is playing sound through right now. Empty while it's silent or inactive.
pub fn active_endpoints_of(pid: u32) -> Vec<ActiveEndpoint> {
    unsafe fn look(pid: u32) -> windows::core::Result<Vec<ActiveEndpoint>> {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let default_id = take_pwstr(devices.GetDefaultAudioEndpoint(eRender, eConsole)?.GetId()?)?;
        let all = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut found = Vec::new();
        for i in 0..all.GetCount()? {
            let device = all.Item(i)?;
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let sessions = manager.GetSessionEnumerator()?;
            for k in 0..sessions.GetCount()? {
                let session = sessions.GetSession(k)?;
                let Ok(control) = session.cast::<IAudioSessionControl2>() else { continue };
                if control.GetProcessId().ok() != Some(pid) || session.GetState()? != AudioSessionStateActive {
                    continue;
                }
                let id = take_pwstr(device.GetId()?)?;
                let store = device.OpenPropertyStore(STGM_READ)?;
                let name = store.GetValue(&PKEY_Device_FriendlyName)?.to_string();
                found.push(ActiveEndpoint { is_default: id == default_id, id, name });
                break;
            }
        }
        Ok(found)
    }
    unsafe { look(pid) }.unwrap_or_default()
}

const RUN_KEY: PCWSTR = windows::core::w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = windows::core::w!("Afterclip");

/// Adds or removes Afterclip from the programs Windows starts when you sign in.
pub fn set_launch_on_startup(enabled: bool) -> Result<(), String> {
    unsafe {
        if !enabled {
            // Already being absent is the goal, not a failure.
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
        let result = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            REG_SZ.0,
            Some(command.as_ptr() as *const core::ffi::c_void),
            (command.len() * 2) as u32,
        );
        if result.is_ok() { Ok(()) } else { Err(format!("Could not set startup: {result:?}")) }
    }
}

const TOAST_CLASS: PCWSTR = windows::core::w!("AfterclipToast");
const TOAST_MS: u32 = 2600;
const TOAST_CLOSE_TIMER: usize = 1;
const TOAST_SHINE_TIMER: usize = 2;
const SHINE_FRAME_MS: u32 = 20;
/// Start and length of each sweep, in milliseconds. Nothing repaints between or after them, to stay light during a game.
const SHINE_SWEEPS: [(u128, u128); 2] = [(120, 850), (1400, 850)];
const SHINE_END_MS: u128 = 2250;
const TOAST_BACKGROUND: (f64, f64, f64) = (0x17 as f64, 0x14 as f64, 0x33 as f64);
const TOAST_SHINE: (f64, f64, f64) = (0xA9 as f64, 0x9D as f64, 0xFF as f64);
static TOAST_WINDOW: AtomicIsize = AtomicIsize::new(0);
static TOAST_TEXT: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static TOAST_CLASS_READY: Once = Once::new();
thread_local! {
    // Each toast runs on its own thread.
    static TOAST_SHOWN_AT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// How far through a sweep the shine is at this moment, from 0 to 1, eased at both ends.
fn shine_progress(elapsed_ms: u128) -> Option<f64> {
    SHINE_SWEEPS.iter().find(|(start, length)| elapsed_ms >= *start && elapsed_ms < start + length).map(|(start, length)| {
        let t = (elapsed_ms - start) as f64 / *length as f64;
        t * t * (3.0 - 2.0 * t)
    })
}

/// A soft diagonal band of light across the background, drawn in small blocks without allocating anything.
unsafe fn paint_shine(dc: HDC, area: &RECT, progress: f64, scale: i32) {
    let block = (2 * scale / 96).max(1);
    let (width, height) = (area.right as f64, area.bottom as f64);
    let band = 72.0 * scale as f64 / 96.0;
    let slant = 0.7;
    let centre = -band + (width + 2.0 * band + height * slant) * progress;
    let brush = HBRUSH(GetStockObject(DC_BRUSH).0);
    let mut y = 0;
    while y < area.bottom {
        let row_centre = centre - y as f64 * slant;
        let mut x = (((row_centre - band) as i32 / block) * block).max(0);
        while x < area.right && (x as f64) < row_centre + band {
            let distance = ((x as f64 - row_centre) / band).abs().min(1.0);
            let glow = (distance * std::f64::consts::FRAC_PI_2).cos().powi(2) * 0.6;
            let mix = |base: f64, light: f64| (base + (light - base) * glow) as u32;
            let (r, g, b) = (mix(TOAST_BACKGROUND.0, TOAST_SHINE.0), mix(TOAST_BACKGROUND.1, TOAST_SHINE.1), mix(TOAST_BACKGROUND.2, TOAST_SHINE.2));
            SetDCBrushColor(dc, COLORREF(r | g << 8 | b << 16));
            FillRect(dc, &RECT { left: x, top: y, right: x + block, bottom: y + block }, brush);
            x += block;
        }
        y += block;
    }
}

extern "system" fn toast_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let screen = BeginPaint(hwnd, &mut ps);
                let mut area = RECT::default();
                let _ = GetClientRect(hwnd, &mut area);
                let scale = GetDpiForWindow(hwnd) as i32;
                let px = |v: i32| v * scale / 96;
                // Drawn off screen and copied across in one go, so the sweep can't flicker.
                let dc = CreateCompatibleDC(Some(screen));
                let bitmap = CreateCompatibleBitmap(screen, area.right, area.bottom);
                let old_bitmap = SelectObject(dc, bitmap.into());
                let background = CreateSolidBrush(COLORREF(0x0033_1417));
                FillRect(dc, &area, background);
                let _ = DeleteObject(background.into());
                let old_shine_brush = SelectObject(dc, GetStockObject(DC_BRUSH));
                let elapsed = TOAST_SHOWN_AT.with(|t| t.get()).map_or(0, |at| at.elapsed().as_millis());
                if let Some(progress) = shine_progress(elapsed) {
                    paint_shine(dc, &area, progress, scale);
                }
                SelectObject(dc, old_shine_brush);
                // The brand dot, then the message. COLORREF is written blue-green-red.
                let dot = CreateSolidBrush(COLORREF(0x00FF_6C7C));
                let old_brush = SelectObject(dc, dot.into());
                let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                let middle = area.bottom / 2;
                let _ = Ellipse(dc, px(18), middle - px(6), px(30), middle + px(6));
                SelectObject(dc, old_pen);
                SelectObject(dc, old_brush);
                let _ = DeleteObject(dot.into());
                let font = CreateFontW(-px(16), 0, 0, 0, 600, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, windows::core::w!("Segoe UI"));
                let old_font = SelectObject(dc, font.into());
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0x00FC_EEF0));
                let mut text = TOAST_TEXT.lock().unwrap().clone();
                let mut box_area = RECT { left: px(42), top: 0, right: area.right - px(14), bottom: area.bottom };
                DrawTextW(dc, &mut text, &mut box_area, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
                SelectObject(dc, old_font);
                let _ = DeleteObject(font.into());
                let _ = BitBlt(screen, 0, 0, area.right, area.bottom, Some(dc), 0, 0, SRCCOPY);
                SelectObject(dc, old_bitmap);
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TOAST_SHINE_TIMER => {
                let elapsed = TOAST_SHOWN_AT.with(|t| t.get()).map_or(u128::MAX, |at| at.elapsed().as_millis());
                if elapsed >= SHINE_END_MS {
                    let _ = KillTimer(Some(hwnd), TOAST_SHINE_TIMER);
                }
                // One extra frame after each sweep clears the band away.
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_TIMER => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = TOAST_WINDOW.compare_exchange(hwnd.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Shows a small note in the top right of the primary monitor. It's a plain Windows window rather than a
/// web view, so it costs almost nothing during a game. It can't take focus or catch clicks.
pub fn show_toast(message: &str) {
    let text: Vec<u16> = message.encode_utf16().collect();
    thread::spawn(move || unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance = HINSTANCE(module.0);
        TOAST_CLASS_READY.call_once(|| {
            let class = WNDCLASSW { lpfnWndProc: Some(toast_proc), hInstance: instance, lpszClassName: TOAST_CLASS, ..Default::default() };
            RegisterClassW(&class);
        });
        // A new note replaces the last one rather than stacking.
        let previous = TOAST_WINDOW.swap(0, Ordering::SeqCst);
        if previous != 0 {
            let _ = PostMessageW(Some(to_hwnd(previous)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        *TOAST_TEXT.lock().unwrap() = text;

        let scale = GetDpiForSystem() as i32;
        let px = |v: i32| v * scale / 96;
        let (width, height, margin, corner) = (px(290), px(54), px(16), px(22));
        let (_, top, right, _) = work_area_at(0, 0);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            TOAST_CLASS,
            windows::core::w!("Afterclip"),
            WS_POPUP,
            right - width - margin,
            top + margin,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        ) else { return };
        TOAST_WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 245, LWA_ALPHA);
        SetWindowRgn(hwnd, Some(CreateRoundRectRgn(0, 0, width + 1, height + 1, corner, corner)), true);
        TOAST_SHOWN_AT.with(|t| t.set(Some(Instant::now())));
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TOAST_CLOSE_TIMER, TOAST_MS, None);
        SetTimer(Some(hwnd), TOAST_SHINE_TIMER, SHINE_FRAME_MS, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    });
}

pub fn window_still_open(hwnd: isize) -> bool {
    unsafe { IsWindow(Some(to_hwnd(hwnd))).as_bool() && !IsIconic(to_hwnd(hwnd)).as_bool() }
}

/// The usable area, without the taskbar, of the monitor containing this point. Returns left, top, right, bottom.
pub fn work_area_at(x: i32, y: i32) -> (i32, i32, i32, i32) {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let w = info.rcWork;
        (w.left, w.top, w.right, w.bottom)
    }
}

/// The primary monitor's width, height and refresh rate.
pub fn primary_display_mode() -> (u32, u32, u32) {
    unsafe {
        let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut mode).as_bool() {
            return (mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency);
        }
        let (w, h) = primary_monitor_size();
        (w as u32, h as u32, 60)
    }
}

pub fn primary_monitor_size() -> (i64, i64) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN) as i64, GetSystemMetrics(SM_CYSCREEN) as i64) }
}

// PlaySound reads this after returning, so it has to outlive the call.
static PLAYING: Mutex<Vec<u8>> = Mutex::new(Vec::new());

const TONE_RATE: u32 = 48000;

/// Each tone fades in and out so it can't click. Silence at both ends lets the audio device settle,
/// because opening a device mid-game is when a sound is most likely to break up.
fn tone_samples(frequencies: &[f64]) -> Vec<i16> {
    let ms = |v: f64| (TONE_RATE as f64 * v / 1000.0) as usize;
    let (lead, tone, attack, gap) = (ms(30.0), ms(100.0), ms(6.0), ms(10.0));
    let mut samples = vec![0i16; lead];
    for freq in frequencies {
        for i in 0..tone {
            let rise = (i as f64 / attack as f64).min(1.0);
            let rise = 0.5 - 0.5 * (std::f64::consts::PI * rise).cos();
            let fade = 1.0 - i as f64 / tone as f64;
            let wave = (2.0 * std::f64::consts::PI * freq * i as f64 / TONE_RATE as f64).sin();
            samples.push((wave * 0.15 * rise * fade * 32767.0) as i16);
        }
        samples.extend(std::iter::repeat_n(0i16, gap));
    }
    samples.extend(std::iter::repeat_n(0i16, lead));
    samples
}

/// Plays beeps without a window, since this is the only confirmation a player gets in a fullscreen game.
pub fn play_tones(frequencies: &[f64]) {
    const RATE: u32 = TONE_RATE;
    let samples = tone_samples(frequencies);
    let bytes = samples.len() * 2;
    let mut wav = Vec::with_capacity(44 + bytes);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + bytes as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(bytes as u32).to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    let mut playing = PLAYING.lock().unwrap();
    // Stop the current sound before replacing the buffer it may still be reading.
    unsafe {
        let _ = PlaySoundW(PCWSTR::null(), None, SND_ASYNC);
    }
    *playing = wav;
    unsafe {
        let _ = PlaySoundW(PCWSTR(playing.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_start_and_end_silent_without_clicks() {
        let samples = tone_samples(&[880.0, 1320.0]);
        assert_eq!(samples[0], 0);
        assert_eq!(*samples.last().unwrap(), 0);
        assert!(samples.iter().any(|s| s.abs() > 3000), "the tone should be audible");
        // A click is a jump far larger than the steepest slope of the highest tone at full volume.
        let steepest = 0.15 * 32767.0 * 2.0 * std::f64::consts::PI * 1320.0 / TONE_RATE as f64;
        let biggest = samples.windows(2).map(|w| (w[1] as f64 - w[0] as f64).abs()).fold(0.0, f64::max);
        assert!(biggest <= steepest * 1.1, "jump of {biggest}, allowed {steepest}");
    }

    #[test]
    fn shine_stops_repainting_between_and_after_sweeps() {
        assert_eq!(shine_progress(0), None);
        assert_eq!(shine_progress(120), Some(0.0));
        assert_eq!(shine_progress(1100), None);
        assert!(shine_progress(1800).is_some());
        assert_eq!(shine_progress(SHINE_END_MS), None);
    }
}
