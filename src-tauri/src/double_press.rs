//! Pressing a clip key again soon after the last press makes one longer clip, reaching back to the first press,
//! instead of two clips that show the same moment.

use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A second press this soon after the last one extends it.
pub const WINDOW: Duration = Duration::from_secs(10);
/// How long a press waits for the one before it to finish saving, so it can replace it.
pub const WAIT_FOR_EARLIER: Duration = Duration::from_secs(90);

/// One press of a clip key, and the clip it saved once it's done.
pub struct Press {
    pub at: Instant,
    /// What this press saves. 0 is the whole buffer.
    pub seconds: i64,
    saved: Mutex<Option<Option<PathBuf>>>,
    done: Condvar,
}

impl Press {
    pub fn new(at: Instant, seconds: i64) -> Self {
        Press { at, seconds, saved: Mutex::new(None), done: Condvar::new() }
    }

    /// Says the press finished, with the clip it saved, or None if saving failed.
    pub fn finish(&self, file: Option<PathBuf>) {
        *self.saved.lock().unwrap() = Some(file);
        self.done.notify_all();
    }

    /// Waits for the press to finish, and returns its clip. None if it failed or took too long.
    pub fn wait(&self, longest: Duration) -> Option<PathBuf> {
        let saved = self.saved.lock().unwrap();
        let (saved, _) = self.done.wait_timeout_while(saved, longest, |s| s.is_none()).unwrap();
        saved.clone().flatten()
    }
}

/// How long a press at `now` should save, when it extends `earlier`: from the earlier press's start to now, or
/// what was asked if that's longer. None when it doesn't extend it: too late, or either is the whole buffer.
pub fn extended_seconds(earlier: &Press, now: Instant, seconds: i64) -> Option<i64> {
    let gap = now.checked_duration_since(earlier.at)?;
    if gap > WINDOW || seconds <= 0 || earlier.seconds <= 0 {
        return None;
    }
    let reach = earlier.seconds + gap.as_secs_f64().ceil() as i64;
    Some(reach.max(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn a_quick_second_press_reaches_back_to_the_first() {
        let start = Instant::now();
        let earlier = Press::new(start, 30);
        assert_eq!(extended_seconds(&earlier, start + Duration::from_millis(4200), 30), Some(35));
        // A longer key wins when it already reaches further back.
        assert_eq!(extended_seconds(&earlier, start + Duration::from_secs(2), 60), Some(60));
    }

    #[test]
    fn a_late_press_or_the_whole_buffer_is_its_own_clip() {
        let start = Instant::now();
        let earlier = Press::new(start, 30);
        assert_eq!(extended_seconds(&earlier, start + WINDOW + Duration::from_millis(1), 30), None);
        assert_eq!(extended_seconds(&earlier, start + Duration::from_secs(1), 0), None);
        assert_eq!(extended_seconds(&Press::new(start, 0), start + Duration::from_secs(1), 30), None);
    }

    #[test]
    fn a_press_waits_for_the_earlier_clip() {
        let earlier = Arc::new(Press::new(Instant::now(), 30));
        let saving = Arc::clone(&earlier);
        let saver = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            saving.finish(Some(PathBuf::from("a.mp4")));
        });
        assert_eq!(earlier.wait(Duration::from_secs(5)), Some(PathBuf::from("a.mp4")));
        saver.join().unwrap();
        let failed = Press::new(Instant::now(), 30);
        failed.finish(None);
        assert_eq!(failed.wait(Duration::from_secs(5)), None);
        assert_eq!(Press::new(Instant::now(), 30).wait(Duration::from_millis(10)), None, "gives up");
    }
}
