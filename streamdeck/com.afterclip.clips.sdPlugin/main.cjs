// Stream Deck launches this with: -port <n> -pluginUUID <id> -registerEvent <name> -info <json>
const fs = require('node:fs');
const path = require('node:path');
const { keySvg, confirmSvg } = require('./key-art.cjs');

const arg = (name) => process.argv[process.argv.indexOf(`-${name}`) + 1];
const API = process.env.AFTERCLIP_API ?? 'http://127.0.0.1:4467';
// Afterclip 1.0.0 kept this in Roaming, and it's still there until that copy is updated.
const TOKEN_FILES = process.env.AFTERCLIP_TOKEN_FILE ? [process.env.AFTERCLIP_TOKEN_FILE] : ['LOCALAPPDATA', 'APPDATA'].map((folder) => path.join(process.env[folder] ?? '', 'Afterclip', 'api-token.txt'));
const REQUEST_TIMEOUT_MS = 90_000; // Saving a long buffer takes a moment.
const STATUS_TIMEOUT_MS = 1500;
const POLL_MS = 2000;
const DEFAULT_CUSTOM_SECONDS = 30;
// The saved animation. Fast enough to read as motion, without flooding the hardware.
const CONFIRM_FRAMES = 9;
const CONFIRM_FRAME_MS = 70;
const CONFIRM_HOLD_MS = 650;
const PULSE_MS = 220;
const FAIL_HOLD_MS = 1200;

const RECORD = 'com.afterclip.clips.record';
const MIC = 'com.afterclip.clips.mic';
const CLEAN = 'com.afterclip.clips.clean-mic';
const MARKER = 'com.afterclip.clips.marker';
const SAVE_FULL = 'com.afterclip.clips.save-full';
const OPEN_PANEL = 'com.afterclip.clips.open-panel';
const OPEN_CLIPS = 'com.afterclip.clips.open-clips';
const EDIT_LAST = 'com.afterclip.clips.edit-last';
const COPY_LAST = 'com.afterclip.clips.copy-last';
// Keys that do one thing and don't depend on the capture state.
const SIMPLE_KEYS = {
  [OPEN_PANEL]: { route: '/open/panel', symbol: 'panel', small: 'OPEN' },
  [OPEN_CLIPS]: { route: '/open/clips', symbol: 'folder', small: 'CLIPS' },
  [EDIT_LAST]: { route: '/last-clip/edit', symbol: 'edit', small: 'EDIT LAST' },
  [COPY_LAST]: { route: '/last-clip/copy', symbol: 'copy', small: 'COPY LAST', confirm: 'COPIED' },
};
const sleep = (ms) => new Promise((resolve) => { setTimeout(resolve, ms); });

/** The length built into an action's UUID, such as save-15-sec, or null if it doesn't have one. */
function fixedSeconds(action) {
  if (action === SAVE_FULL) return 0;
  const m = /\.save-(\d+)-(sec|min)$/.exec(action);
  return m ? Number(m[1]) * (m[2] === 'min' ? 60 : 1) : null;
}

const keys = new Map(); // context -> { action, seconds, saving, drawn }
let status = null; // The last answer from Afterclip, or null when it isn't running.
const ws = new WebSocket(`ws://127.0.0.1:${arg('port')}`);
const sd = (event, context, payload) => ws.send(JSON.stringify({ event, context, payload }));

async function call(method, route, timeout = REQUEST_TIMEOUT_MS) {
  const tokenFile = TOKEN_FILES.find((file) => fs.existsSync(file)) ?? TOKEN_FILES[0];
  const token = fs.readFileSync(tokenFile, 'utf8').trim(); // Read each time: Afterclip may have just created it.
  const res = await fetch(API + route, { method, headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(timeout) });
  if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error ?? `Afterclip returned ${res.status}`);
  return res.json();
}

const image = (face) => `data:image/svg+xml;charset=utf8,${encodeURIComponent(keySvg(face))}`;

function lengthFace(seconds) {
  if (seconds === 0) return { big: 'FULL', small: 'BUFFER' };
  if (seconds % 60 === 0) return { big: String(seconds / 60), small: 'MIN' };
  return { big: String(seconds), small: 'SEC' };
}

function faceFor(key) {
  const simple = SIMPLE_KEYS[key.action];
  if (simple) return { symbol: simple.symbol, small: status ? simple.small : 'OFF', tone: status ? 'normal' : 'off' };
  if (key.action === RECORD) {
    if (!status) return { symbol: 'record', small: 'OFF', tone: 'off' };
    return status.recording ? { symbol: 'stop', small: 'STOP', tone: 'active' } : { symbol: 'record', small: 'REC' };
  }
  if (key.action === MIC) {
    if (!status) return { symbol: 'mic', small: 'OFF', tone: 'off' };
    return status.micMuted ? { symbol: 'micMuted', small: 'MUTED', tone: 'active' } : { symbol: 'mic', small: 'MIC ON' };
  }
  if (key.action === CLEAN) {
    if (!status) return { symbol: 'clean', small: 'OFF', tone: 'off' };
    return status.cleanMic ? { symbol: 'clean', small: 'CLEAN', tone: 'active' } : { symbol: 'clean', small: 'CLEAN MIC' };
  }
  if (key.action === MARKER) {
    // Markers only exist inside a recording, so the key is dim until one is running.
    return status?.recording ? { symbol: 'marker', small: 'MARKER' } : { symbol: 'marker', small: status ? 'MARKER' : 'OFF', tone: 'off' };
  }
  const face = lengthFace(key.seconds);
  if (key.saving) return { ...face, small: 'SAVING', tone: 'active' };
  if (!status?.buffering) return { ...face, small: status ? face.small : 'OFF', tone: 'off' };
  return face;
}

function draw(context) {
  const key = keys.get(context);
  if (!key || key.playing) return;
  const picture = image(faceFor(key));
  // Only sends a change, because this runs every poll and the hardware doesn't need the traffic.
  if (picture !== key.drawn) {
    key.drawn = picture;
    sd('setImage', context, { image: picture });
  }
}

const drawAll = () => { for (const context of keys.keys()) draw(context); };

/** Shows a run of pictures on a key, then returns it to its live face. A newer run replaces this one. */
async function play(context, pictures) {
  const key = keys.get(context);
  if (!key) return;
  key.run = (key.run ?? 0) + 1;
  const run = key.run;
  key.playing = true;
  for (const [picture, ms] of pictures) {
    if (keys.get(context) !== key || key.run !== run) return;
    sd('setImage', context, { image: picture });
    await sleep(ms);
  }
  if (key.run !== run) return;
  key.playing = false;
  key.drawn = null;
  draw(context);
}

const confirmed = (context, label) => play(context, [...Array(CONFIRM_FRAMES).keys()].map((i) => {
  const last = i === CONFIRM_FRAMES - 1;
  const picture = `data:image/svg+xml;charset=utf8,${encodeURIComponent(confirmSvg(label, i / (CONFIRM_FRAMES - 1), i === 0 ? 'confirmed' : undefined))}`;
  return [picture, last ? CONFIRM_HOLD_MS : CONFIRM_FRAME_MS];
}));

/** A toggle's new state shows on the key itself, so it only needs a brief lit moment. */
const pulsed = (context) => {
  const key = keys.get(context);
  if (key) play(context, [[image({ ...faceFor(key), tone: 'active', mark: 'pulsed' }), PULSE_MS]]);
};

function failed(context, action, reason) {
  const small = !status ? 'NOT OPEN' : action === MARKER && !status.recording ? 'NO REC' : reason;
  play(context, [[image({ symbol: 'alert', small, tone: 'fail', mark: 'failed' }), FAIL_HOLD_MS]]);
}

async function poll() {
  if (keys.size === 0) return;
  status = await call('GET', '/status', STATUS_TIMEOUT_MS).catch(() => null);
  drawAll();
}

const customSeconds = (settings) => {
  const n = Number(settings?.seconds);
  return Number.isInteger(n) && n >= 0 ? n : DEFAULT_CUSTOM_SECONDS;
};
const secondsFor = (action, settings) => fixedSeconds(action) ?? customSeconds(settings);

async function press(context, action, settings) {
  const key = keys.get(context);
  const simple = SIMPLE_KEYS[action];
  let finish;
  try {
    if (action === RECORD || action === MIC || action === CLEAN) {
      await call('POST', { [RECORD]: '/record/toggle', [MIC]: '/mic/toggle', [CLEAN]: '/mic/clean' }[action]);
      finish = () => pulsed(context);
    } else if (action === MARKER) {
      await call('POST', '/marker');
      finish = () => confirmed(context, 'MARKED');
    } else if (simple) {
      await call('POST', simple.route);
      // Opening a window is its own confirmation.
      finish = simple.confirm ? () => confirmed(context, simple.confirm) : () => pulsed(context);
    } else {
      if (key?.saving) return; // A second press mid-save would save the same moment twice.
      if (key) {
        key.saving = true;
        key.run = (key.run ?? 0) + 1;
        key.playing = false;
        draw(context);
      }
      await call('POST', `/clip/${secondsFor(action, settings)}`);
      finish = () => confirmed(context, 'SAVED');
    }
  } catch (error) {
    await poll();
    finish = () => failed(context, action, /No clips/.test(error.message) ? 'NO CLIPS' : 'FAILED');
  }
  if (key) key.saving = false;
  await poll();
  finish();
}

ws.addEventListener('open', () => {
  ws.send(JSON.stringify({ event: arg('registerEvent'), uuid: arg('pluginUUID') }));
  setInterval(poll, POLL_MS);
});

ws.addEventListener('message', ({ data }) => {
  const { event, action, context, payload } = JSON.parse(data);
  if (event === 'willAppear' || event === 'didReceiveSettings') {
    const existing = keys.get(context);
    if (existing) existing.seconds = secondsFor(action, payload.settings);
    else keys.set(context, { action, seconds: secondsFor(action, payload.settings), saving: false, playing: false, drawn: undefined });
    draw(context);
    if (!existing) poll();
  } else if (event === 'willDisappear') {
    keys.delete(context);
  } else if (event === 'keyDown') {
    press(context, action, payload.settings);
  }
});

ws.addEventListener('close', () => process.exit(0));
