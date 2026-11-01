// Shows whether a DualSense can be read while another program has focus. It only listens: it never
// sends anything to the controller, never presses a key and doesn't touch Afterclip or any game.
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;
use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDI_DEVICEINFO, RID_DEVICE_INFO, RID_INPUT, RIM_TYPEHID,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, RegisterClassW, SetTimer, MSG, WINDOW_EX_STYLE, WM_INPUT, WM_TIMER, WNDCLASSW, WS_POPUP,
};

const SONY: u32 = 0x054C;
const DUALSENSE: [u32; 2] = [0x0CE6, 0x0DF2];
const REPORT_EVERY_MS: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
struct Pad {
    /// North, then clockwise. Anything else is released.
    dpad: u8,
    /// Face, shoulder, stick and system buttons, in the order of NAMES.
    buttons: u32,
}

const NAMES: [&str; 15] = ["Square", "Cross", "Circle", "Triangle", "L1", "R1", "L2", "R2", "Create", "Options", "L3", "R3", "PS", "Touchpad", "Mute"];
const CREATE: u32 = 1 << 8;

/// Over Bluetooth, the short report arrives padded to the size of the longest one, so only USB is exactly 64 bytes.
fn is_usb(report: &[u8]) -> bool {
    report.first() == Some(&0x01) && report.len() == 64
}

/// Bluetooth starts with a short report, and sends 0x31 once something has asked for more.
fn parse(report: &[u8]) -> Option<Pad> {
    // The short report keeps a counter in the upper bits of its system byte, right beside PS and Touchpad.
    let (base, system) = match (report.first()?, report.len()) {
        (0x01, 64) => (8, 0x07),
        (0x01, 10..) => (5, 0x03),
        (0x31, 12..) => (9, 0x07),
        _ => return None,
    };
    let (a, b, c) = (report[base] as u32, report[base + 1] as u32, report[base + 2] as u32);
    Some(Pad { dpad: (a & 0x0F) as u8, buttons: (a >> 4) | (b << 4) | ((c & system) << 12) })
}

/// What the suggested button combination would do. Only a direction pressed while Create is held counts.
fn action(before: Pad, now: Pad) -> Option<&'static str> {
    if now.buttons & CREATE == 0 || now.dpad == before.dpad {
        return None;
    }
    match now.dpad {
        4 => Some("Save a clip"),
        0 => Some("Start or stop recording"),
        6 => Some("Mute or unmute the mic"),
        2 => Some("Add a marker"),
        _ => None,
    }
}

fn describe(pad: Pad) -> String {
    let mut held: Vec<&str> = NAMES.iter().enumerate().filter(|(i, _)| pad.buttons & (1 << i) != 0).map(|(_, n)| *n).collect();
    let directions = ["D-pad Up", "D-pad Up+Right", "D-pad Right", "D-pad Down+Right", "D-pad Down", "D-pad Down+Left", "D-pad Left", "D-pad Up+Left"];
    if let Some(direction) = directions.get(pad.dpad as usize) {
        held.push(direction);
    }
    if held.is_empty() { "nothing held".into() } else { held.join(" + ") }
}

struct Probe {
    log: Option<File>,
    last: Pad,
    start: Instant,
    reports: u32,
    window_reports: u32,
    cpu_at: u64,
    report_size: usize,
}

static PROBE: Mutex<Option<Probe>> = Mutex::new(None);

fn say(probe: &mut Probe, text: &str) {
    let line = format!("[{:>7.1}s] {text}", probe.start.elapsed().as_secs_f64());
    println!("{line}");
    if let Some(log) = &mut probe.log {
        let _ = writeln!(log, "{line}");
    }
}

fn focused_program() -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
        let Ok(handle): Result<HANDLE, _> = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return "unknown".into() };
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return "unknown".into();
        }
        String::from_utf16_lossy(&buf[..size as usize]).rsplit('\\').next().unwrap_or("unknown").to_string()
    }
}

fn cpu_ticks() -> u64 {
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let as_ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user);
    }
    as_ticks(kernel) + as_ticks(user)
}

fn is_dualsense(device: HANDLE) -> bool {
    let mut info = RID_DEVICE_INFO { cbSize: std::mem::size_of::<RID_DEVICE_INFO>() as u32, ..Default::default() };
    let mut size = info.cbSize;
    let read = unsafe { GetRawInputDeviceInfoW(Some(device), RIDI_DEVICEINFO, Some(&mut info as *mut _ as *mut _), &mut size) };
    if read == u32::MAX {
        return false;
    }
    let hid = unsafe { info.Anonymous.hid };
    hid.dwVendorId == SONY && DUALSENSE.contains(&hid.dwProductId)
}

fn on_input(handle: isize) {
    let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    unsafe {
        GetRawInputData(HRAWINPUT(handle as _), RID_INPUT, None, &mut size, header);
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        if GetRawInputData(HRAWINPUT(handle as _), RID_INPUT, Some(buf.as_mut_ptr() as *mut _), &mut size, header) == u32::MAX {
            return;
        }
        let raw = &*(buf.as_ptr() as *const RAWINPUT);
        if raw.header.dwType != RIM_TYPEHID.0 || !is_dualsense(raw.header.hDevice) {
            return;
        }
        let hid = raw.data.hid;
        let report_size = hid.dwSizeHid as usize;
        let reports = std::slice::from_raw_parts(std::ptr::addr_of!(raw.data.hid.bRawData) as *const u8, report_size * hid.dwCount as usize);
        let mut guard = PROBE.lock().unwrap();
        let probe = guard.as_mut().unwrap();
        for report in reports.chunks_exact(report_size.max(1)) {
            probe.reports += 1;
            probe.window_reports += 1;
            if probe.report_size != report.len() {
                probe.report_size = report.len();
                say(probe, &format!("DualSense found. Report 0x{:02X}, {} bytes, {}.", report[0], report.len(), if is_usb(report) { "USB" } else { "Bluetooth" }));
            }
            let Some(now) = parse(report) else { continue };
            if now != probe.last {
                let before = probe.last;
                probe.last = now;
                let focus = focused_program();
                say(probe, &format!("{}   (focus: {focus})", describe(now)));
                if let Some(would) = action(before, now) {
                    say(probe, &format!(">>> WOULD: {would}"));
                }
            }
        }
    }
}

fn on_timer() {
    let mut guard = PROBE.lock().unwrap();
    let probe = guard.as_mut().unwrap();
    let ticks = cpu_ticks();
    // A tick is 100 ns, and this is a share of one CPU core.
    let core = (ticks - probe.cpu_at) as f64 / (REPORT_EVERY_MS as f64 * 10_000.0) * 100.0;
    probe.cpu_at = ticks;
    let rate = probe.window_reports as f64 / (REPORT_EVERY_MS as f64 / 1000.0);
    probe.window_reports = 0;
    let (total, focus) = (probe.reports, focused_program());
    say(probe, &format!("Still listening. {rate:.0} reports a second, {total} in total, {core:.2}% of one CPU core, focus: {focus}"));
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_INPUT => on_input(lparam.0),
        WM_TIMER => on_timer(),
        _ => {}
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

fn main() {
    let path = std::env::current_exe().unwrap().with_file_name("controller-probe.log");
    let mut probe = Probe { log: File::create(&path).ok(), last: Pad { dpad: 8, buttons: 0 }, start: Instant::now(), reports: 0, window_reports: 0, cpu_at: cpu_ticks(), report_size: 0 };
    say(&mut probe, "Afterclip controller test. Read-only: nothing is sent to the controller.");
    say(&mut probe, &format!("Log file: {}", path.display()));
    say(&mut probe, "Press buttons on your DualSense. Hold Create, then press the D-pad. Close this window to stop.");
    *PROBE.lock().unwrap() = Some(probe);

    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let class = w!("AfterclipControllerProbe");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() });
        // Never shown, so it can't take focus from a game.
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Afterclip controller test"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None).unwrap();
        // INPUTSINK is what keeps reports coming while another program has focus.
        let wanted = [4u16, 5u16].map(|usage| RAWINPUTDEVICE { usUsagePage: 1, usUsage: usage, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd });
        RegisterRawInputDevices(&wanted, std::mem::size_of::<RAWINPUTDEVICE>() as u32).unwrap();
        SetTimer(Some(hwnd), 1, REPORT_EVERY_MS, None);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            DispatchMessageW(&message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(dpad_face: u8, shoulders: u8, system: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x01;
        (r[8], r[9], r[10]) = (dpad_face, shoulders, system);
        r
    }

    #[test]
    fn reads_usb_buttons() {
        let pad = parse(&usb(0x28, 0x00, 0x00)).unwrap();
        assert_eq!(describe(pad), "Cross");
        assert_eq!(describe(parse(&usb(0x04, 0x10, 0x00)).unwrap()), "Create + D-pad Down");
        assert_eq!(describe(parse(&usb(0x08, 0x00, 0x07)).unwrap()), "PS + Touchpad + Mute");
    }

    #[test]
    fn reads_both_bluetooth_reports() {
        let mut short = vec![0u8; 10];
        short[0] = 0x01;
        (short[5], short[6]) = (0x16, 0x10);
        assert_eq!(describe(parse(&short).unwrap()), "Square + Create + D-pad Left");
        let mut full = vec![0u8; 78];
        full[0] = 0x31;
        (full[9], full[10]) = (0x88, 0x20);
        assert_eq!(describe(parse(&full).unwrap()), "Triangle + Options");
    }

    #[test]
    fn padded_bluetooth_report_ignores_triggers_and_its_counter() {
        let mut padded = vec![0u8; 78];
        padded[0] = 0x01;
        padded[5] = 0x08;
        assert!(!is_usb(&padded));
        // A half-pulled trigger, as seen in a real Bluetooth log, and the counter in the system byte.
        (padded[7], padded[8], padded[9]) = (0xFC, 0x7F, 0xFF);
        assert_eq!(describe(parse(&padded).unwrap()), "nothing held");
        padded[6] = 0x90;
        assert_eq!(describe(parse(&padded).unwrap()), "Create + R3");
    }

    #[test]
    fn ignores_other_reports() {
        assert_eq!(parse(&[0x05, 0, 0]), None);
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn fires_once_and_only_with_create_held() {
        let idle = Pad { dpad: 8, buttons: CREATE };
        let down = Pad { dpad: 4, buttons: CREATE };
        assert_eq!(action(idle, down), Some("Save a clip"));
        assert_eq!(action(down, down), None);
        assert_eq!(action(Pad { dpad: 8, buttons: 0 }, Pad { dpad: 4, buttons: 0 }), None);
        assert_eq!(action(idle, Pad { dpad: 0, buttons: CREATE }), Some("Start or stop recording"));
        assert_eq!(action(idle, Pad { dpad: 6, buttons: CREATE }), Some("Mute or unmute the mic"));
        assert_eq!(action(idle, Pad { dpad: 2, buttons: CREATE }), Some("Add a marker"));
    }
}
