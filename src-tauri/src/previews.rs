//! Thumbnails, scrub strips and loudness are made in the background, a few at a time. Each one reads a clip,
//! so deleting or replacing that clip waits for all of them to finish first.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

/// How many previews of one kind are made at once while no game is running. During a game it's one, so
/// they never compete with it.
pub const AT_ONCE: usize = 3;

#[derive(Default)]
struct State {
    running: usize,
    /// Outputs being made right now, so two requests for the same one never write it together.
    busy: HashSet<PathBuf>,
    /// Holders of `exclusive`, which stop new work starting.
    closed: usize,
}

/// Lets up to a limit of jobs run at once, never two for the same output, and none while something holds it
/// exclusively.
#[derive(Default)]
pub struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

/// A running job. Dropping it lets the next one start.
pub struct Pass<'a> {
    gate: &'a Gate,
    output: PathBuf,
}

/// Holds off every job until dropped.
pub struct Exclusive<'a> {
    gate: &'a Gate,
}

impl Gate {
    /// Waits for a free place, for no other job making `output`, and for nothing holding the gate.
    pub fn enter(&self, output: &Path, limit: usize) -> Pass<'_> {
        let limit = limit.max(1);
        let mut state = self.state.lock().unwrap();
        while state.closed > 0 || state.running >= limit || state.busy.contains(output) {
            state = self.changed.wait(state).unwrap();
        }
        state.running += 1;
        state.busy.insert(output.to_path_buf());
        Pass { gate: self, output: output.to_path_buf() }
    }

    /// Stops new jobs starting and waits for running ones to finish.
    pub fn exclusive(&self) -> Exclusive<'_> {
        let mut state = self.state.lock().unwrap();
        state.closed += 1;
        while state.running > 0 {
            state = self.changed.wait(state).unwrap();
        }
        Exclusive { gate: self }
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        state.running -= 1;
        state.busy.remove(&self.output);
        self.gate.changed.notify_all();
    }
}

impl Drop for Exclusive<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        state.closed -= 1;
        self.gate.changed.notify_all();
    }
}

/// The clip a cache file belongs to: cache files are named after it, then a dash and its modified time, then
/// maybe a kind, such as `Clip.mp4-1700000000000-strip.jpg`.
fn cached_for(cache_file: &str) -> Option<&str> {
    let ends_in_time = |stem: &str| stem.rsplit_once('-').is_some_and(|(_, t)| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()));
    let (stem, _extension) = cache_file.rsplit_once('.')?;
    // Either `name-mtime.ext` or `name-mtime-kind.ext`.
    if ends_in_time(stem) {
        return Some(stem);
    }
    let (stem, _kind) = stem.rsplit_once('-')?;
    ends_in_time(stem).then_some(stem)
}

/// Deletes cached previews of clips that are gone or have changed, such as ones deleted in File Explorer.
/// `current` names each clip with its modified time, the same way the cache files start. Returns how many went.
pub fn sweep(dir: &Path, current: &HashSet<String>) -> usize {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(owner) = cached_for(&name) else { continue };
        if !current.contains(owner) && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn cache_files_name_their_clip_and_time() {
        assert_eq!(cached_for("Clip 1.mp4-1700000000000.jpg"), Some("Clip 1.mp4-1700000000000"));
        assert_eq!(cached_for("Clip 1.mp4-1700000000000.txt"), Some("Clip 1.mp4-1700000000000"));
        assert_eq!(cached_for("Clip-2.mp4-1700000000000-strip.jpg"), Some("Clip-2.mp4-1700000000000"));
        assert_eq!(cached_for("Clip.mp4-1700000000000-wave.json"), Some("Clip.mp4-1700000000000"));
        assert_eq!(cached_for("notes.txt"), None);
    }

    #[test]
    fn sweep_keeps_current_clips_only() {
        let dir = std::env::temp_dir().join(format!("afterclip-sweep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for file in ["A.mp4-1.jpg", "A.mp4-1.txt", "A.mp4-1-strip.jpg", "A.mp4-0.jpg", "Gone.mp4-5.jpg", "Gone.mp4-5-wave.json", "other.dat"] {
            fs::write(dir.join(file), b"x").unwrap();
        }
        let current: HashSet<String> = ["A.mp4-1".to_string()].into();
        assert_eq!(sweep(&dir, &current), 3);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["A.mp4-1-strip.jpg", "A.mp4-1.jpg", "A.mp4-1.txt", "other.dat"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_up_to_the_limit_at_once() {
        let gate = Arc::new(Gate::default());
        let now = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..8).map(|i| {
            let (gate, now, most) = (gate.clone(), now.clone(), most.clone());
            thread::spawn(move || {
                let _pass = gate.enter(Path::new(&format!("{i}.jpg")), 3);
                let running = now.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(running, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(30));
                now.fetch_sub(1, Ordering::SeqCst);
            })
        }).collect();
        workers.into_iter().for_each(|w| w.join().unwrap());
        assert_eq!(most.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn the_same_output_is_never_made_twice_at_once() {
        let gate = Arc::new(Gate::default());
        let now = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..4).map(|_| {
            let (gate, now) = (gate.clone(), now.clone());
            thread::spawn(move || {
                let _pass = gate.enter(Path::new("same.jpg"), 3);
                assert_eq!(now.fetch_add(1, Ordering::SeqCst), 0);
                thread::sleep(Duration::from_millis(20));
                now.fetch_sub(1, Ordering::SeqCst);
            })
        }).collect();
        workers.into_iter().for_each(|w| w.join().unwrap());
    }

    #[test]
    fn exclusive_waits_for_running_jobs_and_holds_new_ones() {
        let gate = Arc::new(Gate::default());
        let pass = gate.enter(Path::new("a.jpg"), 3);
        let done = Arc::new(AtomicUsize::new(0));
        let waiter = {
            let (gate, done) = (gate.clone(), done.clone());
            thread::spawn(move || {
                let _all = gate.exclusive();
                done.store(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(50));
                done.store(2, Ordering::SeqCst);
            })
        };
        thread::sleep(Duration::from_millis(30));
        assert_eq!(done.load(Ordering::SeqCst), 0, "exclusive waited for the running job");
        drop(pass);
        while done.load(Ordering::SeqCst) == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        let _later = gate.enter(Path::new("b.jpg"), 3);
        assert_eq!(done.load(Ordering::SeqCst), 2, "a new job waited for exclusive to end");
        waiter.join().unwrap();
    }
}
