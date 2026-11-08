use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::os::windows::process::CommandExt;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Layout {
    pub crop: f64,
    #[serde(default)]
    pub pins: Vec<[f64; 4]>,
}

impl Layout {
    pub fn valid(&self) -> bool {
        (0.0..=1.0).contains(&self.crop) && self.pins.len() <= 2 && self.pins.iter().all(|[x,y,w,h]|
            *x >= 0.0 && *y >= 0.0 && *w >= 0.01 && *h >= 0.01 && x + w <= 1.000001 && y + h <= 1.000001)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    pub name: String,
    pub action: String,
    pub channel: Option<String>,
    pub tracks: Vec<usize>,
    pub even: bool,
    pub max_mb: Option<u64>,
    pub layout: Option<Layout>,
}

impl Recipe {
    pub fn valid(&self) -> bool {
        !self.name.trim().is_empty() && self.name.chars().count() <= 48
            && matches!(self.action.as_str(), "compress" | "vertical" | "discord" | "vertical-discord")
            && self.tracks.len() <= 6 && self.tracks.iter().all(|track| *track < 6)
            && self.max_mb.is_none_or(|mb| (1..=250).contains(&mb))
            && self.layout.as_ref().is_none_or(Layout::valid)
    }
}

#[derive(Deserialize, Serialize)]
pub struct SpeechLine {
    pub start: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
}

pub fn transcript_path(folder: &Path, clip: &Path) -> Result<PathBuf, String> {
    let meta = fs::metadata(clip).map_err(|e| e.to_string())?;
    let mut key = DefaultHasher::new();
    clip.hash(&mut key);
    meta.len().hash(&mut key);
    meta.modified().ok().hash(&mut key);
    Ok(folder.join(format!("{:016x}.json", key.finish())))
}

pub fn read_transcript(path: &Path) -> Vec<SpeechLine> {
    fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

pub fn transcribe(runtime: &Path, wav: &Path) -> Result<Vec<SpeechLine>, String> {
    let model = runtime.join("ggml-tiny.bin");
    let binary = runtime.join("whisper-cli.exe");
    if !model.is_file() || !binary.is_file() {
        return Err("The offline speech model is missing from this installation.".into());
    }
    let result = Command::new(binary)
        .args(["-m"]).arg(model).args(["-f"]).arg(wav)
        .args(["-l", "auto", "-t", "4", "-ng", "-oj", "-of"]).arg(wav)
        .current_dir(runtime).creation_flags(0x0800_4000).output()
        .map_err(|e| format!("Couldn't start offline transcription: {e}"))?;
    let mut json = wav.as_os_str().to_os_string();
    json.push(".json");
    let result_file = PathBuf::from(json);
    let bytes = fs::read(&result_file);
    let _ = fs::remove_file(&result_file);
    if !result.status.success() {
        return Err("Offline transcription failed. Try a shorter clip or a different audio track.".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let segments = value["transcription"].as_array().ok_or("No transcription was returned.")?;
    Ok(segments.iter().filter_map(|part| {
        let start = part["offsets"]["from"].as_f64()? / 1000.0;
        let text = part["text"].as_str()?.trim().to_string();
        let end = part["offsets"]["to"].as_f64().map(|time| time / 1000.0);
        (!text.is_empty() && !text.starts_with('[')).then_some(SpeechLine { start, text, end })
    }).collect())
}

// Audio features at 50 Hz. Comparing changes in level reduces sensitivity to
// overall volume differences between the two recordings.
fn envelope(wav: &Path) -> Result<Vec<f32>, String> {
    let bytes = fs::read(wav).map_err(|e| e.to_string())?;
    let pcm = bytes.get(44..).ok_or("No audio samples.")?;
    let level: Vec<_> = pcm.chunks_exact(640).map(|block| {
        let energy: f32 = block.chunks_exact(2).map(|x| (i16::from_le_bytes([x[0],x[1]]) as f32 / 32768.0).powi(2)).sum();
        (0.00001 + energy / 320.0).ln()
    }).collect();
    Ok(level.windows(2).map(|p| p[1] - p[0]).collect())
}

fn correlation(a: &[f32], b: &[f32], shift: i32, stride: usize) -> f32 {
    let first = (-shift).max(0) as usize;
    let other = shift.max(0) as usize;
    let overlap = a.len().saturating_sub(first).min(b.len().saturating_sub(other));
    if overlap < 150 { return -1.0; }
    let (mut cross, mut aa, mut bb) = (0.0,0.0,0.0);
    for i in (0..overlap).step_by(stride) {
        let (x,y) = (a[first+i], b[other+i]);
        cross += x*y;
        aa += x*x;
        bb += y*y;
    }
    if aa < 0.01 || bb < 0.01 { return -1.0; }
    cross / (aa*bb).sqrt()
}

fn best_offset(a: &[f32], b: &[f32]) -> (i32, f32) {
    let candidates = (-6000..=6000).map(|shift| (shift, correlation(a,b,shift,10)));
    let mut best: Vec<_> = candidates.collect();
    best.sort_unstable_by(|a,b| b.1.total_cmp(&a.1));
    best.into_iter().take(24)
        .map(|(shift,_)| (shift, correlation(a,b,shift,1)))
        .max_by(|a,b| a.1.total_cmp(&b.1)).unwrap_or((0,-1.0))
}

pub fn sync_audio(a: &Path, b: &Path) -> Result<(f64, f32), String> {
    let a = envelope(a)?;
    let b = envelope(b)?;
    let (shift, score) = best_offset(&a, &b);
    if score < 0.32 {
        return Err("No reliable audio match. Set the offset by matching the same moment in both clips.".into());
    }
    Ok((shift as f64 / 50.0, score))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_an_offset_with_different_volume() {
        let a: Vec<_> = (0..650).map(|i| ((i * i * 313 + i * 29) % 971) as f32 - 480.0).collect();
        let mut b = vec![0.0; 37];
        b.extend(a.iter().map(|x| x * 0.3));
        let (shift, score) = best_offset(&a, &b);
        assert_eq!(shift, 37);
        assert!(score > 0.99);
    }
}
