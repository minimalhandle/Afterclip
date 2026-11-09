use super::*;
use std::ffi::OsString;
use std::os::windows::process::CommandExt;

#[derive(Deserialize)]
pub struct Export {
    file: String,
    start: f64,
    end: f64,
    blur: Option<Blur>,
    audio: Option<AudioEdit>,
    vertical: bool,
}

#[derive(Deserialize)]
struct Blur {
    rect: [f64; 4],
    from: f64,
    to: f64,
}

#[derive(Deserialize)]
struct AudioEdit {
    track: Option<usize>,
    bleep: bool,
    from: f64,
    to: f64,
}

#[derive(Deserialize)]
pub struct AngleCut {
    time: f64,
    angle: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AngleExport {
    file: String,
    start: f64,
    end: f64,
    offset: f64,
    cuts: Vec<AngleCut>,
    continuous_audio: bool,
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn create(core: &Core) -> Result<Self, String> {
        let path = core.engine.scratch_file("render")?;
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn ffmpeg(core: &Core, folder: &WorkDir, args: &[OsString]) -> Result<(), String> {
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vendor/studio/ffmpeg.exe");
    let binary = core
        .app
        .path()
        .resource_dir()
        .ok()
        .map(|folder| strip_verbatim(folder).join("studio/ffmpeg.exe"))
        .filter(|path| path.is_file())
        .unwrap_or(local);
    let output = Command::new(binary)
        .current_dir(&folder.0)
        .creation_flags(0x0800_4000)
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-filter_threads",
            "2",
            "-filter_complex_threads",
            "2",
        ])
        .args(args)
        .output()
        .map_err(|e| format!("Couldn't start the editor export tool: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr);
    Err(message.lines().take(3).collect::<Vec<_>>().join(" "))
}

fn push(args: &mut Vec<OsString>, values: &[&str]) {
    args.extend(values.iter().map(OsString::from));
}

fn video_encoder(args: &mut Vec<OsString>) {
    push(
        args,
        &[
            "-c:v",
            "libx264",
            "-threads:v",
            "4",
            "-preset",
            "veryfast",
            "-crf",
            "19",
            "-pix_fmt",
            "yuv420p",
        ],
    );
}

fn finish(core: &Shared, source: &Path, label: &str, rendered: &Path) -> Result<String, String> {
    let clips = core.engine.clips_dir();
    fs::create_dir_all(&clips).map_err(|e| e.to_string())?;
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let stem: String = stem.chars().take(90).collect();
    let scratch = core.engine.scratch_file("edit")?;
    let id = scratch.file_name().unwrap_or_default().to_string_lossy();
    let output = clips.join(format!("{stem}_{label}_{id}.mp4"));
    fs::rename(rendered, &output).map_err(|e| e.to_string())?;
    let _ = core.app.emit("studio-import", 1);
    Ok(output.to_string_lossy().into_owned())
}

fn range(core: &Core, source: &Path, start: f64, end: f64) -> Result<(f64, f64), String> {
    let duration = core.engine.duration(source)?;
    if !start.is_finite()
        || !end.is_finite()
        || start < 0.0
        || end > duration + 0.1
        || end - start < 0.49
    {
        return Err("Choose a valid range of at least half a second.".into());
    }
    Ok((start, end.min(duration)))
}

fn interval(from: f64, to: f64, start: f64, end: f64) -> Result<(f64, f64), String> {
    if !from.is_finite() || !to.is_finite() || from < 0.0 || from >= to {
        return Err("Choose a valid edit start and end time.".into());
    }
    let from = (from - start).max(0.0);
    let to = (to.min(end) - start).max(0.0);
    if from >= to {
        return Err("The edit doesn't overlap your export range.".into());
    }
    Ok((from, to))
}

#[tauri::command(async)]
pub fn render_clip(core: tauri::State<Shared>, payload: Export) -> Result<String, String> {
    let _job = core
        .analysis_lock
        .try_lock()
        .map_err(|_| "Another export or analysis is running.")?;
    let source = own_media(&core, &payload.file)?;
    let (start, end) = range(&core, &source, payload.start, payload.end)?;
    let folder = WorkDir::create(&core)?;
    let mut args = vec![
        "-ss".into(),
        start.to_string().into(),
        "-i".into(),
        source.as_os_str().into(),
    ];
    push(&mut args, &["-t", &(end - start).to_string()]);
    let mut graph = String::new();
    let mut input = "[0:v]";
    if let Some(blur) = payload.blur {
        let [x, y, w, h] = blur.rect;
        if !(x >= 0.0
            && y >= 0.0
            && w >= 0.01
            && h >= 0.01
            && x + w <= 1.000001
            && y + h <= 1.000001)
        {
            return Err("Drag a valid area to blur.".into());
        }
        let (from, to) = interval(blur.from, blur.to, start, end)?;
        graph = format!("[0:v]split[main][crop];[crop]crop=w=ceil(iw*{w}/2)*2:h=ceil(ih*{h}/2)*2:x=trunc(iw*{x}/2)*2:y=trunc(ih*{y}/2)*2,boxblur=luma_radius=min(h\\,w)/12:luma_power=3[blur];[main][blur]overlay=x=trunc(main_w*{x}/2)*2:y=trunc(main_h*{y}/2)*2:enable='between(t,{from},{to})'[clean];");
        input = "[clean]";
    }
    let mut filters = Vec::new();
    if payload.vertical {
        filters.push("crop=w='min(iw,trunc(ih*9/32)*2)':h=ih,scale=720:1280:force_original_aspect_ratio=decrease,pad=720:1280:(ow-iw)/2:(oh-ih)/2,setsar=1");
    }
    if !graph.is_empty() || !filters.is_empty() {
        let chain = if filters.is_empty() {
            "null".into()
        } else {
            filters.join(",")
        };
        graph.push_str(&format!("{input}{chain}[edited]"));
        push(&mut args, &["-filter_complex", &graph, "-map", "[edited]"]);
    } else {
        push(&mut args, &["-map", "0:v:0"]);
    }
    push(
        &mut args,
        &[
            "-map",
            "0:a?",
            "-map_metadata",
            "-1",
            "-map_chapters",
            "-1",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
        ],
    );
    if let Some(audio) = payload.audio {
        let count = core.engine.track_count(&source)? as usize;
        if count == 0 || audio.track.is_some_and(|track| track >= count) {
            return Err("Choose an available audio track.".into());
        }
        let (from, to) = interval(audio.from, audio.to, start, end)?;
        let filter = if audio.bleep {
            format!("aeval=exprs='if(between(t,{from},{to}),0.16*sin(2*PI*950*t),val(ch))':c=same")
        } else {
            format!("volume=volume=0:enable='between(t,{from},{to})'")
        };
        let option = audio
            .track
            .map_or_else(|| "-filter:a".into(), |track| format!("-filter:a:{track}"));
        push(&mut args, &[&option, &filter]);
    }
    video_encoder(&mut args);
    push(&mut args, &["-movflags", "+faststart", "output.mp4"]);
    ffmpeg(&core, &folder, &args)?;
    finish(&core, &source, "redacted", &folder.path("output.mp4"))
}

#[tauri::command(async)]
pub fn export_angles(core: tauri::State<Shared>, payload: AngleExport) -> Result<String, String> {
    let _job = core
        .analysis_lock
        .try_lock()
        .map_err(|_| "Another export or analysis is running.")?;
    let first = own_media(&core, &payload.file)?;
    let second = core
        .angle_file
        .lock()
        .unwrap()
        .clone()
        .ok_or("Choose a second clip.")?;
    let (start, end) = range(&core, &first, payload.start, payload.end)?;
    if !payload.offset.is_finite() || payload.cuts.is_empty() || payload.cuts.len() > 24 {
        return Err("Add between 1 and 24 angle cuts.".into());
    }
    let folder = WorkDir::create(&core)?;
    let second_end = core.engine.duration(&second)?;
    let first_has_audio = core.engine.track_count(&first)? > 0;
    let second_has_audio = core.engine.track_count(&second)? > 0;
    let mut segments = Vec::new();
    let mut cursor = start;
    let mut angle = 1;
    for cut in payload.cuts {
        if !cut.time.is_finite()
            || cut.time < cursor
            || cut.time >= end
            || !matches!(cut.angle, 1 | 2)
        {
            return Err("Keep the cuts in order and inside the export range.".into());
        }
        if cut.time > cursor {
            segments.push((cursor, cut.time, angle));
        }
        cursor = cut.time;
        angle = cut.angle;
    }
    segments.push((cursor, end, angle));
    let mut list = String::new();
    for (index, (from, to, angle)) in segments.into_iter().enumerate() {
        let second_angle = angle == 2;
        let position = if second_angle {
            from + payload.offset
        } else {
            from
        };
        let length = to - from;
        if length < 0.09
            || position < 0.0
            || (second_angle && position + length > second_end + 0.02)
        {
            return Err("A cut is too close to another cut or outside the second clip.".into());
        }
        let path = if second_angle { &second } else { &first };
        let audio = if second_angle {
            second_has_audio
        } else {
            first_has_audio
        };
        let mut args = vec![
            "-ss".into(),
            position.to_string().into(),
            "-i".into(),
            path.as_os_str().into(),
        ];
        if !audio && !payload.continuous_audio {
            push(
                &mut args,
                &["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"],
            );
        }
        push(&mut args, &["-t", &length.to_string(), "-map", "0:v:0", "-vf",
            "scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30"]);
        if payload.continuous_audio {
            push(&mut args, &["-an"]);
        } else {
            let map = if audio { "0:a:0" } else { "1:a:0" };
            push(
                &mut args,
                &[
                    "-map", map, "-af", "apad", "-ar", "48000", "-ac", "2", "-c:a", "aac", "-b:a",
                    "160k",
                ],
            );
        }
        video_encoder(&mut args);
        let segment = format!("angle-{index}.mp4");
        args.push(segment.clone().into());
        ffmpeg(&core, &folder, &args)?;
        list.push_str(&format!("file '{segment}'\n"));
    }
    fs::write(folder.path("angles.txt"), list).map_err(|e| e.to_string())?;
    let mut args = Vec::new();
    push(
        &mut args,
        &["-f", "concat", "-safe", "1", "-i", "angles.txt"],
    );
    if payload.continuous_audio && first_has_audio {
        push(&mut args, &["-ss", &start.to_string(), "-i"]);
        args.push(first.as_os_str().into());
        push(
            &mut args,
            &[
                "-map", "0:v:0", "-map", "1:a:0", "-af", "apad", "-c:a", "aac",
            ],
        );
    } else {
        push(
            &mut args,
            &["-map", "0:v:0", "-map", "0:a?", "-c:a", "copy"],
        );
    }
    push(
        &mut args,
        &[
            "-c:v",
            "copy",
            "-t",
            &(end - start).to_string(),
            "-movflags",
            "+faststart",
            "replay.mp4",
        ],
    );
    ffmpeg(&core, &folder, &args)?;
    finish(&core, &first, "angles", &folder.path("replay.mp4"))
}
