//! The panel, editor and share windows, and the tray: creating, placing, showing and recovering them.

use super::*;

/// Reloads a window that never reported rendering, then shows it regardless, and logs both for diagnosis.
pub(crate) fn watch_startup(core: &Shared, label: &'static str) {
    core.ready_windows.lock().unwrap().retain(|l| l != label);
    let core = Arc::clone(core);
    thread::spawn(move || {
        let is_ready = |core: &Core| core.ready_windows.lock().unwrap().iter().any(|l| l == label);
        thread::sleep(READY_RELOAD_AFTER);
        if is_ready(&core) {
            return;
        }
        core.log_ui(&format!("{label} had not rendered after {READY_RELOAD_AFTER:?}, reloading"));
        if let Some(window) = core.app.get_webview_window(label) {
            let _ = window.eval("window.location.reload()");
        }
        thread::sleep(READY_SHOW_AFTER - READY_RELOAD_AFTER);
        if is_ready(&core) {
            core.log_ui(&format!("{label} rendered after reloading"));
            return;
        }
        core.log_ui(&format!("{label} still had not rendered after {READY_SHOW_AFTER:?}, showing it anyway"));
        if let Some(window) = core.app.get_webview_window(label) {
            let _ = window.show();
        }
    });
}

pub(crate) fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// Where everything except clips and recordings lives.
pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("AFTERCLIP_HOME").filter(|_| api::is_test_instance()).map(PathBuf::from).unwrap_or_else(|| local_app_data().join("Afterclip"))
}

/// Earlier versions kept settings in Roaming and the windows' data in a folder of its own.
pub(crate) fn move_old_data(home: &Path, identifier: &str) {
    if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
        data_move::move_files(&roaming.join("Afterclip"), home, &["settings.json", "library.json", "api-token.txt"]);
    }
    let old_windows = local_app_data().join(identifier);
    thread::spawn(move || {
        let _ = fs::remove_dir_all(old_windows.join("EBWebView"));
        let _ = fs::remove_dir(old_windows);
    });
}

pub(crate) fn build_window<'a>(app: &'a AppHandle, label: &str, page: &str) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    let mut builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title("Afterclip")
        // Hidden until the page reports it has rendered, on a dark background rather than white.
        .visible(false)
        .background_color(BACKGROUND)
        .data_directory(home_dir().join("webview"));
    if api::is_test_instance() {
        if let Ok(port) = std::env::var("AFTERCLIP_DEBUG_PORT") {
            builder = builder.additional_browser_args(&format!("--remote-debugging-port={port}"));
        }
    }
    builder
}

/// Docks the panel flush into the corner of the usable area beside the tray, touching the taskbar.
pub(crate) fn position_panel(core: &Core, window: &WebviewWindow) {
    let anchor = core.tray_anchor.lock().unwrap().or_else(|| {
        let tray = core.app.tray_by_id("afterclip")?;
        let rect = tray.rect().ok()??;
        let position = rect.position.to_physical::<f64>(1.0);
        let size = rect.size.to_physical::<f64>(1.0);
        Some(((position.x + size.width / 2.0) as i32, (position.y + size.height / 2.0) as i32))
    });
    let (ax, ay) = anchor.unwrap_or_else(|| {
        let (w, h) = win32::primary_monitor_size();
        (w as i32, h as i32)
    });
    let (left, top, right, bottom) = win32::work_area_at(ax, ay);
    let Ok(size) = window.outer_size() else { return };
    let (w, h) = (size.width as i32, size.height as i32);
    // Works for a taskbar on any edge: the corner nearest the tray icon.
    let x = if ax > (left + right) / 2 { right - w } else { left };
    let y = if ay > (top + bottom) / 2 { bottom - h } else { top };
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Overlays like RivaTuner can crash the browser behind a window. The window stays, showing nothing, and
/// would never paint again. Removing it means the next open builds a new one. A visible panel comes straight
/// back, but not repeatedly, so something that crashes every browser can't start a loop.
// The panel is rebuilt after up to this many browser crashes in this long. A fresh browser can crash again a few
// seconds after the first, so once isn't enough, but a browser that keeps crashing isn't restarted forever.
const PANEL_RECOVERIES: usize = 3;
const PANEL_RECOVER_WINDOW: Duration = Duration::from_secs(120);
const PANEL_RECOVER_DELAY: Duration = Duration::from_secs(1);

/// Whether the panel may be rebuilt after a crash now, remembering it if so.
fn may_recover(recent: &mut Vec<Instant>, now: Instant) -> bool {
    recent.retain(|at| now.saturating_duration_since(*at) < PANEL_RECOVER_WINDOW);
    let allowed = recent.len() < PANEL_RECOVERIES;
    if allowed {
        recent.push(now);
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_comes_back_after_a_few_crashes_but_not_endlessly() {
        let start = Instant::now();
        let mut recent = Vec::new();
        assert!(may_recover(&mut recent, start));
        // A second crash seconds later, as a fresh browser sometimes has.
        assert!(may_recover(&mut recent, start + Duration::from_secs(5)));
        assert!(may_recover(&mut recent, start + Duration::from_secs(10)));
        assert!(!may_recover(&mut recent, start + Duration::from_secs(15)), "a browser that keeps crashing stays down");
        assert!(may_recover(&mut recent, start + PANEL_RECOVER_WINDOW + Duration::from_secs(1)), "and comes back once things settle");
    }
}

pub(crate) fn recover_from_browser_crash(core: &Shared, window: &WebviewWindow) {
    let label = window.label().to_string();
    let core = Arc::clone(core);
    let _ = window.with_webview(move |webview| unsafe {
        let Ok(browser) = webview.controller().CoreWebView2() else { return };
        let handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            // How it ended, for the log: the exit code and WebView2's reason, where it says.
            let mut detail = String::new();
            if let Some(args) = args {
                args.ProcessFailedKind(&mut kind)?;
                use windows::core::Interface;
                if let Ok(more) = args.cast::<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2ProcessFailedEventArgs2>() {
                    let (mut code, mut reason) = (0i32, Default::default());
                    let _ = more.ExitCode(&mut code);
                    let _ = more.Reason(&mut reason);
                    detail = format!(", exit code {code:#x}, reason {}", reason.0);
                }
            }
            if [COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED, COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED].contains(&kind) {
                let core = Arc::clone(&core);
                let label = label.clone();
                let which = if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED { "browser" } else { "page" };
                // Never from inside the browser's own event.
                thread::spawn(move || {
                    let Some(window) = core.app.get_webview_window(&label) else { return };
                    core.log_ui(&format!("{label}'s browser stopped ({which}{detail}), so its window will be rebuilt"));
                    let reopen = label == PANEL && window.is_visible().unwrap_or(false);
                    let _ = window.destroy();
                    if reopen && may_recover(&mut core.panel_recoveries.lock().unwrap(), Instant::now()) {
                        // A browser started at once after a crash sometimes crashes too, so the old one gets a
                        // moment to finish exiting first.
                        thread::sleep(PANEL_RECOVER_DELAY);
                        show_panel(&core);
                    }
                });
            }
            Ok(())
        }));
        let mut token = Default::default();
        let _ = browser.add_ProcessFailed(&handler, &mut token);
    });
}

/// The panel is the app's only front door. It's opened from the tray and closes when it loses focus.
pub(crate) fn show_panel(core: &Shared) {
    if let Some(window) = core.app.get_webview_window(PANEL) {
        position_panel(core, &window);
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("panel-shown", ());
        return;
    }
    let built = build_window(&core.app, PANEL, "index.html")
        .inner_size(420.0, 700.0)
        .resizable(false)
        .decorations(false)
        .skip_taskbar(true)
        .always_on_top(true)
        // A shadow adds an invisible frame around the window, which would leave a gap from the taskbar.
        .shadow(false)
        // See-through outside the page's rounded corners.
        .transparent(true)
        .background_color(Color(0, 0, 0, 0))
        .build();
    match built {
        Ok(window) => recover_from_browser_crash(core, &window),
        Err(e) => {
            core.log_ui(&format!("panel could not open: {e}"));
            return;
        }
    }
    watch_startup(core, PANEL);
}

pub(crate) fn toggle_panel(core: &Shared) {
    let visible = core.app.get_webview_window(PANEL).is_some_and(|w| w.is_visible().unwrap_or(false));
    if visible {
        if let Some(window) = core.app.get_webview_window(PANEL) {
            let _ = window.hide();
        }
        return;
    }
    // Clicking the tray takes focus from the panel first, which already hid it. This click means close.
    let just_hidden = core.panel_hidden_at.lock().unwrap().is_some_and(|at| at.elapsed() < Duration::from_millis(300));
    if !just_hidden {
        show_panel(core);
    }
}

pub(crate) fn show_editor(core: &Shared, files: Vec<String>) {
    core.pending_editor_files.lock().unwrap().extend(files);
    if let Some(window) = core.app.get_webview_window(EDITOR) {
        let _ = window.emit("editor-files", ());
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let built = build_window(&core.app, EDITOR, "editor.html")
        .title("Afterclip Editor")
        .inner_size(1120.0, 740.0)
        .min_inner_size(860.0, 600.0)
        .build();
    match built {
        Ok(window) => recover_from_browser_crash(core, &window),
        Err(e) => {
            core.log_ui(&format!("editor could not open: {e}"));
            return;
        }
    }
    watch_startup(core, EDITOR);
}

pub(crate) fn on_panel_blur(core: &Shared) {
    if core.keep_panel_open.load(Ordering::SeqCst) {
        return;
    }
    let Some(window) = core.app.get_webview_window(PANEL) else { return };
    let _ = window.hide();
    *core.panel_hidden_at.lock().unwrap() = Some(Instant::now());
    let hide = core.panel_hide_count.fetch_add(1, Ordering::SeqCst) + 1;
    let core = Arc::clone(core);
    thread::spawn(move || {
        thread::sleep(PANEL_KEEP_ALIVE);
        let still_hidden = core.panel_hide_count.load(Ordering::SeqCst) == hide;
        if let Some(window) = core.app.get_webview_window(PANEL) {
            if still_hidden && !window.is_visible().unwrap_or(true) {
                let _ = window.destroy();
            }
        }
    });
}

pub(crate) fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) => PathBuf::from(plain),
        None => path,
    }
}

/// Installed builds carry the engine in resources. Running from source uses vendor/engine.
pub(crate) fn engine_root(app: &AppHandle) -> PathBuf {
    if let Ok(resources) = app.path().resource_dir() {
        let bundled = strip_verbatim(resources).join("engine");
        if bundled.join("bin").exists() {
            return bundled;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("vendor").join("engine")
}

/// The drawing made for the display's scale, because Windows blurs an icon it has to resize.
pub(crate) fn tray_icon() -> Image<'static> {
    match win32::system_dpi() {
        0..=108 => tauri::include_image!("icons/tray-16.png"),
        109..=132 => tauri::include_image!("icons/tray-20.png"),
        133..=168 => tauri::include_image!("icons/tray-24.png"),
        _ => tauri::include_image!("icons/tray-32.png"),
    }
}

/// The tray's menu. With a capture card chosen, it can switch between the card and this PC.
pub(crate) fn tray_menu(app: &AppHandle, settings: &Settings) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let mut menu = MenuBuilder::new(app)
        .text("show", "Open Afterclip")
        .text("editor", "Open editor")
        .text("clip", "Clip whole buffer")
        .text("record", "Start / stop recording");
    if !settings.capture_card_device.is_empty() {
        menu = menu.text("card", if settings.capture_card { "Record this PC instead" } else { "Record the capture card" });
    }
    menu.separator().text("quit", "Quit").build()
}

/// Brings the tray's menu up to date after the capture card is switched on or off.
pub(crate) fn refresh_tray(core: &Shared) {
    let settings = core.settings.lock().unwrap().clone();
    if let (Some(tray), Ok(menu)) = (core.app.tray_by_id("afterclip"), tray_menu(&core.app, &settings)) {
        let _ = tray.set_menu(Some(menu));
    }
}

pub(crate) fn create_tray(app: &AppHandle, core: Shared) -> tauri::Result<()> {
    let menu = tray_menu(app, &core.settings.lock().unwrap().clone())?;
    let menu_core = Arc::clone(&core);
    TrayIconBuilder::with_id("afterclip")
        .icon(tray_icon())
        .tooltip("Afterclip")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || show_panel(&core));
            }
            "editor" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || show_editor(&core, Vec::new()));
            }
            "clip" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || { let _ = core.clip(0); });
            }
            "record" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || { let _ = core.toggle_record(); });
            }
            "card" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || { let _ = toggle_card(&core); });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(move |_, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = event {
                let position = rect.position.to_physical::<f64>(1.0);
                let size = rect.size.to_physical::<f64>(1.0);
                *core.tray_anchor.lock().unwrap() = Some(((position.x + size.width / 2.0) as i32, (position.y + size.height / 2.0) as i32));
                toggle_panel(&core);
            }
        })
        .build(app)?;
    Ok(())
}
