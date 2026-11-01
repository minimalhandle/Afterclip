// Stream Deck launches this with: -port <n> -pluginUUID <id> -registerEvent <name> -info <json>
const fs = require('node:fs');
const path = require('node:path');
const { keySvg } = require('./key-art.cjs');

const arg = (name) => process.argv[process.argv.indexOf(`-${name}`) + 1];
const API = process.env.AFTERCLIP_API ?? 'http://127.0.0.1:4467';
// Afterclip 1.0.0 kept this in Roaming, and it's still there until that copy is updated.
const TOKEN_FILES = process.env.AFTERCLIP_TOKEN_FILE ? [process.env.AFTERCLIP_TOKEN_FILE] : ['LOCALAPPDATA', 'APPDATA'].map((folder) => path.join(process.env[folder] ?? '', 'Afterclip', 'api-token.txt'));
const REQUEST_TIMEOUT_MS = 90_000; // Saving a long buffer takes a moment.
const STATUS_TIMEOUT_MS = 1500;
const POLL_MS = 2000;
const DEFAULT_CUSTOM_SECONDS = 30;

const RECORD = 'com.afterclip.clips.record';
const MIC = 'com.afterclip.clips.mic';
const CLEAN = 'com.afterclip.clips.clean-mic';
const MARKER = 'com.afterclip.clips.marker';
const SAVE_FULL = 'com.afterclip.clips.save-full';

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
  if (!res.ok) throw new Error(`Afterclip returned ${res.status}`);
  return res.json();
}

const image = (face) => `data:image/svg+xml;charset=utf8,${encodeURIComponent(keySvg(face))}`;

function lengthFace(seconds) {
  if (seconds === 0) return { big: 'FULL', small: 'BUFFER' };
  if (seconds % 60 === 0) return { big: String(seconds / 60), small: 'MIN' };
  return { big: String(seconds), small: 'SEC' };
}

function faceFor(key) {
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
  if (!key) return;
  const picture = image(faceFor(key));
  // Only sends a change, because this runs every poll and the hardware doesn't need the traffic.
  if (picture !== key.drawn) {
    key.drawn = picture;
    sd('setImage', context, { image: picture });
  }
}

const drawAll = () => { for (const context of keys.keys()) draw(context); };

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
  try {
    if (action === RECORD) {
      await call('POST', '/record/toggle');
    } else if (action === MIC) {
      await call('POST', '/mic/toggle');
    } else if (action === CLEAN) {
      await call('POST', '/mic/clean');
    } else if (action === MARKER) {
      await call('POST', '/marker');
    } else {
      if (key?.saving) return; // A second press mid-save would save the same moment twice.
      if (key) { key.saving = true; draw(context); }
      await call('POST', `/clip/${secondsFor(action, settings)}`);
    }
    sd('showOk', context);
  } catch {
    sd('showAlert', context);
  } finally {
    if (key) key.saving = false;
    await poll();
    draw(context);
  }
}

ws.addEventListener('open', () => {
  ws.send(JSON.stringify({ event: arg('registerEvent'), uuid: arg('pluginUUID') }));
  setInterval(poll, POLL_MS);
});

ws.addEventListener('message', ({ data }) => {
  const { event, action, context, payload } = JSON.parse(data);
  if (event === 'willAppear' || event === 'didReceiveSettings') {
    const existing = keys.get(context);
    keys.set(context, { action, seconds: secondsFor(action, payload.settings), saving: existing?.saving ?? false, drawn: existing?.drawn });
    draw(context);
    if (!existing) poll();
  } else if (event === 'willDisappear') {
    keys.delete(context);
  } else if (event === 'keyDown') {
    press(context, action, payload.settings);
  }
});

ws.addEventListener('close', () => process.exit(0));
