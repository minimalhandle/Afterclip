use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use windows::core::w;
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::XboxController::{XInputGetState, XINPUT_STATE};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_DEVICEINFO, RID_DEVICE_INFO, RID_INPUT, RIM_TYPEHID,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, KillTimer, RegisterClassW, SetTimer, MSG, WINDOW_EX_STYLE, WM_INPUT, WM_TIMER, WNDCLASSW, WS_POPUP};

// Buttons by position, because the same spot has a different name on each controller.
const SELECT: u32 = 1 << 0; // View, Create or Minus.
const R3: u32 = 1 << 1;
const L3: u32 = 1 << 2;
const NORTH: u32 = 1 << 3;
const EAST: u32 = 1 << 4;
const SOUTH: u32 = 1 << 5;
const WEST: u32 = 1 << 6;
const UP: u32 = 1 << 7;
const RIGHT: u32 = 1 << 8;
const DOWN: u32 = 1 << 9;
const LEFT: u32 = 1 << 10;
const BUTTONS: usize = 11;

// The two buttons rarely land together, so either can go first. A button held for longer
// than this is being used in the game, and pressing Select then isn't a combination.
const ORDER_WINDOW_MS: u64 = 300;
// One press can arrive twice when Steam mirrors a controller as a virtual one.
const REPEAT_GUARD_MS: u64 = 500;
const SYNC_TIMER: usize = 1;
const SYNC_MS: u32 = 500;
const XINPUT_TIMER: usize = 2;
const XINPUT_MS: u32 = 16;
// Checking a slot with nothing in it is slow, so empty ones are only tried now and then.
const EMPTY_SLOT_EVERY_MS: u64 = 2000;

const SONY: u32 = 0x054C;
const DUALSENSE: [u32; 2] = [0x0CE6, 0x0DF2];
const NINTENDO: u32 = 0x057E;
const SWITCH_PRO: u32 = 0x2009;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Clip,
    Record,
    Mic,
    Marker,
    Screenshot,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    DualSense,
    SwitchPro,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::DualSense => "DualSense",
            Kind::SwitchPro => "Switch Pro Controller",
        }
    }
}

/// Which button goes with Select for each action. 0 is off.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bindings {
    pub clip: u32,
    pub record: u32,
    pub mic: u32,
    pub marker: u32,
    pub screenshot: u32,
}

/// The saved name of a button, as used in settings.
pub fn button(name: &str) -> u32 {
    match name {
        "r3" => R3,
        "north" => NORTH,
        "east" => EAST,
        "south" => SOUTH,
        "west" => WEST,
        "up" => UP,
        "right" => RIGHT,
        "down" => DOWN,
        "left" => LEFT,
        _ => 0,
    }
}

/// North, then clockwise. Anything else is released.
fn hat(value: u8) -> u32 {
    match value {
        0 => UP,
        1 => UP | RIGHT,
        2 => RIGHT,
        3 => DOWN | RIGHT,
        4 => DOWN,
        5 => DOWN | LEFT,
        6 => LEFT,
        7 => UP | LEFT,
        _ => 0,
    }
}

fn map_bits(value: u8, pairs: &[(u8, u32)]) -> u32 {
    pairs.iter().filter(|(bit, _)| value & bit != 0).fold(0, |all, (_, button)| all | button)
}

/// Over Bluetooth, the short report arrives padded to the size of the longest one, so only USB is
/// exactly 64 bytes. Reading a padded one as USB takes the trigger positions for buttons.
fn parse_dualsense(report: &[u8]) -> Option<u32> {
    let base = match (report.first()?, report.len()) {
        (0x01, 64) => 8,
        (0x01, 10..) => 5,
        (0x31, 12..) => 9,
        _ => return None,
    };
    let (face, rest) = (report[base], report[base + 1]);
    Some(hat(face & 0x0F) | map_bits(face, &[(0x10, WEST), (0x20, SOUTH), (0x40, EAST), (0x80, NORTH)]) | map_bits(rest, &[(0x10, SELECT), (0x40, L3), (0x80, R3)]))
}

/// 0x3F is what it sends by itself over Bluetooth, and 0x30 once something like Steam has asked for more.
fn parse_switch_pro(report: &[u8]) -> Option<u32> {
    match (report.first()?, report.len()) {
        (0x3F, 4..) => Some(
            map_bits(report[1], &[(0x01, SOUTH), (0x02, EAST), (0x04, WEST), (0x08, NORTH)])
                | map_bits(report[2], &[(0x01, SELECT), (0x04, L3), (0x08, R3)])
                | hat(report[3]),
        ),
        (0x30, 6..) => Some(
            map_bits(report[3], &[(0x01, WEST), (0x02, NORTH), (0x04, SOUTH), (0x08, EAST)])
                | map_bits(report[4], &[(0x01, SELECT), (0x04, R3), (0x08, L3)])
                | map_bits(report[5], &[(0x01, DOWN), (0x02, UP), (0x04, RIGHT), (0x08, LEFT)]),
        ),
        _ => None,
    }
}

fn parse_xinput(buttons: u16) -> u32 {
    let [low, high] = buttons.to_le_bytes();
    map_bits(low, &[(0x01, UP), (0x02, DOWN), (0x04, LEFT), (0x08, RIGHT), (0x20, SELECT), (0x40, L3), (0x80, R3)])
        | map_bits(high, &[(0x10, SOUTH), (0x20, EAST), (0x40, WEST), (0x80, NORTH)])
}

/// Follows one controller, and reports the buttons that were just completed with Select.
#[derive(Default)]
struct Chords {
    held: u32,
    pressed_at: [u64; BUTTONS],
}

impl Chords {
    fn update(&mut self, now: u32, at_ms: u64) -> u32 {
        let newly = now & !self.held;
        self.held = now;
        for bit in 0..BUTTONS {
            if newly & (1 << bit) != 0 {
                self.pressed_at[bit] = at_ms;
            }
        }
        if now & SELECT == 0 {
            return 0;
        }
        if newly & SELECT == 0 {
            return newly;
        }
        let recent = (0..BUTTONS).filter(|bit| at_ms.saturating_sub(self.pressed_at[*bit]) <= ORDER_WINDOW_MS);
        recent.fold(0, |all, bit| all | (1 << bit)) & now & !SELECT
    }
}

#[derive(Default)]
struct State {
    pads: HashMap<isize, Chords>,
    kinds: HashMap<isize, Option<Kind>>,
    fired_at: Vec<(u64, Action)>,
    seen: Vec<&'static str>,
    connected_slots: [bool; 4],
    empty_checked_at: u64,
}

/// Listens to controllers without ever sending them anything, so a game reading the same one isn't affected.
pub struct Controller {
    bindings: Box<dyn Fn() -> Option<Bindings> + Send + Sync>,
    fire: Box<dyn Fn(Action) + Send + Sync>,
    on_seen: Box<dyn Fn(&[&'static str]) + Send + Sync>,
    state: Mutex<State>,
    start: Instant,
}

static LISTENER: OnceLock<Arc<Controller>> = OnceLock::new();

impl Controller {
    /// `bindings` returns None while controllers are turned off, and nothing is read then.
    pub fn start(
        bindings: impl Fn() -> Option<Bindings> + Send + Sync + 'static,
        fire: impl Fn(Action) + Send + Sync + 'static,
        on_seen: impl Fn(&[&'static str]) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let me = Arc::new(Controller { bindings: Box::new(bindings), fire: Box::new(fire), on_seen: Box::new(on_seen), state: Mutex::default(), start: Instant::now() });
        let _ = LISTENER.set(Arc::clone(&me));
        std::thread::spawn(run_window);
        me
    }

    pub fn seen(&self) -> Vec<&'static str> {
        self.state.lock().unwrap().seen.clone()
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn note_seen(&self, state: &mut State, label: &'static str) {
        if !state.seen.contains(&label) {
            state.seen.push(label);
            (self.on_seen)(&state.seen);
        }
    }

    fn apply(&self, source: isize, pressed: u32) {
        let at = self.now_ms();
        let mut state = self.state.lock().unwrap();
        let completed = state.pads.entry(source).or_default().update(pressed, at);
        if completed == 0 {
            return;
        }
        let Some(bindings) = (self.bindings)() else { return };
        let actions = [(bindings.clip, Action::Clip), (bindings.record, Action::Record), (bindings.mic, Action::Mic), (bindings.marker, Action::Marker), (bindings.screenshot, Action::Screenshot)];
        state.fired_at.retain(|(when, _)| at.saturating_sub(*when) < REPEAT_GUARD_MS);
        for (bit, action) in actions {
            if bit != 0 && completed & bit != 0 && !state.fired_at.iter().any(|(_, a)| *a == action) {
                state.fired_at.push((at, action));
                (self.fire)(action);
            }
        }
    }

    fn feed_report(&self, source: isize, kind: Kind, report: &[u8]) {
        let parsed = match kind {
            Kind::DualSense => parse_dualsense(report),
            Kind::SwitchPro => parse_switch_pro(report),
        };
        if let Some(pressed) = parsed {
            self.note_seen(&mut self.state.lock().unwrap(), kind.label());
            self.apply(source, pressed);
        }
    }

    /// Lets the tests send what a real controller would.
    pub fn test_report(&self, kind: Kind, report: &[u8]) {
        self.feed_report(-1, kind, report);
    }

    fn poll_xinput(&self) {
        let at = self.now_ms();
        let check_empty = {
            let mut state = self.state.lock().unwrap();
            let due = at.saturating_sub(state.empty_checked_at) >= EMPTY_SLOT_EVERY_MS;
            if due {
                state.empty_checked_at = at;
            }
            due
        };
        for slot in 0..4u32 {
            if !check_empty && !self.state.lock().unwrap().connected_slots[slot as usize] {
                continue;
            }
            let mut pad = XINPUT_STATE::default();
            let connected = unsafe { XInputGetState(slot, &mut pad) } == ERROR_SUCCESS.0;
            let mut state = self.state.lock().unwrap();
            state.connected_slots[slot as usize] = connected;
            if connected {
                self.note_seen(&mut state, "Xbox controller");
                drop(state);
                self.apply(1000 + slot as isize, parse_xinput(pad.Gamepad.wButtons.0));
            }
        }
    }

    fn kind_of(&self, device: HANDLE) -> Option<Kind> {
        let mut state = self.state.lock().unwrap();
        *state.kinds.entry(device.0 as isize).or_insert_with(|| {
            let mut info = RID_DEVICE_INFO { cbSize: std::mem::size_of::<RID_DEVICE_INFO>() as u32, ..Default::default() };
            let mut size = info.cbSize;
            if unsafe { GetRawInputDeviceInfoW(Some(device), RIDI_DEVICEINFO, Some(&mut info as *mut _ as *mut _), &mut size) } == u32::MAX {
                return None;
            }
            let hid = unsafe { info.Anonymous.hid };
            match (hid.dwVendorId, hid.dwProductId) {
                (SONY, id) if DUALSENSE.contains(&id) => Some(Kind::DualSense),
                (NINTENDO, SWITCH_PRO) => Some(Kind::SwitchPro),
                _ => None,
            }
        })
    }

    fn on_input(&self, handle: isize, buffer: &mut Vec<u64>) {
        let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let mut size = 0u32;
        unsafe {
            GetRawInputData(HRAWINPUT(handle as _), RID_INPUT, None, &mut size, header);
            buffer.resize((size as usize).div_ceil(8), 0);
            if GetRawInputData(HRAWINPUT(handle as _), RID_INPUT, Some(buffer.as_mut_ptr() as *mut _), &mut size, header) == u32::MAX {
                return;
            }
            let raw = &*(buffer.as_ptr() as *const RAWINPUT);
            if raw.header.dwType != RIM_TYPEHID.0 {
                return;
            }
            let Some(kind) = self.kind_of(raw.header.hDevice) else { return };
            let hid = raw.data.hid;
            let report_size = (hid.dwSizeHid as usize).max(1);
            let reports = std::slice::from_raw_parts(std::ptr::addr_of!(raw.data.hid.bRawData) as *const u8, report_size * hid.dwCount as usize);
            for report in reports.chunks_exact(report_size) {
                self.feed_report(raw.header.hDevice.0 as isize, kind, report);
            }
        }
    }

    /// Reads controllers only while the setting is on, so otherwise this costs nothing.
    fn sync(&self, hwnd: HWND, listening: &mut bool) {
        let wanted = (self.bindings)().is_some();
        if wanted == *listening {
            return;
        }
        let (flags, target) = if wanted { (RIDEV_INPUTSINK, hwnd) } else { (RIDEV_REMOVE, HWND::default()) };
        let devices = [4u16, 5u16].map(|usage| RAWINPUTDEVICE { usUsagePage: 1, usUsage: usage, dwFlags: flags, hwndTarget: target });
        if unsafe { RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }.is_err() {
            return;
        }
        *listening = wanted;
        if wanted {
            unsafe { SetTimer(Some(hwnd), XINPUT_TIMER, XINPUT_MS, None) };
        } else {
            unsafe {
                let _ = KillTimer(Some(hwnd), XINPUT_TIMER);
            }
            let mut state = self.state.lock().unwrap();
            state.pads.clear();
            state.connected_slots = [false; 4];
        }
    }
}

std::thread_local! {
    static WINDOW_STATE: std::cell::RefCell<(bool, Vec<u64>)> = const { std::cell::RefCell::new((false, Vec::new())) };
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(controller) = LISTENER.get() {
        match (message, wparam.0) {
            (WM_INPUT, _) => WINDOW_STATE.with_borrow_mut(|(_, buffer)| controller.on_input(lparam.0, buffer)),
            (WM_TIMER, SYNC_TIMER) => WINDOW_STATE.with_borrow_mut(|(listening, _)| controller.sync(hwnd, listening)),
            (WM_TIMER, XINPUT_TIMER) => controller.poll_xinput(),
            _ => {}
        }
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

fn run_window() {
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = w!("AfterclipController");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() });
        // Never shown, so it can't take focus from a game.
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Afterclip controller"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None) else { return };
        SetTimer(Some(hwnd), SYNC_TIMER, SYNC_MS, None);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            DispatchMessageW(&message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(face: u8, rest: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x01;
        (r[8], r[9]) = (face, rest);
        r
    }

    #[test]
    fn reads_dualsense_over_usb() {
        assert_eq!(parse_dualsense(&usb(0x08, 0x90)), Some(SELECT | R3));
        assert_eq!(parse_dualsense(&usb(0x88, 0x00)), Some(NORTH));
        assert_eq!(parse_dualsense(&usb(0x04, 0x10)), Some(SELECT | DOWN));
    }

    #[test]
    fn padded_bluetooth_report_ignores_triggers() {
        let mut padded = vec![0u8; 78];
        padded[0] = 0x01;
        padded[5] = 0x08;
        // Half-pulled triggers, which a real controller sent, and the counter in the byte before them.
        (padded[7], padded[8], padded[9]) = (0xFC, 0x7F, 0xFF);
        assert_eq!(parse_dualsense(&padded), Some(0));
        padded[6] = 0x90;
        assert_eq!(parse_dualsense(&padded), Some(SELECT | R3));
    }

    #[test]
    fn reads_full_bluetooth_report() {
        let mut full = vec![0u8; 78];
        full[0] = 0x31;
        (full[9], full[10]) = (0x18, 0x10);
        assert_eq!(parse_dualsense(&full), Some(WEST | SELECT));
    }

    #[test]
    fn reads_switch_pro_by_position() {
        assert_eq!(parse_switch_pro(&[0x3F, 0x08, 0x09, 0x08]), Some(NORTH | SELECT | R3));
        assert_eq!(parse_switch_pro(&[0x3F, 0x00, 0x00, 0x04]), Some(DOWN));
        assert_eq!(parse_switch_pro(&[0x30, 0, 0, 0x08, 0x05, 0x02]), Some(EAST | SELECT | R3 | UP));
    }

    #[test]
    fn reads_xbox_by_position() {
        assert_eq!(parse_xinput(0x0020 | 0x0080), SELECT | R3);
        assert_eq!(parse_xinput(0x8000), NORTH);
        assert_eq!(parse_xinput(0x1000), SOUTH);
    }

    #[test]
    fn ignores_unknown_reports() {
        assert_eq!(parse_dualsense(&[0x05, 0, 0]), None);
        assert_eq!(parse_switch_pro(&[0x81, 1, 2, 3, 4, 5]), None);
        assert_eq!(parse_dualsense(&[]), None);
    }

    #[test]
    fn select_then_button_works_however_long_select_is_held() {
        let mut c = Chords::default();
        assert_eq!(c.update(SELECT, 0), 0);
        assert_eq!(c.update(SELECT | R3, 5000), R3);
        assert_eq!(c.update(SELECT | R3, 5010), 0, "holding fires once");
        assert_eq!(c.update(SELECT, 5100), 0);
        assert_eq!(c.update(SELECT | R3, 5200), R3);
    }

    #[test]
    fn either_order_works_close_together() {
        let mut c = Chords::default();
        assert_eq!(c.update(R3, 1000), 0);
        assert_eq!(c.update(R3 | SELECT, 1100), R3);
    }

    #[test]
    fn a_button_held_in_game_is_not_a_combination() {
        let mut c = Chords::default();
        assert_eq!(c.update(R3, 1000), 0);
        assert_eq!(c.update(R3 | SELECT, 3000), 0);
    }

    #[test]
    fn button_alone_never_fires() {
        let mut c = Chords::default();
        assert_eq!(c.update(R3, 0), 0);
        assert_eq!(c.update(0, 100), 0);
        assert_eq!(c.update(DOWN, 200), 0);
    }

    #[test]
    fn settings_names_map_to_buttons() {
        assert_eq!(button("r3"), R3);
        assert_eq!(button(""), 0);
        assert_eq!(button("nonsense"), 0);
    }
}
