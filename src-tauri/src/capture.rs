use crate::engine::Engine;
use crate::win32::{active_endpoints_of, foreground_fullscreen_app, init_com_for_thread, is_self_elevated, window_still_open, FullscreenApp};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(1);
// Hooking a game normally takes a few seconds. After this, say capture has fallen back.
const HOOK_GRACE: Duration = Duration::from_secs(10);
// How many polls between checking which device the game plays through.
const AUDIO_CHECK_EVERY: u32 = 3;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureState {
    /// desktop: primary monitor. detecting: fullscreen app found, hooking. game: hooked. fallback: hook failed, recording its monitor.
    pub mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The program name, in lower case, which is what per-game settings look for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// The app runs above Afterclip's privilege level, so game capture can't reach it.
    pub needs_admin: bool,
    /// The hook failed for an unknown reason, and running as administrator is worth trying.
    pub suggest_admin: bool,
    pub elevated: bool,
}

/// Chooses what to record. It always tries the fullscreen app with game capture while its monitor
/// records underneath, so footage exists during the hook and if the hook never lands.
pub struct CaptureManager {
    state: Mutex<CaptureState>,
}

impl CaptureManager {
    pub fn start(engine: Arc<Engine>, enabled: impl Fn() -> bool + Send + 'static, on_change: impl Fn(&CaptureState) + Send + 'static) -> Arc<Self> {
        let manager = Arc::new(CaptureManager {
            state: Mutex::new(CaptureState {
                mode: "desktop",
                app: None,
                exe: None,
                needs_admin: false,
                suggest_admin: false,
                elevated: is_self_elevated(),
            }),
        });
        let me = Arc::clone(&manager);
        thread::spawn(move || {
            init_com_for_thread();
            let mut watcher = Watcher { game_mix: None, audio_check_in: 0, current: None, candidate: None, deadline: Instant::now(), display_hidden: false, generation: 0 };
            loop {
                thread::sleep(POLL);
                if engine.status().state == "ready" && enabled() {
                    me.tick(&engine, &mut watcher, &on_change);
                }
            }
        });
        manager
    }

    pub fn state(&self) -> CaptureState {
        self.state.lock().unwrap().clone()
    }

    fn publish(&self, on_change: &impl Fn(&CaptureState), change: impl FnOnce(&mut CaptureState)) {
        let mut state = self.state.lock().unwrap();
        let before = state.clone();
        change(&mut state);
        if *state != before {
            on_change(&state);
        }
    }

    fn tick(&self, engine: &Engine, w: &mut Watcher, on_change: &impl Fn(&CaptureState)) {
        // Every engine start rebuilds the scene on the desktop, so detection starts over too.
        if engine.generation() != w.generation {
            w.generation = engine.generation();
            w.current = None;
            w.candidate = None;
            w.display_hidden = false;
            w.game_mix = None;
            engine.set_game(None);
            self.publish(on_change, |s| {
                s.mode = "desktop";
                s.app = None;
                s.exe = None;
                s.needs_admin = false;
                s.suggest_admin = false;
            });
        }
        let found = foreground_fullscreen_app();
        // Needs two polls in a row, so a window flashing past doesn't reconfigure capture.
        let stable = found.as_ref().filter(|f| Some(f.hwnd) == w.candidate);
        w.candidate = found.as_ref().map(|f| f.hwnd);

        if let Some(app) = stable.filter(|f| Some(f.hwnd) != w.current.as_ref().map(|c| c.hwnd)) {
            if engine.capture_app(app).is_err() {
                return;
            }
            w.current = Some(app.clone());
            w.display_hidden = false;
            w.game_mix = None;
            w.audio_check_in = 0;
            w.deadline = Instant::now() + HOOK_GRACE;
            if engine.game_audio_enabled() {
                let _ = engine.set_game_audio(Some(app), false);
            }
            // Some games pad their titles with invisible characters, Call of Duty among them.
            let visible: String = app.title.chars().filter(|c| !c.is_control() && !('\u{200B}'..='\u{200F}').contains(c) && *c != '\u{FEFF}').collect();
            let name = if visible.trim().is_empty() { app.exe.clone() } else { visible.trim().to_string() };
            let blocked = app.blocked_by_elevation;
            let exe = app.exe.to_lowercase();
            engine.set_game(Some(name.clone()));
            self.publish(on_change, |s| {
                s.mode = "detecting";
                s.app = Some(name);
                s.exe = Some(exe);
                s.needs_admin = blocked;
                s.suggest_admin = false;
            });
        } else if w.current.as_ref().is_some_and(|c| !window_still_open(c.hwnd)) {
            if engine.capture_desktop().is_err() {
                return;
            }
            w.current = None;
            w.display_hidden = false;
            w.game_mix = None;
            engine.set_game(None);
            if engine.game_audio_enabled() {
                let _ = engine.set_game_audio(None, false);
            }
            self.publish(on_change, |s| {
                s.mode = "desktop";
                s.app = None;
                s.exe = None;
                s.needs_admin = false;
                s.suggest_admin = false;
            });
        }

        if let Some(current) = &w.current {
            if engine.game_audio_enabled() {
                if w.audio_check_in == 0 {
                    w.audio_check_in = AUDIO_CHECK_EVERY;
                    // Follows the game as it's moved between devices, such as dragging it to a mixer channel.
                    let playing_on = active_endpoints_of(current.pid);
                    if !playing_on.is_empty() {
                        let mix = playing_on.iter().any(|e| !engine.mix_records(e));
                        if w.game_mix != Some(mix) && engine.set_game_audio(Some(current), mix).is_ok() {
                            w.game_mix = Some(mix);
                        }
                    }
                }
                w.audio_check_in -= 1;
            }
            let Ok(hooked) = engine.is_app_hooked() else { return };
            let late = Instant::now() > w.deadline;
            if hooked != w.display_hidden && engine.set_display_visible(!hooked).is_ok() {
                // No point capturing a monitor the game completely covers. It comes back if the hook drops.
                w.display_hidden = hooked;
            }
            let blocked = current.blocked_by_elevation;
            self.publish(on_change, |s| {
                s.mode = if hooked { "game" } else if late { "fallback" } else { "detecting" };
                s.needs_admin = !hooked && blocked;
                s.suggest_admin = !hooked && late && !s.elevated && !blocked;
            });
        }
    }
}

struct Watcher {
    /// The last mix decision sent to the engine for game audio.
    game_mix: Option<bool>,
    audio_check_in: u32,
    current: Option<FullscreenApp>,
    candidate: Option<isize>,
    deadline: Instant,
    display_hidden: bool,
    generation: u64,
}
