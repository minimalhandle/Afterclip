use std::fs;
use std::io;
use std::path::Path;

/// Moves files from where an earlier version kept them. A file already at the new place wins, and
/// an original only goes once its copy is complete, so a failure here loses nothing.
pub fn move_files(old: &Path, new: &Path, names: &[&str]) {
    for name in names {
        let (from, to) = (old.join(name), new.join(name));
        if !from.exists() {
            continue;
        }
        if to.exists() || copy_whole(&from, &to).is_ok() {
            let _ = fs::remove_file(from);
        }
    }
    // Only goes if it's now empty, so anything else in there stays.
    let _ = fs::remove_dir(old);
}

fn copy_whole(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    let partial = to.with_extension("moving");
    fs::copy(from, &partial)?;
    fs::rename(&partial, to)
}

/// Clears the thumbnails an earlier version kept beside the clips. They're rebuilt from the clips
/// when needed. This is someone's save folder, so anything else that's in there stays.
pub fn remove_old_thumbnails(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if matches!(path.extension().and_then(|e| e.to_str()), Some("jpg" | "txt")) {
            let _ = fs::remove_file(path);
        }
    }
    let _ = fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("afterclip-move-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moves_files_and_removes_the_empty_folder() {
        let root = folder("moves");
        let (old, new) = (root.join("old"), root.join("new"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("settings.json"), "mine").unwrap();
        move_files(&old, &new, &["settings.json", "library.json"]);
        assert_eq!(fs::read_to_string(new.join("settings.json")).unwrap(), "mine");
        assert!(!old.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keeps_a_file_already_moved_and_anything_it_does_not_know() {
        let root = folder("keeps");
        let (old, new) = (root.join("old"), root.join("new"));
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("settings.json"), "old").unwrap();
        fs::write(old.join("notes.txt"), "someone's").unwrap();
        fs::write(new.join("settings.json"), "new").unwrap();
        move_files(&old, &new, &["settings.json"]);
        assert_eq!(fs::read_to_string(new.join("settings.json")).unwrap(), "new");
        assert!(!old.join("settings.json").exists());
        assert!(old.join("notes.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_copy_keeps_the_original() {
        let root = folder("fails");
        let old = root.join("old");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("settings.json"), "mine").unwrap();
        // A file where the new folder should go makes the copy fail.
        let blocked = root.join("blocked");
        fs::write(&blocked, "").unwrap();
        move_files(&old, &blocked, &["settings.json"]);
        assert_eq!(fs::read_to_string(old.join("settings.json")).unwrap(), "mine");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_thumbnails_go_but_other_files_stay() {
        let root = folder("thumbs");
        fs::write(root.join("Clip.mp4-1.jpg"), "").unwrap();
        fs::write(root.join("Clip.mp4-1.txt"), "").unwrap();
        fs::write(root.join("keep.mp4"), "").unwrap();
        remove_old_thumbnails(&root);
        assert!(!root.join("Clip.mp4-1.jpg").exists());
        assert!(root.join("keep.mp4").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_empty_thumbnail_folder_goes_and_a_missing_one_is_fine() {
        let root = folder("empty");
        remove_old_thumbnails(&root);
        assert!(!root.exists());
        remove_old_thumbnails(&root);
    }
}
