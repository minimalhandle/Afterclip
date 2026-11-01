// Checks afterclip-media.exe against ffmpeg, which is only a dev dependency used to make and verify test files.
//   node src/test/media-test.js
const { execFileSync, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const root = path.resolve(__dirname, '..', '..');
const ffmpeg = require(path.join(root, 'node_modules', 'ffmpeg-static'));
const media = path.join(root, 'vendor', 'engine', 'bin', '64bit', 'afterclip-media.exe');
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'afterclip-media-'));
const failures = [];
const check = (name, ok, detail = '') => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name} ${detail}`);
  if (!ok) failures.push(name);
};
const run = (...args) => spawnSync(media, args, { encoding: 'utf8', windowsHide: true });
const f = (name) => path.join(dir, name);

try {
  // Same shape as a real replay: 60 fps, 1s keyframes, B-frames, and audio.
  const input = f('in.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=1280x720:r=60:d=30', '-f', 'lavfi', '-i', 'sine=f=440:d=30', '-c:v', 'libx264', '-g', '60', '-bf', '2', '-c:a', 'aac', input]);
  const seconds = (file) => Number(run('duration', file).stdout);
  check('duration', Math.abs(seconds(input) - 30) < 0.2, `${seconds(input)}s`);

  for (const want of [5, 10, 29]) {
    const out = f(`t${want}.mp4`);
    const r = run('trim', input, out, String(want));
    check(`trim ${want}s runs`, r.status === 0, r.stderr.trim());
    const got = seconds(out);
    check(`trim ${want}s length`, got >= want - 0.1 && got <= want + 1.2, `${got}s`);
    const decode = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', out, '-f', 'null', '-'], { encoding: 'utf8' });
    check(`trim ${want}s decodes cleanly`, decode.status === 0 && !decode.stderr.trim(), decode.stderr.trim().slice(0, 200));
    const info = spawnSync(ffmpeg, ['-hide_banner', '-i', out], { encoding: 'utf8' }).stderr;
    check(`trim ${want}s keeps video and audio`, /Video: h264/.test(info) && /Audio: aac/.test(info));
    check(`trim ${want}s moov at the front`, fs.readFileSync(out).subarray(0, 4096).includes('moov'));
  }

  // Share-ready copies: must fit, play, and keep their length.
  const noisy = f('noisy.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=1920x1080:r=60:d=20', '-f', 'lavfi', '-i', 'anoisesrc=d=20:a=0.3', '-f', 'lavfi', '-i', 'sine=f=880:d=20', '-map', '0:v', '-map', '1:a', '-map', '2:a', '-c:v', 'libx264', '-b:v', '20M', '-g', '60', '-bf', '2', '-c:a', 'aac', noisy]);
  const audioCount = (file) => Number(run('tracks', file).stdout);
  check('tracks counts audio tracks', audioCount(noisy) === 2, `${audioCount(noisy)}`);
  for (const mb of [10, 25]) {
    const small = f(`small${mb}.mp4`);
    const r = run('shrink', noisy, small, String(mb * 1000 * 1000));
    check(`shrink to ${mb} MB runs`, r.status === 0, r.stderr.trim());
    const size = fs.statSync(small).size;
    check(`shrink to ${mb} MB fits`, size <= mb * 1000 * 1000 && size > mb * 1000 * 1000 * 0.2, `${(size / 1e6).toFixed(1)} MB`);
    check(`shrink to ${mb} MB keeps its length`, Math.abs(seconds(small) - 20) < 0.6, `${seconds(small)}s`);
    const decode = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', small, '-f', 'null', '-'], { encoding: 'utf8' });
    check(`shrink to ${mb} MB decodes cleanly`, decode.status === 0 && !decode.stderr.trim(), decode.stderr.trim().slice(0, 200));
    const info = spawnSync(ffmpeg, ['-hide_banner', '-i', small], { encoding: 'utf8' }).stderr;
    check(`shrink to ${mb} MB is H.264 with one audio track`, /Video: h264/.test(info) && (info.match(/Audio: aac/g) ?? []).length === 1);
  }
  const tiny = run('shrink', noisy, f('tiny.mp4'), '100000');
  check('shrink refuses a size too small to watch', tiny.status === 4 && /too long/.test(tiny.stderr), tiny.stderr.trim());

  // Track mixing: a track can be removed, or turned up or down, with the video copied untouched.
  const volume = (file, track) => Number(/mean_volume: (-?[\d.]+)/.exec(spawnSync(ffmpeg, ['-hide_banner', '-i', file, '-map', `0:a:${track}`, '-af', 'volumedetect', '-f', 'null', '-'], { encoding: 'utf8' }).stderr)?.[1]);
  const mixed = f('mixed.mp4');
  const mix = run('retrack', noisy, mixed, '1', '0.5');
  check('retrack runs', mix.status === 0, mix.stderr.trim());
  check('retrack keeps both tracks', audioCount(mixed) === 2);
  check('retrack leaves a track at 100% alone', Math.abs(volume(mixed, 0) - volume(noisy, 0)) < 0.2, `${volume(noisy, 0)} to ${volume(mixed, 0)} dB`);
  const drop = volume(noisy, 1) - volume(mixed, 1);
  check('retrack at 50% lowers a track by about 6 dB', drop > 5 && drop < 7, `${drop.toFixed(1)} dB`);
  check('retrack keeps the length', Math.abs(seconds(mixed) - seconds(noisy)) < 0.2, `${seconds(mixed)}s`);
  const videoHash = (file) => spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', file, '-map', '0:v', '-c', 'copy', '-f', 'md5', '-'], { encoding: 'utf8' }).stdout.trim();
  check('retrack copies the video losslessly', videoHash(mixed) === videoHash(noisy));
  const removed = f('removed.mp4');
  check('retrack can remove a track', run('retrack', noisy, removed, '1', '0').status === 0 && audioCount(removed) === 1);
  const mixedDecode = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', mixed, '-f', 'null', '-'], { encoding: 'utf8' });
  check('retrack decodes cleanly', mixedDecode.status === 0 && !mixedDecode.stderr.trim(), mixedDecode.stderr.trim().slice(0, 200));

  const thumb = f('t.jpg');
  const t = run('thumb', input, thumb);
  check('thumbnail runs', t.status === 0, t.stderr.trim());
  const info = spawnSync(ffmpeg, ['-hide_banner', '-i', thumb], { encoding: 'utf8' }).stderr;
  check('thumbnail is a 320px JPEG', /mjpeg/.test(info) && /320x180/.test(info));
  check('thumbnail is small', fs.statSync(thumb).size < 60 * 1024, `${Math.round(fs.statSync(thumb).size / 1024)} KB`);

  const short = f('short.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=640x360:r=30:d=0.5', short]);
  check('thumbnail of a very short clip', run('thumb', short, f('s.jpg')).status === 0 && fs.existsSync(f('s.jpg')));

  const unicode = f('clip ünï 日本.mp4');
  fs.copyFileSync(input, unicode);
  check('non-English file names', run('trim', unicode, f('out ünï 日本.mp4'), '5').status === 0 && fs.existsSync(f('out ünï 日本.mp4')));
  check('non-English thumbnail name', run('thumb', unicode, f('thumb ünï.jpg')).status === 0 && fs.existsSync(f('thumb ünï.jpg')));

  check('missing file fails cleanly', run('thumb', f('none.mp4'), f('x.jpg')).status === 1);

  const decodesCleanly = (file) => {
    const d = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', file, '-f', 'null', '-'], { encoding: 'utf8' });
    return d.status === 0 && !d.stderr.trim();
  };
  // Editor trim: a start snaps back to a keyframe (1s apart here), and an end stays close to exact.
  const edit = f('edit.mp4');
  const e = run('join', edit, input, '10.5', '15.5');
  check('editor trim runs', e.status === 0, e.stderr.trim());
  check('editor trim length', seconds(edit) >= 5 && seconds(edit) <= 6.3, `${seconds(edit)}s`);
  check('editor trim decodes cleanly', decodesCleanly(edit));

  const other = f('other.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=1280x720:r=60:d=30', '-f', 'lavfi', '-i', 'sine=f=880:d=30', '-c:v', 'libx264', '-g', '60', '-bf', '2', '-c:a', 'aac', other]);
  const merged = f('merged.mp4');
  const j = run('join', merged, input, '2', '7', other, '0', '4', input, '20', '-1');
  check('merge runs', j.status === 0, j.stderr.trim());
  check('merge length', seconds(merged) >= 19 && seconds(merged) <= 21.5, `${seconds(merged)}s`);
  check('merge decodes cleanly', decodesCleanly(merged));
  const mergedInfo = spawnSync(ffmpeg, ['-hide_banner', '-i', merged], { encoding: 'utf8' }).stderr;
  check('merge keeps video and audio', /Video: h264/.test(mergedInfo) && /Audio: aac/.test(mergedInfo));

  const small = f('small.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=640x360:r=30:d=4', '-f', 'lavfi', '-i', 'sine=f=440:d=4', '-c:v', 'libx264', '-c:a', 'aac', small]);
  const refused = run('join', f('bad.mp4'), input, '0', '3', small, '0', '3');
  check('refuses clips that cannot merge', refused.status === 3 && /can't be merged/.test(refused.stderr), refused.stderr.trim());

  const tracks = f('tracks.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=640x360:r=30:d=8', '-f', 'lavfi', '-i', 'sine=f=440:d=8', '-f', 'lavfi', '-i', 'sine=f=660:d=8', '-map', '0', '-map', '1', '-map', '2', '-c:v', 'libx264', '-g', '30', '-c:a', 'aac', tracks]);
  const tracksOut = f('tracks-out.mp4');
  run('join', tracksOut, tracks, '1', '5');
  const trackCount = (spawnSync(ffmpeg, ['-hide_banner', '-i', tracksOut], { encoding: 'utf8' }).stderr.match(/Audio: aac/g) ?? []).length;
  check('editing keeps separate audio tracks', trackCount === 2, `${trackCount}`);
} catch (e) {
  check('no exceptions', false, e.message);
} finally {
  fs.rmSync(dir, { recursive: true, force: true });
}
console.log(failures.length ? `\n${failures.length} FAILED: ${failures.join('; ')}` : '\nALL PASSED');
process.exit(failures.length ? 1 : 0);
