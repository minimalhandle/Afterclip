//! What Afterclip remembers about clips beyond the files themselves: favourites, tags, where a webcam sat, and
//! the game a renamed clip came from. Kept beside the settings, so clips stay ordinary files you can move around.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_NAME_CHARS: usize = 120;
const MAX_TAG_CHARS: usize = 30;
const MAX_TAGS: usize = 10;

#[derive(Default, Serialize, Deserialize)]
pub struct LibraryData {
    #[serde(default)]
    pub favourites: BTreeSet<String>,
    #[serde(default)]
    pub games: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, Vec<String>>,
    /// Where the webcam sat, as fractions of the picture: x, y, width, height.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cameras: BTreeMap<String, [f64; 4]>,
}

impl LibraryData {
    pub fn load(file: &Path) -> Self {
        fs::read_to_string(file).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self, file: &Path) -> Result<(), String> {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(file, text).map_err(|e| e.to_string())
    }

    pub fn forget(&mut self, name: &str) {
        self.favourites.remove(name);
        self.games.remove(name);
        self.tags.remove(name);
        self.cameras.remove(name);
    }

    /// Replaces a clip's tags. Empty removes them.
    pub fn set_tags(&mut self, name: &str, typed: &[String]) {
        let tags = clean_tags(typed);
        if tags.is_empty() {
            self.tags.remove(name);
        } else {
            self.tags.insert(name.to_string(), tags);
        }
    }

    pub fn rename(&mut self, from: &str, to: &str, game: Option<String>) {
        if self.favourites.remove(from) {
            self.favourites.insert(to.to_string());
        }
        let known = self.games.remove(from).or(game);
        if let Some(game) = known {
            self.games.insert(to.to_string(), game);
        }
        if let Some(tags) = self.tags.remove(from) {
            self.tags.insert(to.to_string(), tags);
        }
        if let Some(camera) = self.cameras.remove(from) {
            self.cameras.insert(to.to_string(), camera);
        }
    }
}

/// Tags as someone typed them, without blanks, repeats or anything too long. Case is kept, but Funny and funny are one tag.
pub fn clean_tags(typed: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for tag in typed {
        let tag: String = tag.trim().trim_start_matches('#').chars().filter(|c| !c.is_control()).take(MAX_TAG_CHARS).collect();
        let tag = tag.trim().to_string();
        if !tag.is_empty() && !kept.iter().any(|k| k.eq_ignore_ascii_case(&tag)) && kept.len() < MAX_TAGS {
            kept.push(tag);
        }
    }
    kept
}

/// Turns what someone typed into a safe file name, or says why it can't be one.
pub fn clean_file_stem(typed: &str) -> Result<String, String> {
    let kept: String = typed.chars().filter(|c| !r#"<>:"/\|?*%"#.contains(*c) && !c.is_control()).take(MAX_NAME_CHARS).collect();
    let stem = kept.trim().trim_end_matches('.').trim().to_string();
    if stem.is_empty() {
        return Err("Type a name for this clip".into());
    }
    let upper = stem.to_uppercase();
    let base = upper.split('.').next().unwrap_or("");
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&base)
        || (base.len() == 4 && (base.starts_with("COM") || base.starts_with("LPT")) && base.ends_with(|c: char| c.is_ascii_digit()));
    if reserved {
        return Err(format!("Windows doesn't allow the name {stem}"));
    }
    Ok(stem)
}

pub struct StoredClip {
    pub path: PathBuf,
    pub size: u64,
    pub modified: f64,
    pub favourite: bool,
}

/// The oldest clips to remove to get under the limit. Favourites are never chosen, even if that leaves it over.
pub fn over_limit(clips: Vec<StoredClip>, limit_bytes: u64) -> Vec<PathBuf> {
    let mut total: u64 = clips.iter().map(|c| c.size).sum();
    let mut removable: Vec<StoredClip> = clips.into_iter().filter(|c| !c.favourite).collect();
    removable.sort_by(|a, b| a.modified.total_cmp(&b.modified));
    let mut chosen = Vec::new();
    for clip in removable {
        if total <= limit_bytes {
            break;
        }
        total -= clip.size;
        chosen.push(clip.path);
    }
    chosen
}

/// Clips older than this many days. Favourites are never chosen.
pub fn older_than(clips: &[StoredClip], days: i64, now_ms: f64) -> Vec<PathBuf> {
    let cutoff = now_ms - days as f64 * 86_400_000.0;
    clips.iter().filter(|c| !c.favourite && c.modified < cutoff).map(|c| c.path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(name: &str, size: u64, modified: f64, favourite: bool) -> StoredClip {
        StoredClip { path: PathBuf::from(name), size, modified, favourite }
    }

    #[test]
    fn removes_the_oldest_first_and_stops_at_the_limit() {
        let chosen = over_limit(vec![clip("new", 40, 3.0, false), clip("old", 40, 1.0, false), clip("mid", 40, 2.0, false)], 80);
        assert_eq!(chosen, vec![PathBuf::from("old")]);
    }

    #[test]
    fn never_removes_favourites() {
        let chosen = over_limit(vec![clip("best", 100, 1.0, true), clip("other", 10, 2.0, false)], 5);
        assert_eq!(chosen, vec![PathBuf::from("other")]);
    }

    #[test]
    fn removes_nothing_under_the_limit() {
        assert!(over_limit(vec![clip("a", 10, 1.0, false)], 10).is_empty());
    }

    #[test]
    fn cleans_up_only_old_clips_that_are_not_favourites() {
        const DAY: f64 = 86_400_000.0;
        let now = 100.0 * DAY;
        let clips = vec![clip("new", 1, 90.0 * DAY, false), clip("old", 1, 50.0 * DAY, false), clip("kept", 1, 10.0 * DAY, true)];
        assert_eq!(older_than(&clips, 30, now), vec![PathBuf::from("old")]);
    }

    #[test]
    fn names_lose_what_windows_refuses() {
        assert_eq!(clean_file_stem(" Big: clutch? ").unwrap(), "Big clutch");
        assert_eq!(clean_file_stem("ace...").unwrap(), "ace");
        assert!(clean_file_stem("..\\..").is_err());
        assert!(clean_file_stem("   ").is_err());
        assert!(clean_file_stem("con").is_err());
        assert!(clean_file_stem("COM1").is_err());
        assert_eq!(clean_file_stem("Console").unwrap(), "Console");
        assert_eq!(clean_file_stem(&"x".repeat(500)).unwrap().len(), MAX_NAME_CHARS);
    }

    #[test]
    fn tags_are_tidied() {
        let typed: Vec<String> = [" #Funny ", "funny", "", "clutch", &"x".repeat(50)].iter().map(|s| s.to_string()).collect();
        assert_eq!(clean_tags(&typed), vec!["Funny".to_string(), "clutch".into(), "x".repeat(MAX_TAG_CHARS)]);
        let many: Vec<String> = (0..20).map(|n| n.to_string()).collect();
        assert_eq!(clean_tags(&many).len(), MAX_TAGS);
        let mut data = LibraryData::default();
        data.set_tags("a.mp4", &["win".into()]);
        data.cameras.insert("a.mp4".into(), [0.7, 0.7, 0.25, 0.25]);
        data.rename("a.mp4", "b.mp4", None);
        assert_eq!(data.tags.get("b.mp4"), Some(&vec!["win".to_string()]));
        assert!(data.cameras.contains_key("b.mp4"));
        data.set_tags("b.mp4", &[]);
        data.forget("b.mp4");
        assert!(data.tags.is_empty() && data.cameras.is_empty());
    }

    #[test]
    fn renaming_carries_favourite_and_game() {

        let mut data = LibraryData::default();
        data.favourites.insert("a.mp4".into());
        data.rename("a.mp4", "b.mp4", Some("Game".into()));
        assert!(data.favourites.contains("b.mp4") && !data.favourites.contains("a.mp4"));
        assert_eq!(data.games.get("b.mp4").map(String::as_str), Some("Game"));
    }
}
