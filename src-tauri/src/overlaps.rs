//! Finds clips saved so close together that they show the same moment, such as pressing the clip key twice.
//! A saved clip is the last part of the buffer when it was saved, so its name says when it ends, and its
//! length says when it starts.

/// A clip saved from the buffer, with when it ended (seconds since a fixed point) and how long it is.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedClip {
    pub name: String,
    pub ends: f64,
    pub seconds: f64,
    pub favourite: bool,
}

/// Two clips that show the same moment. `covered` is the shorter one, mostly inside `keep`.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlap {
    pub keep: String,
    pub covered: String,
    pub shared_seconds: f64,
}

// Counts as the same moment when at least this much of the shorter clip is in the longer one.
const SHARED_SHARE: f64 = 0.6;

/// When a clip from the buffer was saved, from a name like Game_2026-11-11_20-15-03_30s.mp4 (with _2 and so
/// on added for a repeat). None for anything else, such as an edit, a compressed copy or a renamed clip,
/// whose dates don't say what moment they show.
pub fn saved_at(name: &str) -> Option<f64> {
    let stem = name.strip_suffix(".mp4").or_else(|| name.strip_suffix(".mkv"))?;
    let parts: Vec<&str> = stem.rsplitn(4, '_').collect();
    // Without a repeat number: [label, time, date, prefix]. With one: [n, label, time, date+prefix].
    let (label, time, date) = match parts.as_slice() {
        [n, label, time, rest] if n.chars().all(|c| c.is_ascii_digit()) && is_label(label) => (*label, *time, rest.rsplit('_').next()?),
        [label, time, date, _] => (*label, *time, *date),
        _ => return None,
    };
    if !is_label(label) {
        return None;
    }
    let date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let time = chrono::NaiveTime::parse_from_str(time, "%H-%M-%S").ok()?;
    Some(date.and_time(time).and_utc().timestamp() as f64)
}

fn is_label(label: &str) -> bool {
    label == "full" || label.strip_suffix('s').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Pairs of clips that show the same moment, newest first. A favourite is never the one suggested to go,
/// and each clip is in one pair at most, so removing every suggestion never removes both sides.
pub fn find(mut clips: Vec<SavedClip>) -> Vec<Overlap> {
    clips.sort_by(|a, b| b.ends.total_cmp(&a.ends));
    let mut used = std::collections::BTreeSet::new();
    let mut found = Vec::new();
    for i in 0..clips.len() {
        for j in i + 1..clips.len() {
            let (a, b) = (&clips[i], &clips[j]);
            // Sorted newest first, so once one ends before this one starts, none further can overlap.
            if b.ends <= a.ends - a.seconds {
                break;
            }
            if used.contains(&a.name) || used.contains(&b.name) {
                continue;
            }
            let shared = a.ends.min(b.ends) - (a.ends - a.seconds).max(b.ends - b.seconds);
            let shorter = a.seconds.min(b.seconds);
            if shorter <= 0.0 || shared < shorter * SHARED_SHARE {
                continue;
            }
            let (keep, covered) = if a.seconds >= b.seconds { (a, b) } else { (b, a) };
            if covered.favourite {
                continue;
            }
            used.insert(a.name.clone());
            used.insert(b.name.clone());
            found.push(Overlap { keep: keep.name.clone(), covered: covered.name.clone(), shared_seconds: shared.max(0.0) });
        }
    }
    found
}

/// The key a pair is remembered by when someone keeps both, the same whichever way round.
pub fn pair_key(a: &str, b: &str) -> String {
    if a <= b { format!("{a}|{b}") } else { format!("{b}|{a}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(name: &str, ends: f64, seconds: f64) -> SavedClip {
        SavedClip { name: name.into(), ends, seconds, favourite: false }
    }

    #[test]
    fn reads_when_a_clip_was_saved_from_its_name() {
        let at = saved_at("Apex Legends_2026-11-11_20-15-03_30s.mp4").unwrap();
        assert_eq!(saved_at("Clip_2026-11-11_20-15-13_full.mp4").unwrap() - at, 10.0);
        assert_eq!(saved_at("Apex_2026-11-11_20-15-03_30s_2.mp4"), Some(at), "a repeat number is fine");
        assert_eq!(saved_at("My_Game_2026-11-11_20-15-03_15s.mp4"), Some(at), "a game name can have underscores");
        for other in ["Apex_2026-11-11_20-15-03_30s_100MB.mp4", "Apex_2026-11-11_20-15-03_30s_vertical_2.mp4", "Merged_2026-11-11_20-15-03.mp4", "clutch.mp4", "Apex_2026-11-11_20-15-03_30s-compressed.mp4", "Apex_2026-11-11_20-15-03_30s.png"] {
            assert_eq!(saved_at(other), None, "{other}");
        }
    }

    #[test]
    fn a_clip_inside_a_longer_one_is_suggested() {
        // Saved 5 seconds apart: the 15 second one is all inside the 60 second one.
        let found = find(vec![clip("long", 105.0, 60.0), clip("short", 100.0, 15.0)]);
        assert_eq!(found, vec![Overlap { keep: "long".into(), covered: "short".into(), shared_seconds: 15.0 }]);
    }

    #[test]
    fn clips_of_different_moments_are_left_alone() {
        assert!(find(vec![clip("a", 100.0, 30.0), clip("b", 200.0, 30.0)]).is_empty());
        // Only 10 of 30 seconds shared.
        assert!(find(vec![clip("a", 100.0, 30.0), clip("b", 120.0, 30.0)]).is_empty());
    }

    #[test]
    fn a_favourite_is_never_the_one_to_go() {
        let mut short = clip("short", 100.0, 15.0);
        short.favourite = true;
        assert!(find(vec![clip("long", 105.0, 60.0), short]).is_empty());
    }

    #[test]
    fn each_clip_is_in_one_pair_at_most() {
        let found = find(vec![clip("a", 100.0, 30.0), clip("b", 102.0, 30.0), clip("c", 104.0, 30.0)]);
        assert_eq!(found.len(), 1);
        assert_eq!(pair_key("b", "a"), pair_key("a", "b"));
    }
}
