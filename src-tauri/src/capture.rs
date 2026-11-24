use crate::engine::Engine;
use crate::win32::{active_endpoints_of, clean_title, foreground_game_app, init_com_for_thread, is_self_elevated, window_still_open, FullscreenApp};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(1);
/// What clips, recordings, screenshots and their game folder are named while a capture card is recorded.
pub const CARD_NAME: &str = "CaptureCard";
// A card normally sends its first picture within a few seconds of opening. After this, say it isn't.
const NO_PICTURE_AFTER: Duration = Duration::from_secs(10);
// While the card has sent no picture, it's opened again this often, so one that wasn't ready when capture
// started, or that another app has since let go of, is picked up. Reopening takes a few seconds, so not more often.
const REOPEN_CARD_EVERY: Duration = Duration::from_secs(30);
// The card has shown only a plain, still picture, such as its NO SIGNAL screen, for this long, so the buffer pauses.
// Long enough that a black loading screen or fade never pauses it, and a console just switched off can still be clipped.
const NO_SIGNAL_PAUSE_AFTER: Duration = Duration::from_secs(60);
// Polls in a row with a real picture before a paused buffer starts again, so one odd frame doesn't start it.
const SIGNAL_RESUME_POLLS: u32 = 2;
// Hooking a game normally takes a few seconds. After this, say capture has fallen back.
const HOOK_GRACE: Duration = Duration::from_secs(10);
// A game whose anti-cheat blocks the hook, such as Call of Duty, refuses it every time, and each try reaches into
// the game's process, which can make it stutter. After this long with no hook at all, its monitor is recorded instead.
const HOOK_GIVE_UP: Duration = Duration::from_secs(60);
// How many polls between checking which device the game plays through.
const AUDIO_CHECK_EVERY: u32 = 3;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureState {
    /// desktop: primary monitor. detecting: game found, hooking. game: hooked. fallback: hook failed, recording its monitor.
    /// card: a capture card, so this PC's games aren't looked for.
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
    /// The capture card has sent no picture, usually because another app has it open.
    pub no_picture: bool,
    /// The capture card shows no signal, so the buffer is paused until a picture arrives.
    pub no_signal: bool,
}

/// What the watcher looks for.
#[derive(Clone, Copy, PartialEq)]
pub enum Watch {
    /// Nothing, as for the tests' colour card.
    Off,
    /// Games on this PC.
    Games,
    /// Nothing on this PC, because a capture card is what's recorded. With pause_without_signal, the buffer
    /// pauses while the card shows no signal.
    Card { pause_without_signal: bool },
}

/// Chooses what to record. It always tries the fullscreen app, or a game you've added in any window,
/// with game capture while its monitor records underneath, so footage exists during the hook and if the hook never lands.
pub struct CaptureManager {
    state: Mutex<CaptureState>,
}

impl CaptureManager {
    pub fn start(engine: Arc<Engine>, watch: impl Fn() -> Watch + Send + 'static, added_games: impl Fn() -> Vec<String> + Send + 'static, on_change: impl Fn(&CaptureState) + Send + 'static) -> Arc<Self> {
        let manager = Arc::new(CaptureManager {
            state: Mutex::new(CaptureState {
                mode: "desktop",
                app: None,
                exe: None,
                needs_admin: false,
                suggest_admin: false,
                elevated: is_self_elevated(),
                no_picture: false,
                no_signal: false,
            }),
        });
        let me = Arc::clone(&manager);
        thread::spawn(move || {
            init_com_for_thread();
            let mut watcher = Watcher { game_mix: None, audio_check_in: 0, current: None, candidate: None, deadline: Instant::now(), display_hidden: false, generation: 0, privacy: None, hooked_once: false, gave_up: false, card_since: None, card_reopened: None, blank_since: None, picture_polls: 0 };
            loop {
                thread::sleep(POLL);
                if engine.status().state != "ready" {
                    continue;
                }
                match watch() {
                    Watch::Games => me.tick(&engine, &mut watcher, &added_games(), &on_change),
                    Watch::Card { pause_without_signal } => me.show_card(&engine, &mut watcher, pause_without_signal, &on_change),
                    Watch::Off => {}
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

    /// Says the card is being recorded. Leaving card mode restarts the engine, so game detection starts afresh then.
    fn show_card(&self, engine: &Engine, w: &mut Watcher, pause_without_signal: bool, on_change: &impl Fn(&CaptureState)) {
        // Any device can be behind a card, so its files are named for the card, not a console.
        if w.generation != engine.generation() || w.current.is_some() {
            engine.set_game(Some(CARD_NAME.into()));
            w.card_since = Some(Instant::now());
            w.card_reopened = None;
            w.blank_since = None;
            w.picture_polls = 0;
        }
        let waited = w.card_since.is_some_and(|since| since.elapsed() > NO_PICTURE_AFTER);
        let signal = engine.card_signal();
        let no_picture = waited && signal.is_some_and(|c| !c.picture);
        if no_picture && w.card_reopened.is_none_or(|at| at.elapsed() >= REOPEN_CARD_EVERY) {
            // Tried again on a later poll if it fails, like any other card change.
            w.card_reopened = Some(Instant::now());
            let _ = engine.reopen_card();
        }
        // No picture at all, once the card has had time to send one, is as empty as a NO SIGNAL screen.
        let blank = signal.is_some_and(|c| c.blank == Some(true) || (waited && !c.picture));
        if blank {
            w.blank_since.get_or_insert_with(Instant::now);
            w.picture_polls = 0;
        } else if signal.is_some_and(|c| c.blank == Some(false)) {
            w.blank_since = None;
            w.picture_polls += 1;
        }
        let status = engine.status();
        let long_blank = w.blank_since.is_some_and(|since| since.elapsed() >= NO_SIGNAL_PAUSE_AFTER);
        // A failure tries again on the next poll. A recording, which shares the encoder, starts the buffer too.
        if !status.waiting_for_signal && pause_without_signal && long_blank && status.buffering && !status.recording {
            let _ = engine.set_buffer_paused(true);
        } else if status.waiting_for_signal && (!pause_without_signal || status.recording || w.picture_polls >= SIGNAL_RESUME_POLLS) {
            let _ = engine.set_buffer_paused(false);
        }
        let no_signal = engine.status().waiting_for_signal;
        w.generation = engine.generation();
        w.current = None;
        w.candidate = None;
        self.publish(on_change, |s| {
            s.mode = "card";
            s.app = None;
            s.exe = None;
            s.needs_admin = false;
            s.suggest_admin = false;
            s.no_picture = no_picture;
            s.no_signal = no_signal;
        });
    }

    fn tick(&self, engine: &Engine, w: &mut Watcher, added_games: &[String], on_change: &impl Fn(&CaptureState)) {
        // Every engine start rebuilds the scene on the desktop, so detection starts over too.
        if engine.generation() != w.generation {
            w.generation = engine.generation();
            w.current = None;
            w.candidate = None;
            w.display_hidden = false;
            w.game_mix = None;
            w.privacy = None;
            w.card_since = None;
            engine.set_game(None);
            self.publish(on_change, |s| {
                s.mode = "desktop";
                s.app = None;
                s.exe = None;
                s.needs_admin = false;
                s.suggest_admin = false;
                s.no_picture = false;
                s.no_signal = false;
            });
        }
        let found = foreground_game_app(added_games);
        // Needs two polls in a row, so a window flashing past doesn't reconfigure capture.
        let stable = found.as_ref().filter(|f| Some(f.hwnd) == w.candidate);
        w.candidate = found.as_ref().map(|f| f.hwnd);

        if let Some(app) = stable.filter(|f| Some(f.hwnd) != w.current.as_ref().map(|c| c.hwnd)) {
            if engine.capture_app(app).is_err() {
                return;
            }
            w.current = Some(app.clone());
            w.privacy = None;
            w.display_hidden = false;
            w.game_mix = None;
            w.audio_check_in = 0;
            w.deadline = Instant::now() + HOOK_GRACE;
            w.hooked_once = false;
            w.gave_up = false;
            if engine.game_audio_enabled() {
                let _ = engine.set_game_audio(Some(app), false);
            }
            let visible = clean_title(&app.title);
            let name = if visible.is_empty() { app.exe.clone() } else { visible };
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
            let private = engine.privacy_for(&current.exe);
            if w.privacy != Some(private) && engine.set_game_privacy(current.pid, private).is_ok() {
                w.privacy = Some(private);
            }
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
            w.hooked_once |= hooked;
            // A game that hooked once may just be loading, and a private game needs the hook to be shown at all.
            let hopeless = Instant::now() > w.deadline - HOOK_GRACE + HOOK_GIVE_UP;
            if !hooked && !w.hooked_once && !w.gave_up && !private && hopeless && engine.stop_hooking(&current.exe).is_ok() {
                w.gave_up = true;
            }
            if hooked != w.display_hidden && engine.set_display_visible(!hooked).is_ok() {
                // No point capturing a monitor the game completely covers. It comes back if the hook drops.
                w.display_hidden = hooked;
            }
            let blocked = current.blocked_by_elevation;
            self.publish(on_change, |s| {
                s.mode = if private && (!hooked || found.as_ref().is_none_or(|app| app.pid != current.pid)) {
                    "private"
                } else if hooked { "game" } else if late { "fallback" } else { "detecting" };
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
    privacy: Option<bool>,
    /// The current game has been hooked at least once.
    hooked_once: bool,
    /// The current game kept refusing the hook, so it's no longer tried until it's opened again.
    gave_up: bool,
    /// When the capture card was opened, to give it time to send its first picture.
    card_since: Option<Instant>,
    /// When the card, having sent no picture, was last opened again.
    card_reopened: Option<Instant>,
    /// Since when the card has shown only a plain, still picture, such as its NO SIGNAL screen.
    blank_since: Option<Instant>,
    /// Polls in a row with a real picture from the card.
    picture_polls: u32,
}
