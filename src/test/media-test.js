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
const decodes = (file) => {
  const d = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', file, '-f', 'null', '-'], { encoding: 'utf8' });
  return d.status === 0 && !d.stderr.trim();
};

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

  // Compress: aims for the size at 1080p60, keeps its quality when the size is out of reach, and never grows a small one.
  const compressed = f('compressed.mp4');
  const c = run('compress', noisy, compressed, '100000000');
  check('compress runs', c.status === 0, c.stderr.trim());
  check('compress makes a smaller copy within the size', fs.statSync(compressed).size <= 100000000 && fs.statSync(compressed).size < fs.statSync(noisy).size, `${(fs.statSync(compressed).size / 1e6).toFixed(1)} MB of ${(fs.statSync(noisy).size / 1e6).toFixed(1)} MB`);
  check('compress reports the final size', Number(c.stdout) === fs.statSync(compressed).size, c.stdout.trim());
  check('compress keeps its length', Math.abs(seconds(compressed) - 20) < 0.6, `${seconds(compressed)}s`);
  check('compress keeps 1080p60', /Video: h264.*\b1920x1080\b.*\b60(\.\d+)? fps/.test(spawnSync(ffmpeg, ['-hide_banner', '-i', compressed], { encoding: 'utf8' }).stderr));
  const outOfReach = f('out-of-reach.mp4');
  const o = run('compress', noisy, outOfReach, '5000000');
  check('compress finishes when the size is out of reach', o.status === 0, o.stderr.trim());
  // 20 seconds at the good 4000 kbps of video and 128 of audio is about 10 MB.
  check('it stops at a good quality, not the size', fs.statSync(outOfReach).size > 5000000 && fs.statSync(outOfReach).size < 13000000, `${(fs.statSync(outOfReach).size / 1e6).toFixed(1)} MB`);
  check('it still keeps 1080p60', /Video: h264.*\b1920x1080\b.*\b60(\.\d+)? fps/.test(spawnSync(ffmpeg, ['-hide_banner', '-i', outOfReach], { encoding: 'utf8' }).stderr));
  const big = f('1440p.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=2560x1440:r=60:d=5', '-c:v', 'libx264', '-preset', 'ultrafast', '-b:v', '30M', big]);
  const bigSmall = f('1440p-small.mp4');
  check('compress brings 1440p down', run('compress', big, bigSmall, '100000000').status === 0 && /Video: h264.*\b1920x1080\b/.test(spawnSync(ffmpeg, ['-hide_banner', '-i', bigSmall], { encoding: 'utf8' }).stderr));
  const flat = f('flat.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'color=gray:s=640x360:r=30:d=5', '-c:v', 'libx264', flat]);
  const flatResult = run('compress', flat, f('flat-small.mp4'), '100000000');
  check('compress leaves an already small clip alone', flatResult.status === 4 && /can't get much smaller/.test(flatResult.stderr), flatResult.stderr.trim());

  // Vertical copies: 9:16, from the middle of the picture, with the length and sound kept.
  const vertical = f('vertical.mp4');
  const v = run('vertical', noisy, vertical);
  check('vertical copy runs', v.status === 0, v.stderr.trim());
  const verticalInfo = spawnSync(ffmpeg, ['-hide_banner', '-i', vertical], { encoding: 'utf8' }).stderr;
  check('vertical copy is 9:16 at the original height', /Video: h264.*\b606x1080\b/.test(verticalInfo), /Video:.*/.exec(verticalInfo)?.[0]);
  check('vertical copy keeps the main audio', (verticalInfo.match(/Audio: aac/g) ?? []).length === 1);
  check('vertical copy keeps its length', Math.abs(seconds(vertical) - 20) < 0.6, `${seconds(vertical)}s`);
  const verticalDecode = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', vertical, '-f', 'null', '-'], { encoding: 'utf8' });
  check('vertical copy decodes cleanly', verticalDecode.status === 0 && !verticalDecode.stderr.trim(), verticalDecode.stderr.trim().slice(0, 200));
  const sides = f('sides.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'color=red:s=640x360:r=30:d=1', '-f', 'lavfi', '-i', 'color=lime:s=640x360:r=30:d=1', '-f', 'lavfi', '-i', 'color=blue:s=640x360:r=30:d=1', '-filter_complex', '[0][1][2]hstack=3', '-c:v', 'libx264', sides]);
  const middle = f('middle.mp4');
  run('vertical', sides, middle);
  const pixel = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', middle, '-vf', 'scale=1:1:flags=area', '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'], { encoding: 'buffer' }).stdout;
  check('vertical copy keeps the middle of the picture', pixel[1] > 200 && pixel[0] < 60 && pixel[2] < 60, [...pixel].join());
  // Face cam: the webcam from the corner goes across the top, and the middle of the game fills the rest.
  const sample = (file, x, y) => spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', file, '-vf', `crop=4:4:${x}:${y},scale=1:1:flags=area`, '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'], { encoding: 'buffer' }).stdout;
  const withCam = f('with-cam.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'color=lime:s=1920x1080:r=30:d=2', '-f', 'lavfi', '-i', 'color=magenta:s=480x270:r=30:d=2', '-filter_complex', '[0][1]overlay=1414:784', '-c:v', 'libx264', withCam]);
  const faceCam = f('face-cam.mp4');
  const fc = run('vertical', withCam, faceCam, String(1414 / 1920), String(784 / 1080), String(480 / 1920), String(270 / 1080));
  check('face cam copy runs', fc.status === 0, fc.stderr.trim());
  check('face cam copy is 9:16', /Video: h264.*\b606x1080\b/.test(spawnSync(ffmpeg, ['-hide_banner', '-i', faceCam], { encoding: 'utf8' }).stderr));
  const camPixel = sample(faceCam, 300, 160);
  const gamePixel = sample(faceCam, 300, 700);
  check('face cam copy puts the webcam on top', camPixel[0] > 200 && camPixel[1] < 60 && camPixel[2] > 200, [...camPixel].join());
  check('face cam copy puts the game below', gamePixel[1] > 200 && gamePixel[0] < 60 && gamePixel[2] < 60, [...gamePixel].join());
  check('face cam copy decodes cleanly', decodes(faceCam));
  const badCam = run('vertical', withCam, f('bad-cam.mp4'), '0.9', '0.9', '0.5', '0.5');
  check('a webcam place off the picture makes a plain vertical copy', badCam.status === 0 && sample(f('bad-cam.mp4'), 300, 160)[1] > 200);

  // Hover previews: a row of frames from across the video, in order.
  const strip = f('strip.jpg');
  const st = run('strip', input, strip, '10');
  check('preview strip runs', st.status === 0, st.stderr.trim());
  check('preview strip is ten 160px frames in a row', /mjpeg.*\b1600x90\b/.test(spawnSync(ffmpeg, ['-hide_banner', '-i', strip], { encoding: 'utf8' }).stderr));
  check('preview strip is small', fs.statSync(strip).size < 120 * 1024, `${Math.round(fs.statSync(strip).size / 1024)} KB`);
  const halves = f('halves.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'color=red:s=320x180:r=30:d=4', '-f', 'lavfi', '-i', 'color=blue:s=320x180:r=30:d=4', '-filter_complex', '[0][1]concat=n=2', '-c:v', 'libx264', '-g', '30', halves]);
  run('strip', halves, f('halves.jpg'), '2');
  const [first, second] = [sample(f('halves.jpg'), 70, 40), sample(f('halves.jpg'), 230, 40)];
  check('preview strip frames come from across the video', first[0] > 200 && first[2] < 60 && second[2] > 200 && second[0] < 60, `${[...first].join()} then ${[...second].join()}`);
  check('preview strip of a missing file fails cleanly', run('strip', f('none.mp4'), f('none.jpg'), '10').status === 1);

  const tall = f('tall.mp4');
  execFileSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=360x640:r=30:d=1', '-c:v', 'libx264', tall]);
  const already = run('vertical', tall, f('tall-out.mp4'));
  check('vertical copy refuses a clip that is already vertical', already.status === 4 && /already vertical/.test(already.stderr), already.stderr.trim());

  // Screenshots: the engine hands over raw NV12 in video levels, which must become the same colours.
  const [w, h] = [64, 32];
  const nv12 = Buffer.alloc(w * h * 3 / 2);
  nv12.fill(235, 0, w * h); // Video-level white.
  nv12.fill(128, w * h);
  fs.writeFileSync(f('frame.nv12'), nv12);
  const png = run('png', f('frame.nv12'), f('frame.png'), String(w), String(h));
  check('png runs', png.status === 0, png.stderr.trim());
  const pngInfo = spawnSync(ffmpeg, ['-hide_banner', '-i', f('frame.png')], { encoding: 'utf8' }).stderr;
  check('png is the right size', /png/.test(pngInfo) && /64x32/.test(pngInfo));
  const white = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', f('frame.png'), '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'], { encoding: 'buffer' }).stdout;
  check('png has full-range colours', white[0] >= 253 && white[1] >= 253 && white[2] >= 253, [...white.subarray(0, 3)].join());
  fs.writeFileSync(f('short.nv12'), Buffer.alloc(10));
  check('png refuses a frame of the wrong size', run('png', f('short.nv12'), f('short.png'), String(w), String(h)).status !== 0);
  check('a thumbnail can be made from a png', run('thumb', f('frame.png'), f('frame.jpg')).status === 0 && fs.existsSync(f('frame.jpg')));

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
