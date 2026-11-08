// Stream Deck launches this with: -port <n> -pluginUUID <id> -registerEvent <name> -info <json>
const fs = require('node:fs');
const path = require('node:path');
const { keySvg, confirmSvg, COLOURS } = require('./key-art.cjs');


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
// Compressing or sending a long clip can take minutes.
const LONG_JOB_MS = 15 * 60_000;
const VOLUME_STEP = 5;
const DIAL_LENGTHS = [15, 30, 60, 120, 300, 600, 0];

const RECORD = 'com.afterclip.clips.record';
const MIC = 'com.afterclip.clips.mic';
const CLEAN = 'com.afterclip.clips.clean-mic';
const MARKER = 'com.afterclip.clips.marker';
const SAVE_FULL = 'com.afterclip.clips.save-full';
const OPEN_PANEL = 'com.afterclip.clips.open-panel';
const OPEN_CLIPS = 'com.afterclip.clips.open-clips';
const EDIT_LAST = 'com.afterclip.clips.edit-last';
const COPY_LAST = 'com.afterclip.clips.copy-last';
const SCREENSHOT = 'com.afterclip.clips.screenshot';
const COMPRESS_LAST = 'com.afterclip.clips.compress-last';
const VERTICAL_LAST = 'com.afterclip.clips.vertical-last';
const STAR_LAST = 'com.afterclip.clips.star-last';
// Saves a clip of its chosen length and stars it.
const SAVE_STAR = 'com.afterclip.clips.save-star';
const WEBCAM = 'com.afterclip.clips.webcam';
// Held rather than pressed: the mic is in clips while it's down.
const TALK = 'com.afterclip.clips.talk';
const DISCORD_LAST = 'com.afterclip.clips.discord-last';
// Stream Deck + dials.
const MIC_DIAL = 'com.afterclip.clips.mic-dial';
const CLIP_DIAL = 'com.afterclip.clips.clip-dial';
const DIALS = new Set([MIC_DIAL, CLIP_DIAL]);
// Keys that do one thing and don't depend on the capture state.
const SIMPLE_KEYS = {
  [OPEN_PANEL]: { route: '/open/panel', symbol: 'panel', small: 'OPEN' },
  [OPEN_CLIPS]: { route: '/open/clips', symbol: 'folder', small: 'CLIPS' },
  [EDIT_LAST]: { route: '/last-clip/edit', symbol: 'edit', small: 'EDIT LAST' },
  [COPY_LAST]: { route: '/last-clip/copy', symbol: 'copy', small: 'COPY LAST', confirm: 'COPIED' },
  [SCREENSHOT]: { route: '/screenshot', symbol: 'camera', small: 'SHOT', confirm: 'SAVED' },
  // These run until the job is done, and show it on the key meanwhile.
  [COMPRESS_LAST]: { route: '/last-clip/compress', symbol: 'compress', small: 'COMPRESS', confirm: 'READY', busy: 'WORKING', timeout: LONG_JOB_MS },
  [DISCORD_LAST]: { route: '/last-clip/discord', symbol: 'send', small: 'DISCORD', confirm: 'SENT', busy: 'SENDING', timeout: LONG_JOB_MS },
  [VERTICAL_LAST]: { route: '/last-clip/vertical', symbol: 'vertical', small: 'VERTICAL', confirm: 'READY', busy: 'WORKING', timeout: LONG_JOB_MS },
  // Starring again takes the star off, so the key says which happened.
  [STAR_LAST]: { route: '/last-clip/star', symbol: 'star', small: 'STAR LAST', confirm: (reply) => (reply.favourite ? 'STARRED' : 'UNSTAR') },
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

/**
 * Where Afterclip is. With no address, it's on this PC, and the key is read from its folder. With the address
 * from Afterclip's Settings, such as http://192.168.1.20:4467/?k=..., it can be another PC on the network.
 */
let remote = null; // { base, key, host } while an address is set
let link = 'local'; // local, ok, offline, key or bad: how the last request went, for the settings page
const inspectors = new Set(); // Settings pages open now, which are told how the connection is going.

function parseAddress(text) {
  const typed = String(text ?? '').trim();
  if (!typed) return null;
  try {
    const url = new URL(/^[a-z]+:\/\//i.test(typed) ? typed : `http://${typed}`);
    const key = url.searchParams.get('k') ?? '';
    return { base: `${url.protocol}//${url.host}`, key, host: url.hostname };
  } catch {
    return { base: '', key: '', host: typed };
  }
}

function tellInspectors() {
  for (const context of inspectors) sd('sendToPropertyInspector', context, { link, host: remote?.host ?? null });
}

function setLink(next) {
  if (next === link) return;
  link = next;
  tellInspectors();
}

async function call(method, route, timeout = REQUEST_TIMEOUT_MS) {
  let base = API;
  let token;
  if (remote) {
    if (!remote.base || !remote.key) {
      setLink('bad');
      throw new Error('The address has no key');
    }
    ({ base } = remote);
    token = remote.key;
  } else {
    const tokenFile = TOKEN_FILES.find((file) => fs.existsSync(file)) ?? TOKEN_FILES[0];
    token = fs.readFileSync(tokenFile, 'utf8').trim(); // Read each time: Afterclip may have just created it.
  }
  let res;
  try {
    res = await fetch(base + route, { method, headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(timeout) });
  } catch (error) {
    setLink('offline');
    throw error;
  }
  if (res.status === 401) setLink('key');
  else setLink(remote ? 'ok' : 'local');
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
  if (simple && key.saving) return { symbol: simple.symbol, small: simple.busy, tone: 'active' };
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
  if (key.action === WEBCAM) {
    if (!status) return { symbol: 'webcam', small: 'OFF', tone: 'off' };
    if (!status.cameraChosen) return { symbol: 'webcam', small: 'NO CAM', tone: 'off' };
    return status.cameraOn ? { symbol: 'webcam', small: 'CAM ON', tone: 'active' } : { symbol: 'webcamOff', small: 'CAM OFF' };
  }
  if (key.action === TALK) {
    if (!status) return { symbol: 'mic', small: 'OFF', tone: 'off' };
    if (!status.pushToTalk) return { symbol: 'mic', small: 'PTT OFF', tone: 'off' };
    return key.holding ? { symbol: 'mic', small: 'TALKING', tone: 'active' } : { symbol: 'mic', small: 'HOLD' };
  }
  if (key.action === MARKER) {
    // Markers only exist inside a recording, so the key is dim until one is running.
    return status?.recording ? { symbol: 'marker', small: 'MARKER' } : { symbol: 'marker', small: status ? 'MARKER' : 'OFF', tone: 'off' };
  }
  const length = lengthFace(key.seconds);
  // Save and Star looks like a length key, marked so it can't be mistaken for one.
  const face = key.action === SAVE_STAR ? { ...length, small: `${length.small} STAR` } : length;
  if (key.saving) return { ...face, small: 'SAVING', tone: 'active' };
  if (!status?.buffering) return { ...face, small: status ? face.small : 'OFF', tone: 'off' };
  return face;
}

const lengthText = (seconds) => (seconds === 0 ? 'Whole buffer' : seconds % 60 === 0 ? `Last ${seconds / 60} min` : `Last ${seconds} sec`);

/** What a dial's screen shows: a title, a value, and a bar in Afterclip's purple. */
function dialFace(key) {
  const bar = (value, colour = COLOURS.ring) => ({ value: Math.round(Math.min(100, Math.max(0, value))), bar_fill_c: colour });
  if (key.action === MIC_DIAL) {
    const volume = status?.micVolume ?? 100;
    const face = !status ? { symbol: 'mic', small: 'OFF', tone: 'off' } : status.micMuted ? { symbol: 'micMuted', small: 'MUTED', tone: 'active' } : { symbol: 'mic', small: `${volume}%` };
    const value = key.note ?? (!status ? 'Not open' : status.micMuted ? 'Muted' : `${volume}%`);
    return { title: 'Mic in clips', value, icon: image(face), indicator: bar(status && !status.micMuted ? volume / 2 : 0, key.note ? COLOURS.fail : COLOURS.ring) };
  }
  const seconds = DIAL_LENGTHS[key.lengthIndex];
  const filled = status?.buffering && status.bufferingSince && status.bufferSeconds ? ((Date.now() - status.bufferingSince) / 1000 / status.bufferSeconds) * 100 : 0;
  const value = key.note ?? (key.saving ? 'Saving' : !status ? 'Not open' : lengthText(seconds));
  return { title: 'Save a clip', value, icon: image({ ...lengthFace(seconds), tone: key.saving ? 'active' : status?.buffering ? 'normal' : 'off' }), indicator: bar(filled, key.note === 'Failed' ? COLOURS.fail : COLOURS.ring) };
}

function draw(context) {
  const key = keys.get(context);
  if (key?.dial) {
    const feedback = dialFace(key);
    const shown = JSON.stringify(feedback);
    if (shown !== key.drawn) {
      key.drawn = shown;
      sd('setFeedback', context, feedback);
    }
    return;
  }
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
  const small = !status ? (link === 'key' || link === 'bad' ? 'BAD KEY' : 'NOT OPEN') : action === MARKER && !status.recording ? 'NO REC' : reason;
  play(context, [[image({ symbol: 'alert', small, tone: 'fail', mark: 'failed' }), FAIL_HOLD_MS]]);
}

async function poll() {
  if (keys.size === 0 && inspectors.size === 0) return;
  status = await call('GET', '/status', STATUS_TIMEOUT_MS).catch(() => {
    // A missing key file means Afterclip has never run on this PC.
    if (!remote && link === 'local') setLink('offline');
    return null;
  });
  drawAll();
}

const customSeconds = (settings) => {
  const n = Number(settings?.seconds);
  return Number.isInteger(n) && n >= 0 ? n : DEFAULT_CUSTOM_SECONDS;
};
const secondsFor = (action, settings) => fixedSeconds(action) ?? customSeconds(settings);

/** Shows a word on a dial for a moment, such as Saved, then its live state again. */
async function note(context, words) {
  const key = keys.get(context);
  if (!key) return;
  key.note = words;
  draw(context);
  await sleep(words === 'Failed' ? FAIL_HOLD_MS : CONFIRM_HOLD_MS);
  if (key.note === words) key.note = undefined;
  draw(context);
}

/** Turning sends one change at a time, adding up any ticks that arrive while one is on its way. */
async function turnMic(context, ticks) {
  const key = keys.get(context);
  if (!key) return;
  key.ticks = (key.ticks ?? 0) + ticks;
  if (key.turning) return;
  key.turning = true;
  while (key.ticks) {
    const change = key.ticks * VOLUME_STEP;
    key.ticks = 0;
    try {
      const reply = await call('POST', `/mic/volume/${change}`);
      if (status) status.micVolume = reply.micVolume;
      draw(context);
    } catch {
      await poll();
      note(context, 'Failed');
    }
  }
  key.turning = false;
}

function turnLength(context, ticks) {
  const key = keys.get(context);
  if (!key) return;
  key.lengthIndex = Math.min(DIAL_LENGTHS.length - 1, Math.max(0, key.lengthIndex + ticks));
  sd('setSettings', context, { seconds: DIAL_LENGTHS[key.lengthIndex] });
  draw(context);
}

async function pushDial(context, action) {
  const key = keys.get(context);
  if (!key || key.saving) return;
  try {
    if (action === MIC_DIAL) {
      await call('POST', '/mic/toggle');
      await poll();
      return;
    }
    key.saving = true;
    draw(context);
    await call('POST', `/clip/${DIAL_LENGTHS[key.lengthIndex]}`);
    key.saving = false;
    await poll();
    note(context, 'Saved');
  } catch {
    key.saving = false;
    await poll();
    note(context, 'Failed');
  }
}

async function press(context, action, settings) {
  const key = keys.get(context);
  const simple = SIMPLE_KEYS[action];
  let finish;
  try {
    if (simple?.busy) {
      if (key?.saving) return; // It's already working on the last clip.
      if (key) {
        key.saving = true;
        key.run = (key.run ?? 0) + 1;
        key.playing = false;
        draw(context);
      }
      await call('POST', simple.route, simple.timeout);
      finish = () => confirmed(context, simple.confirm);
    } else if (action === TALK) {
      if (key) key.holding = true;
      draw(context);
      await call('POST', '/talk/on');
      // The key may have come up before Afterclip answered.
      if (key && !key.holding) await call('POST', '/talk/off');
      return;
    } else if (action === RECORD || action === MIC || action === CLEAN || action === WEBCAM) {
      await call('POST', { [RECORD]: '/record/toggle', [MIC]: '/mic/toggle', [CLEAN]: '/mic/clean', [WEBCAM]: '/camera/toggle' }[action]);
      finish = () => pulsed(context);
    } else if (action === MARKER) {
      await call('POST', '/marker');
      finish = () => confirmed(context, 'MARKED');
    } else if (simple) {
      const reply = await call('POST', simple.route);
      const words = typeof simple.confirm === 'function' ? simple.confirm(reply) : simple.confirm;
      // Opening a window is its own confirmation.
      finish = words ? () => confirmed(context, words) : () => pulsed(context);
    } else {
      if (key?.saving) return; // A second press mid-save would save the same moment twice.
      if (key) {
        key.saving = true;
        key.run = (key.run ?? 0) + 1;
        key.playing = false;
        draw(context);
      }
      await call('POST', `/clip/${secondsFor(action, settings)}${action === SAVE_STAR ? '/star' : ''}`);
      finish = () => confirmed(context, action === SAVE_STAR ? 'STARRED' : 'SAVED');
    }
  } catch (error) {
    if (key) key.holding = false;
    await poll();
    const reason = /No clips/.test(error.message) ? 'NO CLIPS' : /webhook/i.test(error.message) ? 'NO HOOK'
      : /Push to talk/.test(error.message) ? 'PTT OFF' : /Choose a webcam/.test(error.message) ? 'NO CAM' : 'FAILED';
    finish = () => failed(context, action, reason);
  }
  if (key) key.saving = false;
  await poll();
  finish();
}

ws.addEventListener('open', () => {
  ws.send(JSON.stringify({ event: arg('registerEvent'), uuid: arg('pluginUUID') }));
  ws.send(JSON.stringify({ event: 'getGlobalSettings', context: arg('pluginUUID') }));
  setInterval(poll, POLL_MS);
});

ws.addEventListener('message', ({ data }) => {
  const { event, action, context, payload } = JSON.parse(data);
  if (event === 'didReceiveGlobalSettings') {
    remote = parseAddress(payload.settings?.address);
    link = remote ? 'offline' : 'local';
    status = null;
    for (const key of keys.values()) key.drawn = null;
    drawAll();
    // A new address always gets an answer on the settings page, even if it ends up the same as before.
    poll().then(tellInspectors);
    return;
  }
  if (event === 'propertyInspectorDidAppear') {
    inspectors.add(context);
    tellInspectors();
    poll();
    return;
  }
  if (event === 'propertyInspectorDidDisappear') {
    inspectors.delete(context);
    return;
  }
  if (event === 'sendToPlugin' && payload?.ask === 'status') {
    inspectors.add(context);
    poll().then(tellInspectors);
    return;
  }
  if (event === 'willAppear' || event === 'didReceiveSettings') {

    const existing = keys.get(context);
    const lengthIndex = Math.max(0, DIAL_LENGTHS.indexOf(Number(payload.settings?.seconds ?? 30)));
    if (existing) {
      existing.seconds = secondsFor(action, payload.settings);
      if (existing.dial) existing.lengthIndex = lengthIndex;
    } else {
      keys.set(context, { action, seconds: secondsFor(action, payload.settings), saving: false, playing: false, drawn: undefined, dial: DIALS.has(action), lengthIndex });
    }
    draw(context);
    if (!existing) poll();
  } else if (event === 'willDisappear') {
    keys.delete(context);
  } else if (event === 'keyDown') {
    press(context, action, payload.settings);
  } else if (event === 'keyUp' && action === TALK) {
    const key = keys.get(context);
    if (key?.holding) {
      key.holding = false;
      draw(context);
      call('POST', '/talk/off').catch(() => undefined);
    }

  } else if (event === 'dialRotate') {
    if (action === MIC_DIAL) turnMic(context, payload.ticks);
    else turnLength(context, payload.ticks);
  } else if (event === 'dialDown' || event === 'touchTap') {
    pushDial(context, action);
  }
});

ws.addEventListener('close', () => process.exit(0));
