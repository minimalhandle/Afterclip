// Checks the buffer on disk, and moving the buffer from RAM to disk when memory runs low, on a colour card.
//   node src/test/disk-buffer.js
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { ffmpeg, sleep, check, finish, waitFor, startApp } = require('./harness');

const SETTINGS = { width: 1280, height: 720, fps: 30, bitrateKbps: 6000, testPattern: true, audioSources: [], micHotkey: '', markerHotkey: '', presets: [], recordHotkey: '' };

function inspect(file) {
  const decode = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', file, '-f', 'null', '-'], { encoding: 'utf8' });
  const info = spawnSync(ffmpeg, ['-hide_banner', '-i', file], { encoding: 'utf8' }).stderr;
  return { clean: decode.status === 0 && !decode.stderr.trim(), errors: decode.stderr.trim().slice(0, 200), info };
}

const near = (actual, wanted, early = 0.5, late = 2) => actual >= wanted - early && actual <= wanted + late;
const bufferFiles = (output) => {
  const dir = path.join(output, '.staging', 'buffer');
  return fs.existsSync(dir) ? fs.readdirSync(dir) : [];
};

async function onDisk() {
  // Long enough that the disk buffer has finished files of its own, and RAM, at 40 seconds, can't hold it all.
  const session = await startApp({ ...SETTINGS, bufferSeconds: 90, diskBuffer: true });
  const { api, output } = session;
  try {
    // Writing to disk starts a moment after the RAM buffer does.
    const status = await waitFor(async () => { const s = await api.status(); return s.bufferOnDisk && s; }, 10000, 'the buffer on disk').catch(() => ({}));
    check('the buffer is on disk', status.bufferOnDisk === true, JSON.stringify(status.bufferOnDisk));
    console.log('  filling the buffer for 75s');
    await sleep(75000);

    const short = await api.clip(10);
    check('a short clip saves from RAM', near(short.actualSeconds, 10), `${short.actualSeconds.toFixed(2)}s`);
    const long = await api.clip(60);
    const longFile = inspect(long.file);
    check('a long clip saves from disk', near(long.actualSeconds, 60), `${long.actualSeconds.toFixed(2)}s`);
    check('it decodes cleanly across the joins', longFile.clean, longFile.errors);
    check('its audio track is named', /handler_name\s*:\s*Main mix/.test(longFile.info));
    const full = await api.clip(0);
    check('the whole buffer reaches back to the start', full.actualSeconds >= 70 && full.actualSeconds <= 92, `${full.actualSeconds.toFixed(2)}s`);
    check('the whole buffer decodes cleanly', inspect(full.file).clean);

    console.log('  waiting past the buffer length, so old files are deleted');
    // Files are kept until their newest moment is 10 seconds older than the buffer, checked every 5 seconds.
    await sleep(110000);
    const files = bufferFiles(output);
    // 90 seconds in files of up to a minute, plus the one being written.
    check('old files on disk are deleted', files.length >= 2 && files.length <= 4, files.join(', '));
    const after = await api.clip(0);
    check('the whole buffer is still capped at its length', after.actualSeconds <= 92, `${after.actualSeconds.toFixed(2)}s`);
  } catch (e) {
    check('no exceptions on disk', false, e.message);
  } finally {
    await session.stop();
  }
}

async function movedWhenMemoryRunsLow() {
  const session = await startApp({ ...SETTINGS, bufferSeconds: 120 });
  const { api, output } = session;
  try {
    check('the buffer starts in RAM', (await api.status()).bufferOnDisk === false);
    console.log('  filling the buffer in RAM for 50s');
    await sleep(50000);
    const moved = Date.now();
    await api.call('/test/engine/buffer-to-disk', 'POST');
    const status = await waitFor(async () => { const s = await api.status(); return s.bufferOnDisk && s; }, 10000, 'the move to disk');
    check('the buffer moves to disk', status.buffering && status.state === 'ready', `${status.state}`);
    check('what RAM held is kept on disk', bufferFiles(output).some((f) => f.startsWith('Earlier')), bufferFiles(output).join(', '));
    await sleep(15000);
    const elapsed = 50 + (Date.now() - moved) / 1000;
    const full = await api.clip(0);
    const fullFile = inspect(full.file);
    // Nothing lost, and no more than a second or so repeated where RAM's part meets the disk's.
    check('the whole buffer survives the move', near(full.actualSeconds, elapsed, 3, 2), `${full.actualSeconds.toFixed(2)}s of ${elapsed.toFixed(1)}s`);
    check('it decodes cleanly where the two parts meet', fullFile.clean, fullFile.errors);
    const short = await api.clip(5);
    check('short clips still save from RAM', near(short.actualSeconds, 5), `${short.actualSeconds.toFixed(2)}s`);
  } catch (e) {
    check('no exceptions moving to disk', false, e.message);
  } finally {
    await session.stop();
  }
}

async function main() {
  await onDisk();
  await movedWhenMemoryRunsLow();
  finish();
}

main();
