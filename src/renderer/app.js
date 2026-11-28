const api = window.afterclip;
const $ = (id) => document.getElementById(id);
let settings;
let status = { state: 'stopped', buffering: false, recording: false };
let capture = { mode: 'desktop' };
// Only for as long as the panel is open; the next time it opens, the suggestion shows again if it still applies.
let cardHdrDismissed = false;
// Each capture card's sizes and highest frame rates, by device id. Null while being read.
const cardModes = {};
let media = [];
let kind = 'clip';
let chosenLength = null;
let audioChoices = { outputs: [], inputs: [], apps: [] };
const selected = new Set();
const info = new Map(); // file|mtime -> { url, duration }
let activeProfile = null;
let activeBuffer = null; // The running game's profile can use a different buffer from Settings.
let renaming = null;
let tagging = null;
let cameraChoices; // Undefined until asked for, and null if capture couldn't list them.
let cardChoices; // The same, as { cards, audio }.
const strips = new Map(); // file|mtime -> { url, frames }, or a promise while one is being made
let favouritesOnly = false;
let update = { status: 'idle' };
let appVersion = '';
let patchNotes = null;
let seenVersion = '';
let streamDeck = { offer: null, available: null };
let storageUsed = 0;
let remote = { on: false };

const errorText = (e) => String(e.message ?? e);
const lengthLabel = (s) => (s > 0 ? (s >= 60 && s % 60 === 0 ? `${s / 60}m` : `${s}s`) : 'All');
const clockLabel = (seconds) => {
  const s = Math.round(seconds);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};
const bufferLabel = (seconds) => (seconds >= 60 ? `${+(seconds / 60).toFixed(1)}m` : `${seconds}s`);

const BANNER_MS = 6000;
const UNDO_BANNER_MS = 10000; // Longer, so there's time to reach the button.

/** A short message at the bottom of the panel, optionally with one button, such as Undo. */
function showBanner(text, tone, action) {
  const b = $('banner');
  b.textContent = text;
  b.className = `banner ${tone ?? ''}`;
  if (action) {
    const button = document.createElement('button');
    button.className = 'banner-action';
    button.textContent = action.label;
    button.onclick = () => { b.hidden = true; action.run(); };
    b.append(button);
  }
  b.hidden = false;
  clearTimeout(showBanner.timer);
  showBanner.timer = setTimeout(() => { b.hidden = true; }, action ? UNDO_BANNER_MS : BANNER_MS);
}

/** Says files went to the Recycle Bin, with an Undo that puts them back. */
function showDeleted(files) {
  const text = files.length === 1 ? 'Moved to the Recycle Bin' : `Moved ${files.length} to the Recycle Bin`;
  showBanner(text, 'good', { label: 'Undo', run: () => undoDelete(files) });
}

async function undoDelete(files) {
  try {
    const restored = await api.invoke('restore', files);
    showBanner(restored === 1 ? 'Put back' : `Put back ${restored}`, 'good');
  } catch (e) {
    showBanner(errorText(e), 'bad');
  }
  await refresh();
}

function renderStatus() {
  const card = $('capture-card');
  const ready = status.state === 'ready' && status.buffering;
  const shining = card.classList.contains('saved');
  const dark = capture.mode === 'card' && capture.noPicture;
  card.className = `capture-card ${status.recording ? 'recording' : ready && !dark ? 'live' : status.state === 'error' || dark ? 'bad' : ''}`;
  card.classList.toggle('saved', shining);
  renderSummary();
  const labels = {
    desktop: 'Recording primary monitor',
    detecting: `Connecting to ${capture.app ?? 'game'}`,
    game: capture.app ?? 'Game',
    fallback: capture.app ?? 'Fullscreen app',
    private: capture.app ?? 'Protected game',
    card: 'Capture card',
  };
  let title = labels[capture.mode] ?? 'Recording';
  let detail = capture.mode === 'game' ? 'Game capture' : capture.mode === 'fallback' ? 'Recording its monitor' : capture.mode === 'detecting' ? 'Recording its monitor for now' : 'Waiting for a game';
  if (capture.mode === 'card') detail = capture.noPicture ? 'No picture from the card. Close it in other apps such as OBS, and check the console is on with HDCP off. Afterclip keeps trying.' : 'Recording your capture card';
  else if (capture.mode === 'private') detail = 'Video hidden by Capture privacy';
  else if (capture.mode !== 'game' && settings?.gameProfiles.some((game) => game.capturePrivacy)) detail = 'Monitor capture disabled by Capture privacy';
  if (status.state === 'error') { title = 'Capture stopped'; detail = status.message ?? ''; }
  else if (status.waitingForGame) { title = 'Waiting for a game'; detail = 'The buffer starts when a game opens'; }
  else if (status.waitingForSignal) { title = 'Capture card: no signal'; detail = 'The buffer is paused, and starts again when a picture arrives'; }
  else if (!ready) { title = 'Starting capture'; detail = ''; }
  else if (status.recording) { detail = `Full recording running · ${detail}`; }
  $('capture-title').textContent = title;
  $('capture-detail').textContent = detail;
  $('capture-detail').title = detail;

  const rec = $('record');
  rec.textContent = status.recording ? 'Stop' : 'Record';
  rec.classList.toggle('on', status.recording);
  // Recording doesn't need a game, so it can start the buffer that's waiting for one.
  rec.disabled = status.state !== 'ready' && !status.waitingForGame;
  $('save-clip').disabled = !status.buffering;

  const cleanMic = settings?.cleanMic ?? false;
  $('clean-mic').classList.toggle('on', cleanMic);
  $('clean-mic').setAttribute('aria-pressed', String(cleanMic));
  $('cleanMicSwitch').checked = cleanMic;
  $('marker').hidden = !status.recording;
  const micMuted = settings?.micMuted ?? false;
  $('mic').classList.toggle('muted', micMuted);
  $('mic').setAttribute('aria-pressed', String(micMuted));
  $('mic').title = micMuted ? 'Mic is muted in clips. Click to unmute.' : 'Mute mic in clips';

  const showAdmin = !capture.elevated && (capture.needsAdmin || capture.suggestAdmin);
  $('admin').hidden = !showAdmin;
  $('admin-text').textContent = capture.needsAdmin
    ? `${capture.app} is running as administrator, so Afterclip is recording its monitor instead.`
    : `Afterclip couldn't capture ${capture.app} directly, so it's recording its monitor. Running as administrator may fix this.`;
  const preview = capture.mode === 'card' && status.state === 'ready' && !capture.noPicture;
  $('card-preview').hidden = !preview;
  $('capture-dot').hidden = preview;
  if (preview) watchCardPreview();
  $('card-hdr-hint').hidden =!(capture.mode === 'card' && capture.suggestCardHdr && !cardHdrDismissed && !settings?.cardHdr);
  // Afterclip already looked for each one by name, so these are really gone, not just renumbered.
  const missing = status.missingAudio ?? [];
  $('audio-missing').hidden = missing.length === 0;
  $('audio-missing-text').textContent = missing.length === 1
    ? `${missing[0]} isn't connected, so it isn't in your clips. Plug it in or start its app, or choose another device.`
    : `${missing.length} audio sources aren't connected, so they aren't in your clips: ${missing.join(', ')}. Plug them in or start their apps, or choose other devices.`;
  renderBuffer();
}

// The capture card's live picture, a few times a second, only while it's on screen. The engine only makes copies
// for a few seconds after each ask, so a hidden panel costs nothing.
const PREVIEW_EVERY_MS = 300;
let previewTimer = null;
function watchCardPreview() {
  if (previewTimer) return;
  const tick = async () => {
    const canvas = $('card-preview');
    if (canvas.hidden || document.visibilityState !== 'visible') {
      previewTimer = null;
      return;
    }
    try {
      const frame = await api.invoke('card-preview');
      if (frame?.rgba && frame.width === canvas.width && frame.height === canvas.height) {
        const bytes = Uint8ClampedArray.from(atob(frame.rgba), (c) => c.charCodeAt(0));
        for (let i = 3; i < bytes.length; i += 4) bytes[i] = 255;
        canvas.getContext('2d').putImageData(new ImageData(bytes, frame.width, frame.height), 0, 0);
      }
    } catch {
      // Capture restarting: try again on the next tick.
    }
    previewTimer = setTimeout(tick, PREVIEW_EVERY_MS);
  };
  previewTimer = setTimeout(tick, 0);
}
document.addEventListener('visibilitychange', () => { if (!$('card-preview').hidden) watchCardPreview(); });

// The bar: its full width is the buffer, the colour is what's filled so far, and the white part, at the
// newest end, is what Save would keep. It only ticks while the buffer is still filling.
let bufferTimer = null;
let frameProblem = '';
function renderBuffer() {
  if (!settings) return;
  const buffer = Math.max(1, activeBuffer ?? settings.bufferSeconds);
  const filled = status.buffering && status.bufferingSince ? Math.min(buffer, Math.max(0, (Date.now() - status.bufferingSince) / 1000)) : 0;
  const wanted = chosenLength > 0 ? chosenLength : buffer;
  const kept = Math.min(wanted, filled);
  const percent = (seconds) => (100 * seconds) / buffer;
  $('buffer-filled').style.width = `${percent(filled)}%`;
  $('buffer-kept').style.left = `${percent(filled - kept)}%`;
  $('buffer-kept').style.width = `${percent(kept)}%`;
  $('buffer-time').textContent = clockLabel(filled);
  $('buffer-note').classList.toggle('warn', !!frameProblem);
  $('buffer-note').title = frameProblem;
  $('buffer-note').textContent = frameProblem ? 'Losing frames'
    : !status.buffering ? (status.waitingForGame ? 'Off until a game opens' : status.waitingForSignal ? 'Paused until the card has a picture' : 'Not buffering')
    : wanted >= buffer ? 'Whole buffer selected'
      : kept < wanted ? `Filling to ${lengthLabel(wanted)}`
        : `Last ${lengthLabel(wanted)} selected`;
  const filling = status.buffering && filled < buffer;
  if (filling && !bufferTimer) bufferTimer = setInterval(renderBuffer, 1000);
  if (!filling && bufferTimer) { clearInterval(bufferTimer); bufferTimer = null; }
}

function shineBuffer() {
  const card = $('capture-card');
  card.classList.remove('saved');
  // Reading layout restarts the animation when clips are saved close together.
  void card.offsetWidth;
  card.classList.add('saved');
}

function renderSummary() {
  if (!settings) return;
  const quality = settings.quality === 'full' ? 'Full' : settings.quality.charAt(0).toUpperCase() + settings.quality.slice(1);
  const where = `${status.bufferOnDisk ? ' on disk' : ''}`;
  if (settings.captureCard && settings.captureCardDevice) {
    const height = Number(settings.cardResolution);
    $('summary').textContent = `Capture card · ${height === 2160 ? '4K' : `${height}p`}${status.hdr ? ' · HDR' : ''}${settings.verticalMode ? ' · Vertical' : ''} · ${bufferLabel(settings.bufferSeconds)} buffer${where} · ${settings.cardFps} FPS`;
    return;
  }
  $('summary').textContent = activeProfile
    ? `${settings.gameProfiles.find((p) => p.exe === activeProfile)?.name ?? activeProfile} profile · ${bufferLabel(activeBuffer ?? settings.bufferSeconds)} buffer${where}`
    : `${quality}${status.hdr ? ' · HDR' : ''}${settings.verticalMode ? ' · Vertical' : ''} · ${bufferLabel(settings.bufferSeconds)} buffer${where} · ${settings.fps} FPS`;
}

function renderLengths() {
  const presets = settings.presets;
  if (!presets.some((p) => p.seconds === chosenLength)) {
    chosenLength = (presets.find((p) => p.seconds === 30) ?? presets[0])?.seconds ?? 0;
  }
  $('lengths').replaceChildren(...presets.map((p) => {
    const b = document.createElement('button');
    b.textContent = lengthLabel(p.seconds);
    b.setAttribute('role', 'radio');
    b.setAttribute('aria-checked', String(p.seconds === chosenLength));
    b.classList.toggle('active', p.seconds === chosenLength);
    b.title = p.seconds > 0 ? `Save the last ${p.seconds} seconds` : 'Save the whole buffer';
    b.onclick = () => { chosenLength = p.seconds; renderLengths(); };
    return b;
  }));
  $('save-clip').textContent = chosenLength > 0 ? `Save last ${lengthLabel(chosenLength)}` : 'Save whole buffer';
  const key = presets.find((p) => p.seconds === chosenLength)?.hotkey;
  const hint = $('hint');
  hint.replaceChildren();
  if (key) {
    const k = document.createElement('kbd');
    k.textContent = key;
    hint.append('Press ', k, ' to save in-game');
  } else {
    hint.textContent = 'Set a hotkey in Settings to save in-game';
  }
  renderBuffer();
}

const parseName = (name) => {
  const stem = name.replace(/\.[^.]+$/, '');
  const m = /^(.*?)_(\d{4}-\d{2}-\d{2})_(\d{2})-(\d{2})/.exec(stem);
  if (!m) return { game: stem };
  return { game: /^(Clip|Recording|Screenshot)$/.test(m[1]) ? 'Desktop' : m[1].replace(/^(Merged|Edit)$/, '$1 clip') };
};

function timeAgo(ms) {
  const minutes = Math.round((Date.now() - ms) / 60000);
  if (minutes < 1) return 'Just now';
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} hr ago`;
  const days = Math.round(hours / 24);
  return days === 1 ? 'Yesterday' : `${days} days ago`;
}

const sizeLabel = (bytes) => (bytes >= 1073741824 ? `${(bytes / 1073741824).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1048576))} MB`);

// Only asks for thumbnails as items scroll into view, so a long library costs nothing up front.
const observer = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    if (!entry.isIntersecting) continue;
    observer.unobserve(entry.target);
    const { file, key } = entry.target.dataset;
    api.invoke('thumbnail', file).then((result) => {
      if (!result) return;
      info.set(key, result);
      applyInfo(entry.target, result);
    });
  }
});

/** Moving across a video's thumbnail scrubs through it, from a strip of frames made the first time. */
const SCRUB_DELAY_MS = 250;
function wireScrub(thumb, m, key, autoplay) {
  let preview = null;
  let scrubbing = false;
  let timer = null;
  let over = false;
  let x = 0;
  const show = (strip) => {
    if (!over || !strip?.url) return;
    let scrub = thumb.querySelector('.scrub');
    if (!scrub) {
      scrub = document.createElement('div');
      scrub.className = 'scrub';
      scrub.innerHTML = '<div class="scrub-line"></div>';
      scrub.style.backgroundImage = `url("${strip.url}")`;
      // Each frame fills the thumbnail's width. A vertical clip's frames are taller, so their middle shows.
      scrub.style.backgroundSize = `${strip.frames * 100}% auto`;
      thumb.append(scrub);
    }
    const index = Math.min(strip.frames - 1, Math.floor(x * strip.frames));
    scrub.style.backgroundPositionX = `${strip.frames > 1 ? (index / (strip.frames - 1)) * 100 : 0}%`;
    scrub.firstChild.style.width = `${x * 100}%`;
  };
  const load = () => {
    if (!strips.has(key)) {
      strips.set(key, api.invoke('preview-strip', m.file).then((strip) => {
        strips.set(key, strip);
        return strip;
      }, () => { strips.delete(key); }));
    }
    Promise.resolve(strips.get(key)).then((strip) => {
      show(strip);
      if (!over || scrubbing || !strip?.frames || !autoplay || matchMedia('(prefers-reduced-motion: reduce)').matches) return;
      let frame = 0;
      preview = setInterval(() => {
        if (!thumb.isConnected) {
          clearInterval(preview);
          return;
        }
        frame = (frame + 1) % strip.frames;
        x = frame / strip.frames;
        show(strip);
      }, 250);
    });
  };
  thumb.addEventListener('mouseenter', () => { over = true; scrubbing = false; timer = setTimeout(load, SCRUB_DELAY_MS); });
  thumb.addEventListener('mousemove', (e) => {
    if (preview !== null) {
      clearInterval(preview);
      preview = null;
      scrubbing = true;
    }
    const box = thumb.getBoundingClientRect();
    x = Math.min(0.999, Math.max(0, (e.clientX - box.left) / box.width));
    const known = strips.get(key);
    if (known && !(known instanceof Promise)) show(known);
  });
  thumb.addEventListener('mouseleave', () => {
    over = false;
    clearTimeout(timer);
    clearInterval(preview);
    preview = null;
    thumb.querySelector('.scrub')?.remove();
  });
}

function applyInfo(thumb, result) {
  thumb.querySelector('img').src = result.url;
  const badge = thumb.querySelector('.length');
  badge.textContent = clockLabel(result.duration);
  badge.hidden = !result.duration;
}

function closeMenus() {
  document.querySelectorAll('.menu').forEach((m) => m.remove());
}

function renderSelection() {
  $('selection').hidden = selected.size === 0;
  $('selection-count').textContent = `${selected.size} selected`;
  // The editor only takes video.
  $('merge-selected').hidden = kind === 'screenshot';
  disarmDelete();
}

// Deleting many at once takes a second click, so a slip can't clear out a selection.
const DELETE_ARMED_MS = 4000;
let deleteArmed = null;

function disarmDelete() {
  clearTimeout(deleteArmed);
  deleteArmed = null;
  $('delete-selected').textContent = 'Delete';
}

async function deleteSelected() {
  if (selected.size > 1 && !deleteArmed) {
    $('delete-selected').textContent = `Delete ${selected.size}?`;
    deleteArmed = setTimeout(disarmDelete, DELETE_ARMED_MS);
    return;
  }
  disarmDelete();
  const files = [...selected];
  const results = await Promise.allSettled(files.map(async (file) => {
    await api.invoke('trash', file);
    selected.delete(file);
  }));
  const failed = results.filter((r) => r.status === 'rejected');
  // The first reason is shown, since "open in another app" is only one of them.
  if (failed.length) showBanner(`Couldn't delete ${failed.length} of ${files.length}. ${errorText(failed[0].reason)}`, 'bad');
  else showDeleted(files);
  await refresh();
  renderMedia();
}

// Dragging a clip hands it to Windows, which then carries it into any app, as File Explorer does.
const DRAG_START_PIXELS = 6;
let justDragged = false;
function wireDrag(li, m) {
  li.addEventListener('dragstart', (e) => e.preventDefault());
  li.addEventListener('mousedown', (down) => {
    if (down.button !== 0 || down.target.closest('.more, .star, input, .menu')) return;
    const stop = () => {
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', stop);
    };
    const move = (e) => {
      if (Math.hypot(e.clientX - down.clientX, e.clientY - down.clientY) < DRAG_START_PIXELS) return;
      stop();
      justDragged = true;
      api.invoke('start-drag', m.file)
        .catch((error) => showBanner(errorText(error), 'bad'))
        .finally(() => setTimeout(() => { justDragged = false; }, 0));
    };
    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', stop);
  });
}

const KIND_TEXT = {
  clip: { title: 'All clips', empty: 'No clips yet. Save your first moment with the button above or a hotkey.' },
  recording: { title: 'All recordings', empty: 'No recordings yet. Press Record to capture a full session.' },
  screenshot: { title: 'All screenshots', empty: 'No screenshots yet. Press the camera button above or your screenshot hotkey.' },
};

const HOME_CLIPS = 5;
let libraryOpen = false;
function setLibrary(open) {
  libraryOpen = open;
  if (!open) sessionFilter = null;
  $('home').classList.toggle('library-open', open);
  $('library-head').hidden = !open;
  $('brand').hidden = open;
  $('summary').hidden = open;
  $('view-all').hidden = open;
  $('open-editor').hidden = !open;
  $('library-tools').hidden = !open;
  renderMedia();
}

const gameOf = (m) => m.game ?? parseName(m.name).game;
const tagsOf = (m) => m.tags ?? [];

/** The tag filter offers the tags in use, and stays hidden until there are some. */
function renderTagFilter(all) {
  const select = $('library-tag');
  const chosen = select.value;
  const tags = [...new Set(all.flatMap(tagsOf))].sort((a, b) => a.localeCompare(b));
  select.replaceChildren(...[['', 'All tags'], ...tags.map((t) => [t, `#${t}`])].map(([value, label]) => new Option(label, value)));
  select.value = tags.includes(chosen) ? chosen : '';
  select.hidden = tags.length === 0;
}

/** The game filter only offers games you have clips of, plus whatever's chosen. */
function renderGameFilter(all) {
  const select = $('library-game');
  const chosen = select.value;
  const games = [...new Set(all.map(gameOf))].sort((a, b) => a.localeCompare(b));
  const options = [['', 'All games'], ...games.map((g) => [g, g])].map(([value, label]) => {
    const o = document.createElement('option');
    o.value = value;
    o.textContent = label;
    return o;
  });
  select.replaceChildren(...options);
  select.value = games.includes(chosen) ? chosen : '';
}

const SORTS = {
  newest: (a, b) => b.mtime - a.mtime,
  oldest: (a, b) => a.mtime - b.mtime,
  largest: (a, b) => b.size - a.size,
  name: (a, b) => a.name.localeCompare(b.name),
};

/** The game session being looked over, from its card on the home page, or null. */
let sessionFilter = null;
let lastSession = null;

const inSession = (m, s) => m.mtime >= s.startMs && m.mtime <= s.endMs;

/** The card on the home page after a game closes, saying how many clips it saved. */
function renderSession() {
  const card = $('session');
  card.hidden = !lastSession || libraryOpen;
  if (!lastSession) return;
  const clips = media.filter((m) => m.kind === 'clip' && inSession(m, lastSession));
  const games = [...new Set(clips.map(gameOf))];
  const count = `${clips.length} ${clips.length === 1 ? 'clip' : 'clips'}`;
  $('session-text').textContent = games.length === 1 ? `Last ${games[0]} session: ${count}` : `Last session: ${count}`;
  if (!clips.length) card.hidden = true;
}

function reviewSession() {
  sessionFilter = lastSession;
  kind = 'clip';
  document.querySelectorAll('.tab').forEach((t) => t.classList.toggle('active', t.dataset.kind === 'clip'));
  $('library-search').value = '';
  $('library-game').value = '';
  $('library-tag').value = '';
  favouritesOnly = false;
  $('library-favourites').setAttribute('aria-pressed', 'false');
  setLibrary(true);
}

function filtered(all) {
  const words = $('library-search').value.trim().toLowerCase();
  const game = $('library-game').value;
  const tag = $('library-tag').value;
  return all
    .filter((m) => !sessionFilter || inSession(m, sessionFilter))
    .filter((m) => !favouritesOnly || m.favourite)
    .filter((m) => !game || gameOf(m) === game)
    .filter((m) => !tag || tagsOf(m).includes(tag))
    .filter((m) => !words || `${m.name} ${gameOf(m)} ${tagsOf(m).map((t) => `#${t}`).join(' ')}`.toLowerCase().includes(words))
    .sort(SORTS[$('library-sort').value] ?? SORTS.newest);
}

function renameField(m, title) {
  const input = document.createElement('input');
  input.className = 'rename-field';
  input.value = title;
  input.setAttribute('aria-label', 'New clip name');
  let done = false;
  const finish = async (save) => {
    if (done) return;
    done = true;
    renaming = null;
    if (save && input.value.trim() && input.value.trim() !== title) {
      await api.invoke('rename-clip', { file: m.file, name: input.value, game: gameOf(m) }).catch((e) => showBanner(errorText(e), 'bad'));
      await refresh();
    } else {
      renderMedia();
    }
  };
  input.onkeydown = (e) => {
    e.stopPropagation();
    if (e.key === 'Enter') finish(true);
    if (e.key === 'Escape') finish(false);
  };
  input.onblur = () => finish(true);
  setTimeout(() => { input.focus(); input.select(); }, 0);
  return input;
}

/** Tags are typed in one field, separated by commas. */
function tagField(m) {
  const input = document.createElement('input');
  input.className = 'rename-field';
  input.value = tagsOf(m).join(', ');
  input.placeholder = 'Tags, separated by commas';
  input.setAttribute('aria-label', 'Tags');
  let done = false;
  const finish = async (save) => {
    if (done) return;
    done = true;
    tagging = null;
    if (save) {
      const tags = input.value.split(',').map((t) => t.trim()).filter(Boolean);
      await api.invoke('set-tags', { file: m.file, tags }).catch((e) => showBanner(errorText(e), 'bad'));
      await refresh();
    } else {
      renderMedia();
    }
  };
  input.onkeydown = (e) => {
    e.stopPropagation();
    if (e.key === 'Enter') finish(true);
    if (e.key === 'Escape') finish(false);
  };
  input.onblur = () => finish(true);
  setTimeout(() => { input.focus(); input.select(); }, 0);
  return input;
}

function renderMedia() {
  const all = media.filter((m) => m.kind === kind);
  renderGameFilter(all);
  renderTagFilter(all);
  const shown = libraryOpen ? filtered(all) : all.slice(0, HOME_CLIPS);
  $('session-filter').hidden = !sessionFilter;
  renderSession();
  $('view-all').textContent = `View all (${all.length})`;
  $('make-reel').hidden = kind === 'screenshot';
  $('find-overlaps').hidden = kind !== 'clip';
  if (kind !== 'clip') $('overlaps').hidden = true;
  $('library-title').textContent = KIND_TEXT[kind].title;
  const empty = $('empty');
  empty.hidden = shown.length > 0;
  const nothingMatches = libraryOpen && all.length > 0;
  empty.textContent = nothingMatches ? 'Nothing matches that.' : KIND_TEXT[kind].empty;
  observer.disconnect();
  $('media').replaceChildren(...shown.map((m) => {
    const li = document.createElement('li');
    li.className = 'item';
    li.classList.toggle('selected', selected.has(m.file));
    const thumb = document.createElement('button');
    thumb.className = 'thumb';
    thumb.setAttribute('aria-label', `${m.kind === 'screenshot' ? 'Open' : 'Play'} ${m.name}`);
    thumb.title = `Click to ${m.kind === 'screenshot' ? 'open' : 'play'}, or drag into another app`;
    thumb.onclick = () => { if (!justDragged) api.invoke('open', m.file).catch((e) => showBanner(errorText(e), 'bad')); };
    const img = document.createElement('img');
    img.alt = '';
    img.draggable = false;
    const badge = document.createElement('span');
    badge.className = 'length';
    badge.hidden = true;
    thumb.append(img, badge);
    const key = `${m.file}|${m.mtime}`;
    thumb.dataset.file = m.file;
    thumb.dataset.key = key;
    const known = info.get(key);
    if (known) applyInfo(thumb, known); else observer.observe(thumb);
    if (m.kind !== 'screenshot') wireScrub(thumb, m, key, libraryOpen);

    const text = document.createElement('div');
    text.className = 'item-text';
    const title = document.createElement('div');
    title.className = 'item-title';
    const shownName = parseName(m.name).game;
    title.textContent = shownName;
    const sub = document.createElement('div');
    sub.className = 'item-sub';
    // A renamed clip's title no longer says which game it came from.
    const tags = tagsOf(m).map((t) => `#${t}`).join(' ');
    sub.textContent = `${m.game && m.game !== shownName ? `${m.game} · ` : ''}${timeAgo(m.mtime)} · ${sizeLabel(m.size)}${tags ? ` · ${tags}` : ''}`;
    sub.title = tags ? `${m.name}\n${tags}` : m.name;
    if (renaming === m.file) text.append(renameField(m, shownName), sub);
    else if (tagging === m.file) text.append(title, tagField(m));
    else text.append(title, sub);

    const actions = document.createElement('div');
    actions.className = 'item-actions';
    const more = document.createElement('button');
    more.className = 'more';
    more.textContent = '⋯';
    more.setAttribute('aria-label', `Options for ${m.name}`);
    more.onclick = (e) => {
      e.stopPropagation();
      const wasOpen = li.querySelector('.menu');
      closeMenus();
      if (!wasOpen) li.append(buildMenu(m));
    };
    // Right-clicking a clip opens the same menu, instead of the browser's own.
    li.oncontextmenu = (e) => {
      if (e.target.closest('input')) return;
      e.preventDefault();
      closeMenus();
      li.append(buildMenu(m));
    };
    const pick = document.createElement('input');
    pick.type = 'checkbox';
    pick.title = m.kind === 'screenshot' ? 'Select to delete together' : 'Select to edit or delete together';
    pick.setAttribute('aria-label', `Select ${m.name}`);
    pick.checked = selected.has(m.file);
    pick.onchange = () => {
      if (pick.checked) selected.add(m.file); else selected.delete(m.file);
      li.classList.toggle('selected', pick.checked);
      renderSelection();
    };
    const star = document.createElement('button');
    star.className = 'star';
    star.classList.toggle('on', !!m.favourite);
    star.setAttribute('aria-pressed', String(!!m.favourite));
    star.setAttribute('aria-label', `${m.favourite ? 'Remove from favourites' : 'Add to favourites'}: ${m.name}`);
    star.title = m.favourite ? 'Remove from favourites' : 'Add to favourites';
    star.innerHTML = '<svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor" aria-hidden="true"><path d="M12 3l2.7 5.6 6.1.8-4.5 4.2 1.2 6.1L12 16.8l-5.5 2.9 1.2-6.1-4.5-4.2 6.1-.8z"/></svg>';
    star.onclick = async (e) => {
      e.stopPropagation();
      await api.invoke('set-favourite', { file: m.file, on: !m.favourite }).catch((error) => showBanner(errorText(error), 'bad'));
      refresh();
    };
    actions.append(star, more, pick);
    li.append(thumb, text, actions);
    wireDrag(li, m);
    return li;
  }));
  renderSelection();
}


function buildMenu(m) {
  const menu = document.createElement('div');
  menu.className = 'menu';
  const item = (label, fn, cls) => {
    const b = document.createElement('button');
    b.textContent = label;
    if (cls) b.className = cls;
    b.onclick = () => { closeMenus(); fn(); };
    return b;
  };
  menu.append(
    ...(m.kind !== 'screenshot' ? [item('Edit and share', () => openShare(m))] : []),
    item('Rename', () => { renaming = m.file; renderMedia(); }),
    item('Tags', () => { tagging = m.file; renderMedia(); }),
    item('Show in folder', () => api.invoke('reveal', m.file)),
    item('Delete', async () => {
      try {
        await api.invoke('trash', m.file);
        selected.delete(m.file);
        showDeleted([m.file]);
      } catch (e) {
        showBanner(errorText(e), 'bad');
      }
      refresh();
    }, 'danger'),
  );
  return menu;
}

/* Edit and share */

/** Discord channels that have an address, each with its place in the full list, which is how the backend finds it. */
const webhooks = () => (settings?.discordWebhooks ?? [])
  .map((h, index) => ({ name: h.name.trim() || `Channel ${index + 1}`, url: h.url, index }))
  .filter((h) => h.url.trim());

/** Opens the Edit and share window on a clip, optionally at a time in seconds. */
function openShare(m, at = 0) {
  return api.invoke('open-share', { file: m.file, game: gameOf(m), at }).catch((e) => showBanner(errorText(e), 'bad'));
}

async function refresh() {
  const state = await api.invoke('state');
  status = state.status;
  capture = state.capture;
  media = state.media;
  activeProfile = state.activeProfile ?? null;
  activeBuffer = state.activeBufferSeconds ?? null;
  update = state.update ?? update;
  appVersion = state.version ?? appVersion;
  patchNotes = state.patchNotes ?? null;
  seenVersion = state.settings.seenVersion ?? '';
  streamDeck = state.streamDeck ?? streamDeck;
  renderStreamDeck();
  storageUsed = state.storageUsedBytes ?? 0;
  lastSession = state.lastSession ?? null;
  remote = state.remote ?? remote;
  renderRemote();
  controllersSeen = state.controllers ?? controllersSeen;
  renderControllersSeen();
  renderUpdate();
  renderUsageOffer();
  renderStorageUsed();
  // Only this comes from the backend after startup: the form owns the rest while it's being edited.
  if (settings) {
    settings.micMuted = state.settings.micMuted;
    settings.cleanMic = state.settings.cleanMic;
    // The Stream Deck can switch the webcam too.
    if (settings.captureCard !== state.settings.captureCard) {
      settings.captureCard = state.settings.captureCard;
      $('captureCard').checked = settings.captureCard;
      renderCard();
      renderSummary();
    }
    if (settings.cameraEnabled !== state.settings.cameraEnabled) {
      settings.cameraEnabled = state.settings.cameraEnabled;
      $('cameraEnabled').checked = settings.cameraEnabled;
      renderCamera();
    }
  }
  renderEncoderNote(state.encoderInUse);
  const files = new Set(media.map((m) => m.file));
  for (const f of selected) if (!files.has(f)) selected.delete(f);
  renderStatus();
  // Redrawing would throw away a name or tags being typed.
  if (!renaming && !tagging) renderMedia();
  return state;
}

/* Settings */

function acceleratorFrom(e) {
  const mods = [];
  if (e.ctrlKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Super');
  let key;
  if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (/^Digit\d$/.test(e.code)) key = e.code.slice(5);
  else if (/^F\d{1,2}$/.test(e.code) || /^Numpad\d$/.test(e.code)) key = e.code;
  else if (['Insert', 'Delete', 'Home', 'End', 'PageUp', 'PageDown'].includes(e.code)) key = e.code;
  if (!key) return null;
  // A bare letter would stop working as a key in the game, so only F-keys and Insert-row keys may go without a modifier.
  if (!mods.length && /^[A-Z0-9]$/.test(key)) return null;
  return [...mods, key].join('+');
}

function bindHotkey(input, get, set) {
  input.value = get();
  input.readOnly = true;
  input.classList.add('hotkey');
  input.onfocus = () => { input.classList.add('listening'); input.value = 'Press keys'; };
  input.onblur = () => { input.classList.remove('listening'); input.value = get(); };
  input.onkeydown = (e) => {
    e.preventDefault();
    if (e.code === 'Escape') return input.blur();
    if (e.code === 'Backspace') { set(''); return input.blur(); }
    const accel = acceleratorFrom(e);
    if (accel) { set(accel); input.blur(); }
  };
}

/** Push-to-talk takes one key on its own, or a mouse button, because it's held rather than pressed. */
const TALK_KEYS = {
  Space: 'Space', Tab: 'Tab', CapsLock: 'CapsLock', Backquote: 'Backquote',
  ShiftLeft: 'LeftShift', ShiftRight: 'RightShift', ControlLeft: 'LeftCtrl', ControlRight: 'RightCtrl', AltLeft: 'LeftAlt', AltRight: 'RightAlt',
  Insert: 'Insert', Delete: 'Delete', Home: 'Home', End: 'End', PageUp: 'PageUp', PageDown: 'PageDown',
};
const TALK_BUTTONS = { 1: 'MouseMiddle', 3: 'Mouse4', 4: 'Mouse5' };
const talkLabel = (key) => ({ Mouse4: 'Mouse 4', Mouse5: 'Mouse 5', MouseMiddle: 'Middle mouse', LeftShift: 'Left Shift', RightShift: 'Right Shift', LeftCtrl: 'Left Ctrl', RightCtrl: 'Right Ctrl', LeftAlt: 'Left Alt', RightAlt: 'Right Alt', CapsLock: 'Caps Lock', Backquote: '`' }[key] ?? key) || 'None';

function talkKeyFrom(e) {
  if (/^Key[A-Z]$/.test(e.code)) return e.code.slice(3);
  if (/^Digit\d$/.test(e.code)) return e.code.slice(5);
  if (/^F\d{1,2}$/.test(e.code) || /^Numpad\d$/.test(e.code)) return e.code;
  return TALK_KEYS[e.code] ?? null;
}

function bindTalkKey(input) {
  const show = () => { input.value = talkLabel(settings.pushToTalkKey); };
  const set = (key) => { settings.pushToTalkKey = key; scheduleSave(); input.blur(); };
  show();
  input.classList.add('hotkey');
  input.onfocus = () => { input.classList.add('listening'); input.value = 'Press a key or button'; };
  input.onblur = () => { input.classList.remove('listening'); show(); };
  input.onkeydown = (e) => {
    e.preventDefault();
    if (e.code === 'Escape') return input.blur();
    const key = talkKeyFrom(e);
    if (key) set(key);
  };
  input.onmousedown = (e) => {
    const key = document.activeElement === input && TALK_BUTTONS[e.button];
    if (!key) return;
    e.preventDefault();
    set(key);
  };
  // Side buttons would otherwise go back a page.
  input.onmouseup = (e) => { if (e.button > 2) e.preventDefault(); };
  input.onauxclick = (e) => e.preventDefault();
}

/* Webcam */

async function loadCameraChoices() {
  try {
    cameraChoices = await api.invoke('camera-choices');
  } catch {
    cameraChoices = null; // Capture isn't running, so the list can't be read.
  }
  renderCamera();
}

function renderCamera() {
  const select = $('cameraDevice');
  const choices = cameraChoices ?? [];
  select.replaceChildren(...choices.map((c) => new Option(c.name, c.id)));
  if (settings.cameraDevice && !choices.some((c) => c.id === settings.cameraDevice)) {
    select.add(new Option(cameraChoices ? 'Saved camera (not connected)' : 'Saved camera', settings.cameraDevice));
  }
  const none = cameraChoices ? 'No webcam found' : cameraChoices === undefined ? 'Looking for webcams' : "Webcams couldn't be listed";
  if (!select.options.length) select.add(new Option(none, ''));

  select.value = settings.cameraDevice;
  if (!settings.cameraDevice && choices.length) select.value = choices[0].id;
  const on = $('cameraEnabled').checked;
  for (const id of ['cameraDevice', 'cameraCorner', 'cameraSize', 'verticalFaceCam']) $(id).disabled = !on;
  // Vertical mode puts the webcam across the top, so its corner and size don't apply.
  for (const id of ['cameraCorner', 'cameraSize']) $(id).disabled ||= $('verticalMode').checked;
}

/* Capture card */

const CARD_AUDIO_INDEX = -2; // The engine's index for the card's sound, beside -1 for game audio.

async function loadCardChoices() {
  try {
    cardChoices = await api.invoke('card-choices');
  } catch {
    cardChoices = null;
  }
  renderCard();
}

/** A device name as letters and digits, so spacing and punctuation don't stop a match. */
const squash = (name) => name.toLowerCase().replace(/[^a-z0-9]/g, '');

/**
 * The card's own audio device, or none. Its name usually contains the card's, as in "Digital Audio Interface
 * (Game Capture 4K X)", though DirectShow can cut it to 31 characters, leaving "(Game C".
 */
function matchingCardAudio(cardId) {
  const card = cardChoices?.cards.find((c) => c.id === cardId);
  if (!card) return '';
  const cardName = squash(card.name);
  const matches = (audio) => {
    const whole = squash(audio.name);
    const inside = squash(audio.name.split('(')[1] ?? '');
    return whole.includes(cardName) || (inside.length >= 4 && (cardName.startsWith(inside) || inside.includes(cardName)));
  };
  return cardChoices.audio.find(matches)?.id ?? '';
}

function renderCard() {
  const cards = cardChoices?.cards ?? [];
  const audio = cardChoices?.audio ?? [];
  const device = $('captureCardDevice');
  device.replaceChildren(...cards.map((c) => new Option(c.name, c.id)));
  if (settings.captureCardDevice && !cards.some((c) => c.id === settings.captureCardDevice)) {
    device.add(new Option(cardChoices ? 'Saved card (not connected)' : 'Saved card', settings.captureCardDevice));
  }
  const none = cardChoices ? 'No capture card found' : cardChoices === undefined ? 'Looking for capture cards' : "Capture cards couldn't be listed";
  if (!device.options.length) device.add(new Option(none, ''));
  // Never chosen: suggest a card that looks like one, since webcams are listed too.
  const suggested = cards.find((c) => /4k x|game capture|elgato|capture|hdmi/i.test(c.name))?.id ?? cards[0]?.id ?? '';
  device.value = settings.captureCardDevice || suggested;

  const sound = $('captureCardAudio');
  sound.replaceChildren(new Option('No sound from the card', ''), ...audio.map((a) => new Option(a.name, a.id)));
  if (settings.captureCardAudio && !audio.some((a) => a.id === settings.captureCardAudio)) {
    sound.add(new Option(cardChoices ? 'Saved audio (not connected)' : 'Saved audio', settings.captureCardAudio));
  }
  sound.value = settings.captureCardDevice ? settings.captureCardAudio : matchingCardAudio(device.value);

  const bitrate = $('cardBitrateKbps');
  bitrate.replaceChildren(...BITRATES.map(([kbps, label]) => new Option(label, kbps)));
  ensureOption(bitrate, settings.cardBitrateKbps, `Custom (${settings.cardBitrateKbps / 1000} Mbps)`);
  bitrate.value = settings.cardBitrateKbps;
  $('cardResolution').value = settings.cardResolution;
  $('cardFps').value = settings.cardFps;

  const on = $('captureCard').checked;
  device.disabled = !on || !device.value;
  sound.disabled = !on || !device.value;
  for (const id of ['cardResolution', 'cardFps', 'cardBitrateKbps', 'cardHdr', 'cardAudioOwnTrack', 'cardPauseWithoutSignal']) $(id).disabled = !on;
  $('cardHdr').checked = settings.cardHdr;
  $('cardAudioOwnTrack').checked = settings.cardAudioOwnTrack;
  $('cardPauseWithoutSignal').checked = settings.cardPauseWithoutSignal;
  if (!$('card-level').childElementCount) {
    $('card-level').append(levelControl(CARD_AUDIO_INDEX, settings.cardAudioVolume, (v) => { settings.cardAudioVolume = v; }));
  }
  renderCardModes();
  const doubled = on && cardChoices?.doubled;
  $('card-doubled').hidden = !doubled;
  $('card-doubled').textContent = doubled ? `Your audio source ${doubled} is the capture card's sound too, so it would be recorded twice. Choose your real mic in Settings, under Audio.` : '';
  updateLiveViews();
}

/**
 * Greys out the card sizes and frame rates the chosen card doesn't send, uncompressed, and says what happens
 * with one that's chosen anyway. Nothing is greyed out while the card's sizes aren't known.
 */
function renderCardModes() {
  const device = $('captureCardDevice').value;
  const card = cardChoices?.cards.find((c) => c.id === device);
  // Read in the background the first time, as it takes seconds; until then nothing is greyed out.
  if (card && !(device in cardModes)) {
    cardModes[device] = null;
    api.invoke('card-modes', device).then((modes) => { cardModes[device] = modes ?? []; }, () => { delete cardModes[device]; }).then(renderCardModes);
  }
  const modes = cardModes[device] ?? [];
  // The same 16:9 size as the Rust side's with_card.
  const sizeOf = (height) => ({ width: Math.floor((height * 16 / 9 + 1) / 2) * 2, height });
  const fastest = (height) => {
    const { width } = sizeOf(height);
    return Math.max(0, ...modes.filter((m) => m.width === width && m.height === height).map((m) => m.fps));
  };
  // 59.94 counts as 60.
  const sends = (height, fps) => fastest(height) >= fps * 0.99;
  const height = Number($('cardResolution').value);
  const fps = Number($('cardFps').value);
  for (const option of $('cardResolution').options) option.disabled = modes.length > 0 && !fastest(Number(option.value));
  for (const option of $('cardFps').options) option.disabled = modes.length > 0 && !sends(height, Number(option.value));
  let note = '';
  if (modes.length && !fastest(height)) {
    note = `${card.name} doesn't send this size, so it records the nearest size it does, scaled to fit.`;
  } else if (modes.length && !sends(height, fps)) {
    note = `${card.name} sends this size at up to ${Math.round(fastest(height))} fps, so it records at that.`;
  }
  $('card-mode-note').hidden = !note || !$('captureCard').checked;
  $('card-mode-note').textContent = note;
}

function renderPresets(failures = []) {
  $('presets').replaceChildren(...settings.presets.map((p, i) => {
    const row = document.createElement('div');
    row.className = 'preset';
    const seconds = document.createElement('input');
    seconds.type = 'number';
    seconds.min = 0;
    seconds.value = p.seconds;
    seconds.title = 'Seconds to keep. 0 keeps the whole buffer.';
    seconds.setAttribute('aria-label', 'Clip seconds');
    seconds.oninput = () => { p.seconds = Number(seconds.value) || 0; scheduleSave(TYPING_DELAY_MS); };
    const key = document.createElement('input');
    key.setAttribute('aria-label', 'Clip hotkey');
    bindHotkey(key, () => p.hotkey, (v) => { p.hotkey = v; scheduleSave(); });
    key.classList.toggle('failed', failures.includes(p.hotkey));
    if (failures.includes(p.hotkey)) key.title = 'Another app already holds this hotkey';
    const del = document.createElement('button');
    del.className = 'remove';
    del.textContent = '×';
    del.setAttribute('aria-label', 'Remove clip length');
    del.onclick = () => { settings.presets.splice(i, 1); renderPresets(); scheduleSave(); };
    row.append(seconds, key, del);
    return row;
  }));
  $('recordHotkey').classList.toggle('failed', failures.includes(settings.recordHotkey));
}

const MAX_MARKER_KEYS = 8;
/** Named marker keys: a name and a hotkey each. */
function renderMarkerKeys(failures = lastFailures ? lastFailures.split(',') : []) {
  const keys = settings.markerKeys;
  $('marker-keys').replaceChildren(...keys.map((k, i) => {
    const row = document.createElement('div');
    row.className = 'preset marker-key';
    const label = document.createElement('input');
    label.value = k.label;
    label.maxLength = 30;
    label.placeholder = 'Name, such as clutch';
    label.setAttribute('aria-label', 'Marker name');
    label.oninput = () => { k.label = label.value; scheduleSave(TYPING_DELAY_MS); };
    const key = document.createElement('input');
    key.setAttribute('aria-label', `Hotkey for ${k.label || 'this marker'}`);
    bindHotkey(key, () => k.hotkey, (v) => { k.hotkey = v; scheduleSave(); });
    key.classList.toggle('failed', !!k.hotkey && failures.includes(k.hotkey));
    const del = document.createElement('button');
    del.className = 'remove';
    del.textContent = '×';
    del.setAttribute('aria-label', 'Remove named marker');
    del.onclick = () => { keys.splice(i, 1); renderMarkerKeys(); scheduleSave(); };
    row.append(label, key, del);
    return row;
  }));
  $('add-marker-key').disabled = keys.length >= MAX_MARKER_KEYS;
}

function renderAutoClip() {
  $('auto-clip-details').hidden = !$('autoClip').checked;
  const level = Number($('autoClipLevelDb').value);
  $('auto-clip-level').textContent = `${level} dB`;
  $('auto-clip-line').style.left = `${loudPercent(level)}%`;
}

// The meter shows -50 dB to 0 dB, so the slider's whole range fits with some room below it.
const LOUD_FLOOR_DB = -50;
const loudPercent = (db) => Math.max(0, Math.min(100, ((db - LOUD_FLOOR_DB) / -LOUD_FLOOR_DB) * 100));

const kindNames = { output: 'Output', input: 'Input', app: 'App' };
const appLabel = (choice) => {
  const exe = choice.id.split(':').pop().replace(/#3A/g, ':').replace(/#22/g, '#').replace(/\.exe$/i, '');
  return exe ? exe.charAt(0).toUpperCase() + exe.slice(1) : choice.name;
};

/** Running apps, one per program, with Discord first since it's the usual reason to add one. */
function appChoices() {
  const seen = new Map();
  for (const c of audioChoices.apps) {
    const label = appLabel(c);
    if (/^afterclip$/i.test(label)) continue;
    if (!seen.has(label.toLowerCase())) seen.set(label.toLowerCase(), { id: c.id, name: label });
  }
  return [...seen.values()].sort((a, b) => (/discord/i.test(b.name) - /discord/i.test(a.name)) || a.name.localeCompare(b.name));
}

const choicesFor = (source) => (source.kind === 'app' ? appChoices() : source.kind === 'input' ? audioChoices.inputs : audioChoices.outputs);

function renderAudio() {
  $('audio-list').replaceChildren(...settings.audioSources.map((source, i) => {
    const row = document.createElement('div');
    row.className = 'audio-row';
    const top = document.createElement('div');
    top.className = 'audio-top';
    const tag = document.createElement('span');
    tag.className = 'audio-kind';
    tag.textContent = kindNames[source.kind];
    const select = document.createElement('select');
    select.setAttribute('aria-label', `${kindNames[source.kind]} source`);
    const choices = choicesFor(source);
    for (const c of choices) select.add(new Option(c.sleekmeeterName ?? c.name, c.id));
    // Keep a saved source listed even if it isn't available right now, such as Discord being closed.
    if (source.device && !choices.some((c) => c.id === source.device)) {
      select.add(new Option(`${source.label} (not running)`, source.device));
    }
    select.value = source.device;
    select.onchange = () => {
      source.device = select.value;
      source.label = choices.find((choice) => choice.id === source.device)?.name ?? source.label;
      checkAudio();
      scheduleSave();
    };
    const del = document.createElement('button');
    del.className = 'more';
    del.textContent = '×';
    const displayName = choices.find((choice) => choice.id === source.device)?.sleekmeeterName ?? source.label;
    del.setAttribute('aria-label', `Remove ${displayName}`);
    del.onclick = () => { settings.audioSources.splice(i, 1); renderAudio(); scheduleSave(); };
    top.append(tag, select, del);

    const options = document.createElement('div');
    options.className = 'audio-options';
    const toggle = (label, key, value) => {
      const l = document.createElement('label');
      const c = document.createElement('input');
      c.type = 'checkbox';
      c.checked = value;
      c.onchange = () => { source[key] = c.checked; checkAudio(); scheduleSave(); };
      l.append(c, label);
      return l;
    };
    // Matches AudioSource::muted_by_key in the backend: inputs, but not cables, which usually carry apps.
    const mutes = source.muteWithKey ?? (source.kind === 'input' && !/cable/i.test(source.label));
    options.append(toggle('Main mix', 'mix', source.mix), toggle('Own track', 'ownTrack', source.ownTrack), toggle('Mute key', 'muteWithKey', mutes));
    row.append(top, levelControl(i, source.volume ?? 1, (v) => { source.volume = v; }), options);
    return row;
  }));
  checkAudio();
}

/** A volume slider and live meter for one source. Volume changes straight away, without restarting capture. */
function levelControl(index, volume, remember) {
  const wrap = document.createElement('div');
  wrap.className = 'level-row';
  const top = document.createElement('div');
  top.className = 'level-top';
  const slider = document.createElement('input');
  slider.type = 'range';
  slider.className = 'volume';
  slider.min = 0;
  slider.max = 200;
  slider.step = 5;
  slider.value = Math.round(volume * 100);
  slider.setAttribute('aria-label', 'Volume');
  const value = document.createElement('span');
  value.className = 'volume-value';
  value.textContent = `${slider.value}%`;
  slider.oninput = () => {
    value.textContent = `${slider.value}%`;
    const v = Number(slider.value) / 100;
    remember(v);
    api.invoke('set-volume', { index, volume: v }).catch(() => undefined);
    scheduleSave(TYPING_DELAY_MS);
  };
  top.append(slider, value);
  const meter = document.createElement('div');
  meter.className = 'meter';
  meter.setAttribute('role', 'img');
  meter.setAttribute('aria-label', 'Audio level');
  const fill = document.createElement('div');
  fill.className = 'meter-fill';
  fill.dataset.meter = index;
  meter.append(fill);
  wrap.append(top, meter);
  return wrap;
}

const METER_FLOOR_DB = -60;
const METER_MS = 100;
const HOME_STATS_MS = 2000;
const PANE_STATS_MS = 1000;
const OVERLOAD_PERCENT = 1; // Above this share of recent frames lost, it's worth a warning.
let meterTimer = null;
let statsTimer = null;
let loudTimer = null;
const LOUD_MS = 250;
let lastStats = null;

/** Meters and stats run only while on screen, so they cost nothing during play. */
function updateLiveViews() {
  const visible = !document.hidden && document.hasFocus();
  const cardMeter = currentPane === 'video' && $('captureCard').checked;
  const metersWanted = visible && (currentPane === 'audio' || cardMeter) && !$('settings').hidden;
  if (metersWanted && !meterTimer) {
    api.invoke('set-meters', true).catch(() => undefined);
    meterTimer = setInterval(pollLevels, METER_MS);
  } else if (!metersWanted && meterTimer) {
    clearInterval(meterTimer);
    meterTimer = null;
    api.invoke('set-meters', false).catch(() => undefined);
    document.querySelectorAll('[data-meter]').forEach((f) => { f.style.transform = 'scaleX(0)'; });
  }
  const loudWanted = visible && currentPane === 'controls' && !$('settings').hidden && $('autoClip').checked;
  if (loudWanted && !loudTimer) loudTimer = setInterval(pollLoud, LOUD_MS);
  else if (!loudWanted && loudTimer) { clearInterval(loudTimer); loudTimer = null; }
  const paneOpen = !$('settings').hidden && currentPane === 'video';
  const homeOpen = $('settings').hidden;
  const wanted = visible && (paneOpen || homeOpen) ? (paneOpen ? PANE_STATS_MS : HOME_STATS_MS) : 0;
  if (statsTimer?.ms !== wanted) {
    if (statsTimer) clearInterval(statsTimer.id);
    statsTimer = wanted ? { ms: wanted, id: setInterval(pollStats, wanted) } : null;
    lastStats = null;
    frameProblem = '';
    renderBuffer();
    if (wanted) pollStats();
  }
}

let loudBusy = false;
async function pollLoud() {
  if (loudBusy) return;
  loudBusy = true;
  const db = await api.invoke('loud-level', $('autoClipSource').value).catch(() => null);
  loudBusy = false;
  const fill = $('auto-clip-fill');
  const note = $('auto-clip-now');
  if (typeof db !== 'number') {
    fill.style.width = '0%';
    note.textContent = $('autoClipSource').value === 'game'
      ? 'Open your game to see how loud it is. It must be in front.'
      : 'No mic to listen to. Add one in Audio, and unmute it in clips.';
    return;
  }
  fill.style.width = `${loudPercent(db)}%`;
  const over = db >= Number($('autoClipLevelDb').value);
  fill.classList.toggle('over', over);
  note.textContent = `Hearing ${db <= -99 ? 'silence' : `${Math.round(db)} dB`} now${over ? ', which would count as a moment' : ''}. Set the line just above your normal talking.`;
}

async function pollLevels() {
  const levels = await api.invoke('levels').catch(() => null);
  if (!Array.isArray(levels)) return;
  for (const { index, db } of levels) {
    const fill = document.querySelector(`[data-meter="${index}"]`);
    if (fill) fill.style.transform = `scaleX(${Math.max(0, Math.min(1, (db - METER_FLOOR_DB) / -METER_FLOOR_DB))})`;
  }
}

const sizeFromMb = (mb) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb} MB`);

async function pollStats() {
  const s = await api.invoke('stats').catch(() => null);
  if (!s) return;
  // Judged on frames since the last look, so one old hiccup doesn't warn forever.
  const previous = lastStats ?? s;
  const share = (bad, total) => (total > 0 ? (bad / total) * 100 : 0);
  const encoder = share(s.encoderSkipped - previous.encoderSkipped, s.encoderTotal - previous.encoderTotal);
  const render = share(s.renderLagged - previous.renderLagged, s.renderTotal - previous.renderTotal);
  lastStats = s;
  const problem = encoder > OVERLOAD_PERCENT
    ? 'Encoder overloaded: lower the quality or use a faster preset.'
    : render > OVERLOAD_PERCENT ? 'GPU is too busy, so some frames are being missed.' : '';
  const fps = Math.round(s.fps);
  if (problem !== frameProblem) {
    frameProblem = problem;
    renderBuffer();
  }
  $('perf-health').textContent = problem || 'Everything is keeping up.';
  $('perf-health').classList.toggle('warn', !!problem);
  $('stat-fps').textContent = `${fps} fps`;
  $('stat-encoder').textContent = `${s.encoderSkipped} of ${s.encoderTotal}`;
  $('stat-render').textContent = `${s.renderLagged} of ${s.renderTotal}`;
  $('stat-cpu').textContent = `${s.cpu.toFixed(1)}%`;
  $('stat-memory').textContent = sizeFromMb(s.memoryMb);
  $('stat-disk').textContent = sizeFromMb(s.diskFreeMb);
}

function checkAudio() {
  const warnings = [];
  const silent = settings.audioSources.filter((s) => !s.mix && !s.ownTrack);
  if (silent.length) warnings.push(`${silent[0].label} won't be recorded, because it's in neither the main mix nor its own track.`);
  const own = settings.audioSources.filter((s) => s.ownTrack).length;
  if (own > 5) warnings.push('Only 5 sources can have their own track. Extra ones stay in the main mix only.');
  const desktopInMix = settings.audioSources.some((s) => s.kind === 'output' && s.mix);
  const appInMix = settings.audioSources.find((s) => s.kind === 'app' && s.mix);
  if (desktopInMix && appInMix) warnings.push(`${appInMix.label} is already in your desktop audio, so with both in the main mix you'll hear it twice.`);
  $('audio-warning').hidden = warnings.length === 0;
  $('audio-warning').textContent = warnings.join(' ');
}

async function loadAudioChoices() {
  try {
    audioChoices = await api.invoke('audio-choices');
  } catch {
    audioChoices = { outputs: [], inputs: [], apps: [] }; // The engine is restarting. Saved sources still list.
  }
  renderAudio();
}

function addAudio(addKind) {
  const choices = choicesFor({ kind: addKind });
  if (!choices.length) {
    $('save-note').textContent = addKind === 'app' ? 'No apps found. Open the app first, such as Discord.' : 'No devices found.';
    return;
  }
  const first = choices[0];
  // An app is normally part of desktop audio already, so by default it only gets its own track.
  const isApp = addKind === 'app';
  settings.audioSources.push({ kind: addKind, device: first.id, label: first.name, mix: !isApp, ownTrack: isApp });
  renderAudio();
  scheduleSave();
}

async function renderEstimate() {
  const seconds = Number($('bufferMinutes').value) * 60;
  // The card has its own quality, so the memory it needs follows that while it's on.
  const bitrateKbps = Number($($('captureCard').checked ? 'cardBitrateKbps' : 'bitrateKbps').value);
  const mb = await api.invoke('estimate', { bufferSeconds: seconds, bitrateKbps });
  const size = (n) => (n >= 1024 ? `${(n / 1024).toFixed(1)} GB` : `${n} MB`);
  const note = $('estimate');
  if ($('diskBuffer').checked) {
    // Only the last 40 seconds stay in memory. The rest is on disk, plus up to one file being written.
    const ramMb = await api.invoke('estimate', { bufferSeconds: Math.min(seconds, 40), bitrateKbps });
    const diskMb = await api.invoke('estimate', { bufferSeconds: seconds + 60, bitrateKbps });
    note.textContent = `Uses about ${size(ramMb)} of memory and up to ${size(diskMb)} on your save drive at this quality and length.`;
    note.classList.remove('warn');
    return;
  }
  installedMb ??= await api.invoke('installed-memory-mb').catch(() => null);
  // A quarter of this PC's memory, or a fixed size when that can't be read.
  const large = installedMb ? mb >= installedMb * LARGE_BUFFER_SHARE : mb >= LARGE_BUFFER_MB;
  note.textContent = `Uses about ${size(mb)} of memory at this quality and length.`;
  if (large && installedMb) {
    note.textContent += ` That's over a quarter of this PC's ${size(installedMb)}, so games may slow down. Shorten the buffer, lower the quality, or turn on Keep the buffer on disk.`;
  }
  note.classList.toggle('warn', large);
}

const LARGE_BUFFER_MB = 4096;
const LARGE_BUFFER_SHARE = 0.25;
let installedMb = null;
const BITRATES = [[8000, 'Low (8 Mbps)'], [20000, 'Medium (20 Mbps)'], [40000, 'High (40 Mbps)'], [60000, 'Very high (60 Mbps)'], [80000, 'Ultra (80 Mbps)'], [100000, 'Extreme (100 Mbps)'], [150000, 'Maximum (150 Mbps)']];
const HEIGHTS = [2160, 1440, 1080, 720];
const QUALITY = {
  low: { resolution: '720', frameRate: '60', kbps: 8000, preset: 'p1', multipass: false, blurb: 'Smallest files and the least work for your PC.' },
  medium: { resolution: '1080', frameRate: '60', kbps: 20000, preset: 'p3', multipass: false, blurb: 'A good balance for sharing, and light on your PC while you play.' },
  high: { resolution: 'native', frameRate: '60', kbps: 40000, preset: 'p3', multipass: false, blurb: 'Your monitor\'s full resolution, with low encode load.' },
  full: { resolution: 'native', frameRate: 'native', kbps: 150000, preset: 'p5', multipass: true, blurb: 'The most your monitor and encoder can give. It uses the most GPU and memory and can lower your frame rate in games, and files are very large. Shorten the buffer if needed.' },
};
const ENCODER_NAMES = {
  obs_nvenc_h264_tex: 'NVIDIA NVENC H.264', obs_nvenc_hevc_tex: 'NVIDIA NVENC HEVC', obs_nvenc_av1_tex: 'NVIDIA NVENC AV1',
  h264_texture_amf: 'AMD AMF H.264', h265_texture_amf: 'AMD AMF HEVC', av1_texture_amf: 'AMD AMF AV1',
  obs_qsv11_v2: 'Intel QuickSync H.264', obs_qsv11_hevc: 'Intel QuickSync HEVC', obs_qsv11_av1: 'Intel QuickSync AV1', obs_x264: 'x264 (CPU) H.264',
};
let display = null;

/** Lists an option that came from older settings, so a value is never silently changed by showing it. */
function ensureOption(select, value, label) {
  if (![...select.options].some((o) => o.value === String(value))) select.add(new Option(label, value));
}

function fillVideoOptions() {
  const resolution = $('resolution');
  const native = display ? `Native (${display.width}×${display.height})` : 'Native';
  const heights = HEIGHTS.filter((h) => !display || h < display.height);
  resolution.replaceChildren(new Option(native, 'native'), ...heights.map((h) => new Option(`${h}p`, String(h))));
  if (settings.resolution === 'custom') resolution.add(new Option(`Custom (${settings.width}×${settings.height})`, 'custom'));
  resolution.value = settings.resolution;

  const frameRate = $('frameRate');
  frameRate.replaceChildren(new Option('60 fps', '60'), new Option('120 fps', '120'), new Option(display ? `Native (${display.refresh} fps)` : 'Native', 'native'));
  if (settings.frameRate === 'custom') frameRate.add(new Option(`Custom (${settings.fps} fps)`, 'custom'));
  frameRate.value = settings.frameRate;

  const bitrate = $('bitrateKbps');
  bitrate.replaceChildren(...BITRATES.map(([kbps, label]) => new Option(label, kbps)));
  ensureOption(bitrate, settings.bitrateKbps, `Custom (${settings.bitrateKbps / 1000} Mbps)`);
  bitrate.value = settings.bitrateKbps;
}

function describeQuality() {
  const preset = QUALITY[$('quality').value];
  $('quality-note').textContent = preset ? preset.blurb : 'Your own mix of settings.';
}

/** Choosing a profile sets every field below it. */
function applyQuality() {
  const preset = QUALITY[$('quality').value];
  if (preset) {
    const height = Number(preset.resolution);
    // A monitor below the profile's height uses native, because scaling up adds size without detail.
    $('resolution').value = display && height >= display.height ? 'native' : preset.resolution;
    $('frameRate').value = preset.frameRate;
    $('bitrateKbps').value = preset.kbps;
    $('encoderPreset').value = preset.preset;
    $('multipass').value = String(preset.multipass);
  }
  describeQuality();
  renderEstimate();
}

/** Changing a field by hand makes the profile Custom. */
function videoFieldChanged() {
  $('quality').value = 'custom';
  describeQuality();
  renderEstimate();
  scheduleSave();
}

function renderEncoderNote(inUse) {
  const name = ENCODER_NAMES[inUse] ?? inUse;
  $('encoder-note').textContent = name ? `Currently recording with ${name}.` : '';
}

/* Updates */

const UPDATE_COUNTDOWN_REFRESH_MS = 30000;
let updateTimer = null;

// Patch notes are plain text from CHANGELOG.md: lines starting with "- " become a list.
function fillNotes(box, text) {
  const parts = [];
  let list = null;
  for (const raw of (text ?? '').split(/\r?\n/)) {
    const lineText = raw.trim();
    if (!lineText) { list = null; continue; }
    if (lineText.startsWith('- ')) {
      if (!list) parts.push(list = document.createElement('ul'));
      const li = document.createElement('li');
      li.textContent = lineText.slice(2);
      list.append(li);
    } else {
      list = null;
      const p = document.createElement('p');
      p.textContent = lineText;
      parts.push(p);
    }
  }
  box.replaceChildren(...parts);
}

// Asked once on the home page, after any update, patch notes or Stream Deck offer. Until then nothing is sent.
function renderUsageOffer() {
  const busy = ['update-card', 'whats-new', 'deck-offer'].some((id) => !$(id).hidden);
  $('usage-offer').hidden = !settings || typeof settings.usagePing === 'boolean' || busy;
}

function answerUsage(yes) {
  settings.usagePing = yes;
  $('usagePing').checked = yes;
  renderUsageOffer();
  scheduleSave();
  if (yes) showBanner('Thanks! You can turn this off in Settings, under Privacy.', 'good');
}

function renderStreamDeck() {
  const upgrade = streamDeck.offer === 'update';
  $('deck-offer').hidden = !streamDeck.offer;
  $('deck-offer-title').textContent = upgrade ? 'Stream Deck plugin update' : 'Stream Deck found';
  $('deck-offer-text').textContent = upgrade
    ? 'A newer Afterclip plugin is ready, with the keys from this version.'
    : 'Add Afterclip\'s keys to your Stream Deck to clip, record and mute your mic with a press.';
  $('deck-install').textContent = upgrade ? 'Update plugin' : 'Install plugin';
  // Settings offers it again after Not now, for as long as the Stream Deck app is missing it or has an older one.
  $('deck-plugin-row').hidden = !streamDeck.available;
  $('deck-plugin-install').textContent = streamDeck.available === 'update' ? 'Update' : 'Install';
}

function installStreamDeckPlugin() {
  return api.invoke('install-stream-deck-plugin').then(() => {
    streamDeck = { ...streamDeck, offer: null };
    renderStreamDeck();
    showBanner('Finish in the Stream Deck app. Afterclip\'s keys are in its Afterclip category.', 'good');
  }).catch((e) => showBanner(errorText(e), 'bad'));
}

function renderPatchNotes() {
  // A local test build, such as 1.6.0-dev, has the notes of the release it becomes.
  const release = appVersion.replace(/-dev$/, '');
  const unseen = Boolean(patchNotes && appVersion && seenVersion !== release);
  $('whats-new').hidden = !unseen;
  if (unseen) {
    $('whats-new-title').textContent = `What's new in ${release}`;
    fillNotes($('whats-new-notes'), patchNotes);
  }
  $('version-notes').hidden = !patchNotes;
  fillNotes($('version-notes').querySelector('.patch-notes'), patchNotes);
}

function renderUpdate() {
  renderPatchNotes();
  const { status: state, version, message, installsAt, notes: updateNotes } = update;
  $('update-notes').hidden = state !== 'ready' || !updateNotes;
  fillNotes($('update-notes').querySelector('.patch-notes'), state === 'ready' ? updateNotes : '');
  const card = $('update-card');
  card.hidden = !['downloading', 'ready', 'installing', 'failed'].includes(state);
  $('update-actions').hidden = state !== 'ready';
  const line = (heading, detail) => {
    const b = document.createElement('b');
    b.textContent = heading;
    $('update-text').replaceChildren(b, detail);
  };
  if (state === 'downloading') line(`Downloading update ${version}`, 'It installs when you say so, or when you\'re not playing.');
  if (state === 'installing') line(`Installing update ${version}`, 'Afterclip will restart by itself.');
  if (state === 'failed') line('Update problem', message ?? 'The update didn\'t work. Afterclip will try again later.');
  if (state === 'ready') {
    const minutes = installsAt ? Math.max(1, Math.ceil((installsAt - Date.now()) / 60000)) : null;
    line(`Update ${version} is ready`, minutes ? `Installs in about ${minutes} min unless you skip it. Waits while you're in a game or recording.` : 'Restarting clears the buffer, so install it when it suits you.');
  }
  clearInterval(updateTimer);
  updateTimer = state === 'ready' && installsAt ? setInterval(renderUpdate, UPDATE_COUNTDOWN_REFRESH_MS) : null;

  $('version-note').textContent = appVersion ? `Version ${appVersion}` : '';
  const notes = {
    checking: 'Checking for updates.',
    current: 'You have the latest version.',
    downloading: `Downloading ${version}.`,
    ready: `Version ${version} is ready. See the top of the main screen.`,
    failed: message ?? '',
  };
  const skipped = settings?.skippedUpdate;
  $('update-note').textContent = notes[state] ?? (skipped ? `You skipped version ${skipped}. Check now to get it anyway.` : '');
  $('check-update').disabled = ['checking', 'downloading', 'installing'].includes(state);
}

/* Stream Deck on another PC */

function renderRemote() {
  $('remote-details').hidden = !remote.on;
  $('remote-address').value = remote.address ?? '';
  const note = $('remote-note');
  const problem = remote.error ?? (remote.on && !remote.address ? 'This PC isn\'t on a network, so there\'s no address yet.' : '');
  note.textContent = problem || 'On the other PC, open any Afterclip key\'s settings in the Stream Deck app, and paste this into Address.';
  note.classList.toggle('warn', !!problem);
  // iPhone Shortcuts take the key as a header, not in the address as the Stream Deck does.
  const address = remote.address ? new URL(remote.address) : null;
  $('shortcut-url').value = address ? address.origin : '';
  $('shortcut-header').value = address ? `Bearer ${address.searchParams.get('k') ?? ''}` : '';
}

/* Storage */

function renderStorageUsed() {
  const gb = storageUsed / 1e9;
  $('storage-used').textContent = `Your clips and recordings use ${gb >= 0.1 ? `${gb.toFixed(1)} GB` : `${Math.round(storageUsed / 1e6)} MB`}.`;
}

/* Controller */

const CONTROLLER_COMBOS = [
  ['', 'Off'],
  ['r3', 'R3 (press right stick)'],
  ['north', 'Triangle / Y / X'],
  ['east', 'Circle / B / A'],
  ['west', 'Square / X / Y'],
  ['south', 'Cross / A / B'],
  ['up', 'D-pad Up'],
  ['right', 'D-pad Right'],
  ['down', 'D-pad Down'],
  ['left', 'D-pad Left'],
];
const CONTROLLER_ACTIONS = ['controllerClip', 'controllerRecord', 'controllerMic', 'controllerMarker', 'controllerScreenshot'];
let controllersSeen = [];

function renderControllersSeen() {
  const on = settings?.controllerEnabled;
  $('controller-seen').textContent = !on ? 'Off. Your controller isn\'t read at all.'
    : controllersSeen.length ? `Found: ${controllersSeen.join(', ')}` : 'On. Press a button on your controller to check it\'s found.';
}

function fillController() {
  $('controllerEnabled').checked = settings.controllerEnabled;
  for (const id of CONTROLLER_ACTIONS) {
    $(id).replaceChildren(...CONTROLLER_COMBOS.map(([value, text]) => new Option(text, value)));
    $(id).value = settings[id];
  }
  const lengths = $('controllerClipSeconds');
  lengths.replaceChildren(...settings.presets.map((p) => new Option(p.seconds ? `${p.seconds} seconds` : 'Whole buffer', p.seconds)));
  ensureOption(lengths, settings.controllerClipSeconds, `${settings.controllerClipSeconds} seconds`);
  lengths.value = settings.controllerClipSeconds;
  renderControllersSeen();
}

for (const id of CONTROLLER_ACTIONS) {
  $(id).onchange = () => {
    // One combination runs one action, so choosing a taken one moves it here.
    for (const other of CONTROLLER_ACTIONS) {
      if (other !== id && $(id).value && $(other).value === $(id).value) {
        $(other).value = '';
        settings[other] = '';
      }
    }
    settings[id] = $(id).value;
    scheduleSave();
  };
}
$('controllerClipSeconds').onchange = () => { settings.controllerClipSeconds = Number($('controllerClipSeconds').value); scheduleSave(); };
$('controllerEnabled').onchange = () => { settings.controllerEnabled = $('controllerEnabled').checked; renderControllersSeen(); scheduleSave(); };
api.on('controllers', (seen) => { controllersSeen = seen; renderControllersSeen(); });

/* Per-game settings */

const GAME_ICON_SIZE = 48;
let pickerShown = null;

function pickerIcon(pixels) {
  if (!pixels) {
    const blank = document.createElement('div');
    blank.className = 'picker-blank';
    return blank;
  }
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = GAME_ICON_SIZE;
  const bytes = Uint8ClampedArray.from(atob(pixels), (c) => c.charCodeAt(0));
  canvas.getContext('2d').putImageData(new ImageData(bytes, GAME_ICON_SIZE, GAME_ICON_SIZE), 0, 0);
  return canvas;
}

function pickerRow(app) {
  const row = document.createElement('button');
  row.className = 'picker-row';
  row.dataset.exe = app.exe;
  const text = document.createElement('div');
  text.className = 'picker-text';
  const name = document.createElement('div');
  name.textContent = app.name;
  const exe = document.createElement('div');
  exe.className = 'profile-exe';
  exe.textContent = app.exe;
  text.append(name, exe);
  const add = document.createElement('span');
  add.className = 'picker-add';
  add.textContent = 'Add';
  row.append(pickerIcon(app.icon), text, add);
  row.onclick = () => addProfile(app.exe, app.name);
  return row;
}

function pickerGroup(title, apps) {
  if (!apps.length) return [];
  const label = document.createElement('div');
  label.className = 'picker-group';
  label.textContent = title;
  return [label, ...apps.map(pickerRow)];
}

/** Lists what's open, and only redraws when that changes, so a row isn't replaced as it's clicked. */
async function renderGamePicker() {
  if (currentPane !== 'games' || !settings) return;
  const running = await api.invoke('running-apps');
  const apps = running.filter((a) => !settings.gameProfiles.some((p) => p.exe === a.exe));
  const shown = apps.map((a) => `${a.exe}|${a.name}|${a.game}`).join('\n');
  if (shown === pickerShown) return;
  pickerShown = shown;
  const empty = document.createElement('div');
  empty.className = 'picker-empty';
  empty.textContent = 'Open your game and it will appear here.';
  $('game-picker').replaceChildren(...(apps.length ? [
    ...pickerGroup('Games', apps.filter((a) => a.game)),
    ...pickerGroup('Other open apps', apps.filter((a) => !a.game)),
  ] : [empty]));
}
setInterval(() => { if (!document.hidden) renderGamePicker(); }, 3000);

function profileSelect(label, value, options, onChange) {
  const row = document.createElement('div');
  row.className = 'setting';
  const name = document.createElement('div');
  name.className = 'setting-text setting-name';
  name.textContent = label;
  const select = document.createElement('select');
  select.setAttribute('aria-label', label);
  for (const [v, text] of options) {
    const o = document.createElement('option');
    o.value = v;
    o.textContent = text;
    select.append(o);
  }
  select.value = value ?? '';
  select.onchange = () => { onChange(select.value); scheduleSave(); };
  row.append(name, select);
  return row;
}

const USE_DEFAULT = ['', 'Use my normal setting'];

function renderProfiles() {
  $('profiles').replaceChildren(...settings.gameProfiles.map((p) => {
    const card = document.createElement('div');
    card.className = 'card profile';
    card.dataset.exe = p.exe;
    const head = document.createElement('div');
    head.className = 'profile-head';
    const name = document.createElement('div');
    name.className = 'profile-name';
    name.textContent = p.name;
    const exe = document.createElement('div');
    exe.className = 'profile-exe';
    exe.textContent = p.exe;
    name.append(exe);
    const remove = document.createElement('button');
    remove.className = 'small danger';
    remove.textContent = 'Remove';
    remove.onclick = () => {
      settings.gameProfiles = settings.gameProfiles.filter((g) => g !== p);
      renderProfiles();
      renderGamePicker();
      scheduleSave();
    };
    head.append(name, remove);

    const record = document.createElement('div');
    record.className = 'setting';
    const recordText = document.createElement('div');
    recordText.className = 'setting-text';
    recordText.innerHTML = '<div class="setting-name">Auto-record</div><div class="setting-desc">Record the whole session when this game opens, and stop when it closes.</div>';
    const toggle = document.createElement('input');
    toggle.type = 'checkbox';
    toggle.className = 'switch auto-record';
    toggle.setAttribute('aria-label', `Auto-record ${p.name}`);
    toggle.checked = p.autoRecord;
    toggle.onchange = () => { p.autoRecord = toggle.checked; scheduleSave(); };
    record.append(recordText, toggle);

    const optional = (text) => text || undefined;
    const privacy = profileSelect('Capture privacy', p.capturePrivacy ? 'on' : 'off',
      [['off', 'Off'], ['on', 'Only while focused and hooked']], (value) => { p.capturePrivacy = value === 'on'; });
    privacy.title = 'Desktop and monitor fallback are disabled while any game has Capture privacy on.';
    card.append(
      head,
      record,
      privacy,
      profileSelect('Quality', p.quality, [USE_DEFAULT, ['low', 'Low'], ['medium', 'Medium'], ['high', 'High'], ['full', 'Full quality']], (v) => { p.quality = optional(v); }),
      profileSelect('Frame rate', p.frameRate, [USE_DEFAULT, ['60', '60 fps'], ['120', '120 fps'], ['native', 'Native']], (v) => { p.frameRate = optional(v); }),
      profileSelect('Buffer', p.bufferSeconds, [USE_DEFAULT, ['60', '1 minute'], ['120', '2 minutes'], ['300', '5 minutes'], ['600', '10 minutes'], ['1800', '30 minutes']], (v) => { p.bufferSeconds = v ? Number(v) : undefined; }),
      profileSelect('Game audio', p.gameAudio === undefined || p.gameAudio === null ? '' : String(p.gameAudio), [USE_DEFAULT, ['true', 'On'], ['false', 'Off']], (v) => { p.gameAudio = v ? v === 'true' : undefined; }),
      profileSelect('Vertical mode', p.vertical === undefined || p.vertical === null ? '' : String(p.vertical), [USE_DEFAULT, ['true', 'On'], ['false', 'Off']], (v) => { p.vertical = v ? v === 'true' : undefined; }),
    );
    return card;
  }));
}

function addProfile(exe, name) {
  const clean = exe.trim().toLowerCase();
  if (!clean) return;
  const file = clean.endsWith('.exe') ? clean : `${clean}.exe`;
  if (!settings.gameProfiles.some((p) => p.exe === file)) {
    settings.gameProfiles.push({ exe: file, name: name ?? file.replace(/\.exe$/, ''), autoRecord: false });
  }
  renderProfiles();
  renderGamePicker();
  scheduleSave();
}

const DISCORD_WEBHOOK = /^https:\/\/((ptb|canary)\.)?discord(app)?\.com\/api\/webhooks\//;
const MAX_WEBHOOKS = 10;

/** One row for each Discord channel: a name to tell them apart, and the hidden address. */
function renderWebhooks() {
  const hooks = settings.discordWebhooks;
  $('webhook-list').replaceChildren(...hooks.map((hook, index) => {
    const row = document.createElement('div');
    row.className = 'webhook-row';
    const name = document.createElement('input');
    name.value = hook.name;
    name.placeholder = 'Name';
    name.maxLength = 40;
    name.setAttribute('aria-label', `Name of channel ${index + 1}`);
    name.oninput = () => { hook.name = name.value; scheduleSave(TYPING_DELAY_MS); };
    const url = document.createElement('input');
    url.type = 'password';
    url.autocomplete = 'off';
    url.spellcheck = false;
    url.value = hook.url;
    url.placeholder = 'https://discord.com/api/webhooks/…';
    url.setAttribute('aria-label', `Webhook address for ${hook.name || `channel ${index + 1}`}`);
    const warning = document.createElement('div');
    warning.className = 'warn';
    const check = () => {
      const typed = url.value.trim();
      warning.textContent = typed && !DISCORD_WEBHOOK.test(typed) ? "That doesn't look like a Discord webhook address. It starts with https://discord.com/api/webhooks/" : '';
      warning.hidden = !warning.textContent;
    };
    url.oninput = () => { hook.url = url.value.trim(); check(); scheduleSave(TYPING_DELAY_MS); };
    check();
    const remove = document.createElement('button');
    remove.className = 'icon';
    remove.title = 'Remove this channel';
    remove.setAttribute('aria-label', `Remove ${hook.name || `channel ${index + 1}`}`);
    remove.innerHTML = '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>';
    remove.onclick = () => { hooks.splice(index, 1); renderWebhooks(); scheduleSave(); };
    row.append(name, url, remove, warning);
    return row;
  }));
  $('add-webhook').disabled = hooks.length >= MAX_WEBHOOKS;
}

/** The upload server's address, token and form field, with a warning for an address that isn't HTTPS. */
function renderUpload() {
  for (const key of ['uploadUrl', 'uploadToken', 'uploadField']) {
    const input = $(key);
    input.value = settings[key] ?? '';
    input.oninput = () => {
      settings[key] = input.value.trim();
      checkUpload();
      scheduleSave(TYPING_DELAY_MS);
    };
  }
  checkUpload();
}

function checkUpload() {
  const typed = $('uploadUrl').value.trim();
  let warning = '';
  if (typed) {
    try {
      if (new URL(typed).protocol !== 'https:') warning = 'The address must start with https://, so your clips and token are never sent unencrypted.';
    } catch {
      warning = "That doesn't look like a web address. It starts with https://";
    }
  }
  $('upload-warning').textContent = warning;
  $('upload-warning').hidden = !warning;
}

function fillForm() {
  for (const k of ['encoder', 'outputDir', 'quality', 'codec', 'encoderPreset']) $(k).value = settings[k];
  ensureOption($('storageLimitGb'), settings.storageLimitGb, `${settings.storageLimitGb} GB`);
  $('storageLimitGb').value = settings.storageLimitGb;
  ensureOption($('deleteAfterDays'), settings.deleteAfterDays, `After ${settings.deleteAfterDays} days`);
  $('deleteAfterDays').value = settings.deleteAfterDays;
  renderWebhooks();
  renderUpload();
  ensureOption($('starClipSeconds'), settings.starClipSeconds, `${settings.starClipSeconds} seconds`);
  $('starClipSeconds').value = settings.starClipSeconds;
  ensureOption($('markerClipSeconds'), settings.markerClipSeconds, `${settings.markerClipSeconds} seconds`);
  $('markerClipSeconds').value = settings.markerClipSeconds;
  ensureOption($('autoClipSeconds'), settings.autoClipSeconds, `${settings.autoClipSeconds} seconds`);
  for (const k of ['toastCorner', 'autoClipSource', 'autoClipSeconds', 'autoClipLevelDb']) $(k).value = settings[k];
  renderMarkerKeys();
  renderAutoClip();
  renderProfiles();
  renderStorageUsed();
  renderUpdate();
  $('multipass').value = String(settings.multipass);
  fillVideoOptions();
  describeQuality();
  const minutes = +(settings.bufferSeconds / 60).toFixed(2);
  ensureOption($('bufferMinutes'), minutes, `Custom (${settings.bufferSeconds} seconds)`);
  $('bufferMinutes').value = minutes;
  for (const k of SWITCHES) $(k).checked = settings[k];
  // Not in SWITCHES: it stays unanswered (null) until someone chooses.
  $('usagePing').checked = settings.usagePing === true;
  renderAutoClip();
  $('weeklyReelTag').value = settings.weeklyReelTag ? `#${settings.weeklyReelTag}` : '';
  renderWeeklyReel();
  ensureOption($('clipAfterSeconds'), settings.clipAfterSeconds, `${settings.clipAfterSeconds} seconds`);
  ensureOption($('cameraSize'), settings.cameraSize, `${settings.cameraSize}%`);
  ensureOption($('lowSpaceWarnGb'), settings.lowSpaceWarnGb, `${settings.lowSpaceWarnGb} GB`);
  for (const k of ['clipAfterSeconds', 'cameraCorner', 'cameraSize', 'lowSpaceWarnGb']) $(k).value = settings[k];
  bindTalkKey($('pushToTalkKey'));
  $('remotePort').value = settings.remotePort;
  renderCamera();
  renderCard();
  bindHotkey($('starClipHotkey'), () => settings.starClipHotkey, (v) => { settings.starClipHotkey = v; scheduleSave(); });
  bindHotkey($('recordHotkey'), () => settings.recordHotkey, (v) => { settings.recordHotkey = v; scheduleSave(); });
  bindHotkey($('micHotkey'), () => settings.micHotkey, (v) => { settings.micHotkey = v; scheduleSave(); });
  bindHotkey($('markerHotkey'), () => settings.markerHotkey, (v) => { settings.markerHotkey = v; scheduleSave(); });
  bindHotkey($('screenshotHotkey'), () => settings.screenshotHotkey, (v) => { settings.screenshotHotkey = v; scheduleSave(); });
  bindHotkey($('cardHotkey'), () => settings.cardHotkey, (v) => { settings.cardHotkey = v; scheduleSave(); });
  ensureOption($('recordSplit'), settings.recordSplitSeconds, `Every ${settings.recordSplitSeconds} seconds`);
  $('recordSplit').value = settings.recordSplitSeconds;
  $('game-level').replaceChildren(levelControl(-1, settings.gameAudioVolume, (v) => { settings.gameAudioVolume = v; }));
}

const SWITCHES = ['gameAudio', 'gameAudioOwnTrack', 'runAsAdmin', 'toastEnabled', 'beepEnabled', 'launchOnStartup', 'startMinimized', 'closeToTray', 'autoUpdate',
  'bufferOnlyInGames', 'cameraEnabled', 'verticalFaceCam', 'pushToTalk', 'gameFolders', 'remoteStreamDeck', 'verticalMode', 'evenAudio', 'autoClip', 'keepTagged',
  'diskBuffer', 'freeMemoryInGames', 'hdrCapture', 'weeklyReel', 'captureCard', 'extendOnDoublePress'];
const TYPING_DELAY_MS = 1000; // Long enough to finish typing, because a restart clears the buffer.
const NOTE_DEFAULT = 'Changes save automatically.';
let saveTimer = null;
let saving = false;
let saveAgain = false;
let lastFailures = '';

function scheduleSave(delay = 100) {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(saveNow, delay);
}

/** Sends the form to the backend. A change made while a save is running goes out right after it. */
async function saveNow() {
  clearTimeout(saveTimer);
  saveTimer = null;
  if (!settings) return;
  if (saving) { saveAgain = true; return; }
  saving = true;
  readForm();
  const note = $('save-note');
  try {
    const r = await api.invoke('save-settings', settings);
    const failed = r.hotkeyFailures.length ? ` These hotkeys are taken: ${r.hotkeyFailures.join(', ')}.` : '';
    note.classList.toggle('warn', !!failed);
    note.textContent = (r.restarted ? 'Saved. Capture restarted, so the buffer is starting over.' : 'Saved.') + failed;
    clearTimeout(saveNow.reset);
    if (!failed) saveNow.reset = setTimeout(() => { note.textContent = NOTE_DEFAULT; }, 5000);
    // Only redrawn when it changes, because redrawing takes focus from a field being typed in.
    const failures = r.hotkeyFailures.join();
    if (failures !== lastFailures) { lastFailures = failures; renderPresets(r.hotkeyFailures); renderMarkerKeys(r.hotkeyFailures); }
    settings.width = r.width;
    settings.height = r.height;
    settings.fps = r.fps;
    renderSummary();
    renderLengths();
    renderEstimate();
    refresh();
  } catch (e) {
    note.classList.add('warn');
    note.textContent = errorText(e);
  } finally {
    saving = false;
    if (saveAgain) { saveAgain = false; saveNow(); }
  }
}

function readForm() {
  settings.bitrateKbps = Number($('bitrateKbps').value);
  settings.recordSplitSeconds = Number($('recordSplit').value);
  settings.storageLimitGb = Number($('storageLimitGb').value);
  settings.deleteAfterDays = Number($('deleteAfterDays').value);
  settings.starClipSeconds = Number($('starClipSeconds').value);
  settings.markerClipSeconds = Number($('markerClipSeconds').value);
  for (const k of ['toastCorner', 'autoClipSource']) settings[k] = $(k).value;
  for (const k of ['autoClipSeconds', 'autoClipLevelDb']) settings[k] = Number($(k).value);
  settings.bufferSeconds = Math.round(Number($('bufferMinutes').value) * 60);
  for (const k of SWITCHES) settings[k] = $(k).checked;
  for (const k of ['quality', 'resolution', 'frameRate', 'codec', 'encoderPreset', 'encoder', 'cameraCorner']) settings[k] = $(k).value;
  for (const k of ['clipAfterSeconds', 'cameraSize', 'lowSpaceWarnGb']) settings[k] = Number($(k).value);
  settings.remotePort = Math.round(Number($('remotePort').value)) || settings.remotePort;
  settings.cameraDevice = $('cameraDevice').value || settings.cameraDevice;
  // Only once a card is chosen, so turning the switch on keeps the suggested card and its audio.
  if (settings.captureCard || settings.captureCardDevice) {
    settings.captureCardDevice = $('captureCardDevice').value || settings.captureCardDevice;
    settings.captureCardAudio = $('captureCardAudio').value;
  }
  settings.cardResolution = $('cardResolution').value;
  settings.cardFps = Number($('cardFps').value);
  settings.cardBitrateKbps = Number($('cardBitrateKbps').value);
  settings.cardHdr = $('cardHdr').checked;
  settings.cardAudioOwnTrack = $('cardAudioOwnTrack').checked;
  settings.cardPauseWithoutSignal = $('cardPauseWithoutSignal').checked;
  settings.multipass = $('multipass').value === 'true';
  settings.outputDir = $('outputDir').value;
  settings.weeklyReelTag = $('weeklyReelTag').value.trim().replace(/^#/, '');
}

/** The weekly reel's choices only show while it's on. With the buffer on disk, moving it there has nothing to do. */
function renderWeeklyReel() {
  $('weekly-reel-details').hidden = !$('weeklyReel').checked;
  $('freeMemoryInGames').disabled = $('diskBuffer').checked;
}

const icon = (path) => `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${path}</svg>`;
const SECTIONS = [
  ['general', 'General', 'Startup, notifications, updates', '<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.2 2.2M16.2 16.2l2.2 2.2M5.6 18.4l2.2-2.2M16.2 7.8l2.2-2.2"/>'],
  ['video', 'Video', 'Quality, buffer, webcam, performance', '<rect x="3" y="6" width="13" height="12" rx="2.5"/><path d="M16 10l5-3v10l-5-3"/>'],
  ['audio', 'Audio', 'Game audio, devices, microphone', '<path d="M4 10v4h4l5 4V6L8 10zM16.5 9a4 4 0 0 1 0 6M19 6.5a8 8 0 0 1 0 11"/>'],
  ['controls', 'Controls', 'Hotkeys, clip lengths, controller', '<rect x="3" y="6" width="18" height="12" rx="2.5"/><path d="M7 10h.01M11 10h.01M15 10h.01M7 14h10"/>'],
  ['games', 'Games', 'Per-game settings and auto-record', '<rect x="2.5" y="7" width="19" height="11" rx="4.5"/><path d="M7 11v3M5.5 12.5h3M15.5 11.5h.01M18 13.5h.01"/>'],
  ['clips', 'Clips and sharing', 'Save location, share copies, Discord', '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>'],
];
let currentPane = null;

function renderSectionList() {
  $('settings-list').replaceChildren(...SECTIONS.map(([id, title, sub, path]) => {
    const b = document.createElement('button');
    b.className = 'nav-item';
    b.dataset.pane = id;
    b.innerHTML = `<span class="nav-icon">${icon(path)}</span><span class="nav-text"><div class="nav-title">${title}</div><div class="nav-sub">${sub}</div></span><span class="nav-arrow" aria-hidden="true">›</span>`;
    b.onclick = () => showPane(id);
    return b;
  }));
}

function showPane(id) {
  currentPane = id;
  $('settings-list').hidden = !!id;
  document.querySelectorAll('.pane').forEach((p) => { p.hidden = p.dataset.pane !== id; });
  $('crumb-sep').hidden = !id;
  $('crumb-now').hidden = !id;
  $('crumb-now').textContent = SECTIONS.find(([s]) => s === id)?.[1] ?? '';
  if (id === 'audio') loadAudioChoices();
  if (id === 'games') renderGamePicker();
  if (id === 'controls') fillController();
  if (id === 'video') { loadCameraChoices(); loadCardChoices(); }
  updateLiveViews();
}

function showView(name) {
  const inSettings = name === 'settings';
  if (libraryOpen) setLibrary(false);
  $('home').hidden = inSettings;
  $('settings').hidden = !inSettings;
  $('brand').hidden = inSettings;
  $('summary').hidden = inSettings;
  $('open-settings').hidden = inSettings;
  $('crumbs').hidden = !inSettings;
  if (inSettings) {
    showPane(null);
    loadAudioChoices();
  }
}

/* Wiring */

api.on('status', (s) => { status = s; renderStatus(); });
api.on('session', () => refresh());
api.on('capture', (c) => { capture = c; renderStatus(); });
api.on('update', (u) => { update = u; renderUpdate(); });
api.on('profile', (p) => { activeProfile = p.game ?? null; activeBuffer = p.bufferSeconds; renderSummary(); renderBuffer(); });
api.on('panel-shown', () => { refresh(); updateLiveViews(); });
window.addEventListener('focus', updateLiveViews);
document.addEventListener('visibilitychange', updateLiveViews);
api.on('feedback', (f) => {
  // A Stream Deck dial changed the mic volume, so the form takes it, or a later save would put it back.
  if (f.kind === 'mic-volume') {
    refresh().then((state) => {
      settings?.audioSources.forEach((s, i) => {
        const saved = state.settings.audioSources[i];
        if (saved?.device === s.device) s.volume = saved.volume;
      });
      if (currentPane === 'audio' && !$('settings').hidden) renderAudio();
    });
    return;
  }
  if (f.kind === 'clip-waiting') {
    showBanner(`Saving in ${f.seconds} seconds, so what happens next is in the clip too.`);
  } else if (f.kind === 'clip' && f.ok) {
    shineBuffer();
    if (f.short) showBanner(`Only ${Math.round(f.actualSeconds)}s was buffered, so the clip is shorter than the ${f.requestedSeconds}s you asked for.`);
    else showBanner(`Saved ${Math.round(f.actualSeconds)}s clip`, 'good');
  } else if (f.kind === 'recording-saved') {
    showBanner('Recording saved', 'good');
  } else if (f.kind === 'compressed' && f.ok) {
    showBanner(f.overTarget ? `Your ${f.megabytes} MB copy is ready. This clip is too long to go smaller and still look good, so trim it to fit 100 MB.` : `Your ${f.megabytes} MB copy is ready to drag into any app.`, 'good');
  } else if (f.kind === 'vertical' && f.ok) {
    showBanner('Your vertical copy is ready to drag into any app.', 'good');
  } else if (f.kind === 'screenshot' && f.ok) {
    showBanner('Screenshot saved', 'good');
  } else if (f.kind === 'discord' && f.ok) {
    showBanner(webhooks().length > 1 && f.name ? `Sent to ${f.name}` : 'Sent to Discord', 'good');
  } else if (f.kind === 'trimmed' && f.ok) {
    showBanner('Your trimmed copy is ready to drag into any app.', 'good');
  } else if (f.kind === 'clip-extended' && f.ok) {
    showBanner('Pressed twice, so you get one longer clip. The shorter one is in the Recycle Bin.', 'good');
  } else if (f.kind === 'starred-clip' && f.ok) {
    shineBuffer();
    showBanner('Saved and starred', 'good');
  } else if (f.kind === 'storage') {
    showBanner(`Moved ${f.removed} old ${f.removed === 1 ? 'file' : 'files'} to the Recycle Bin, as your storage settings ask.`, 'good');
  } else if (f.kind === 'exported') {
    showBanner('Edited clip saved', 'good');
  } else if (f.kind === 'clean-mic' && f.ok) {
    showBanner(f.cleanMic ? 'Clean Mic is on' : 'Clean Mic is off', 'good');
  } else if (f.kind === 'marker' && f.ok) {
    showBanner(`Marker ${f.number} added`, 'good');
  } else if (f.kind === 'recovering') {
    showBanner(`Capture stopped after a problem. Starting it again in ${f.seconds} seconds.`, 'bad');
  } else if (f.kind === 'recovered') {
    showBanner('Capture is running again. The buffer started over.', 'good');
  } else if (f.kind === 'buffer-on-disk') {
    showBanner(f.message, 'good');
  } else if (f.kind === 'reel' && f.ok) {
    const total = Math.round(f.seconds);
    showBanner(`Your weekly reel is ready: ${f.clips} clips, ${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}. It's tagged #reel.`, 'good');
  } else if (f.kind === 'card' && f.ok) {
    showBanner(f.cardOn ? 'Recording the capture card' : 'Recording this PC', 'good');
  } else if (f.kind === 'camera' && f.ok) {
    showBanner(f.cameraOn ? 'Webcam is on' : 'Webcam is off', 'good');
  } else if (f.kind === 'favourite' && f.ok) {
    showBanner(f.favourite ? 'Starred your newest clip' : 'Took the star off your newest clip', 'good');
  } else if (f.kind === 'mic' && f.ok) {

    showBanner(f.micMuted ? 'Mic muted in clips' : 'Mic is back on in clips', 'good');
  } else if (!f.ok) {
    showBanner(f.message, 'bad');
  }
  refresh();
});

document.addEventListener('click', closeMenus);
document.querySelectorAll('.tab').forEach((tab) => {
  tab.onclick = () => {
    kind = tab.dataset.kind;
    document.querySelectorAll('.tab').forEach((t) => t.classList.toggle('active', t === tab));
    renderMedia();
  };
});
document.querySelectorAll('[data-add]').forEach((b) => { b.onclick = () => addAudio(b.dataset.add); });
$('open-settings').onclick = () => showView('settings');
$('library-search').oninput = renderMedia;
$('library-game').onchange = () => renderMedia();
$('library-tag').onchange = () => renderMedia();
$('library-sort').onchange = () => renderMedia();
$('library-favourites').onclick = () => {
  favouritesOnly = !favouritesOnly;
  $('library-favourites').setAttribute('aria-pressed', String(favouritesOnly));
  renderMedia();
};
$('update-now').onclick = () => api.invoke('install-update').catch((e) => showBanner(errorText(e), 'bad'));
$('update-skip').onclick = () => api.invoke('skip-update').then(() => {
  settings.skippedUpdate = update.version ?? settings.skippedUpdate;
  return refresh();
}).catch((e) => showBanner(errorText(e), 'bad'));
$('check-update').onclick = () => api.invoke('check-update');
$('whats-new-close').onclick = () => api.invoke('dismiss-patch-notes').then(() => {
  seenVersion = appVersion.replace(/-dev$/, '');
  renderPatchNotes();
}).catch((e) => showBanner(errorText(e), 'bad'));
$('deck-install').onclick = installStreamDeckPlugin;
$('deck-plugin-install').onclick = installStreamDeckPlugin;
$('deck-dismiss').onclick = () => api.invoke('dismiss-stream-deck-plugin').then(() => {
  streamDeck = { ...streamDeck, offer: null };
  renderStreamDeck();
}).catch((e) => showBanner(errorText(e), 'bad'));
$('autoUpdate').onchange = () => scheduleSave();
$('usagePing').onchange = () => answerUsage($('usagePing').checked);
$('usage-yes').onclick = () => answerUsage(true);
$('usage-no').onclick = () => answerUsage(false);
$('storageLimitGb').onchange = () => scheduleSave();
$('deleteAfterDays').onchange = () => scheduleSave();
$('add-webhook').onclick = () => {
  settings.discordWebhooks.push({ name: settings.discordWebhooks.length ? '' : 'Discord', url: '' });
  renderWebhooks();
  $('webhook-list').lastElementChild?.querySelector('input[type=password]')?.focus();
};
$('starClipSeconds').onchange = () => scheduleSave();
$('profile-browse').onclick = async () => { const exe = await api.invoke('pick-game'); if (exe) addProfile(exe); };
$('view-all').onclick = () => setLibrary(true);
$('library-back').onclick = () => setLibrary(false);
$('session-review').onclick = reviewSession;
$('session-dismiss').onclick = async () => {
  lastSession = null;
  renderSession();
  await api.invoke('dismiss-session').catch(() => {});
};
$('session-filter').onclick = () => { sessionFilter = null; renderMedia(); };

// The Delete key deletes what's selected, with the same second press for more than one as the Delete button.
document.addEventListener('keydown', (e) => {
  if (e.key !== 'Delete' || !selected.size || $('home').hidden) return;
  if (e.target instanceof Element && e.target.closest('input, textarea, select, [contenteditable]')) return;
  e.preventDefault();
  deleteSelected();
});
$('close-panel').onclick = () => { if (saveTimer) saveNow(); api.invoke('close-panel'); };
$('crumb-root').onclick = () => showPane(null);
$('go-back').onclick = () => {
  if (saveTimer) saveNow();
  if (currentPane) showPane(null); else showView('home');
};
// The panel closes when it loses focus, so anything still waiting to save goes now.
window.addEventListener('blur', () => { if (saveTimer) saveNow(); updateLiveViews(); });
$('mic').onclick = () => api.invoke('toggle-mic').catch(() => undefined);
$('clean-mic').onclick = () => api.invoke('toggle-clean-mic').catch(() => undefined);
$('cleanMicSwitch').onchange = () => api.invoke('toggle-clean-mic').catch(() => renderStatus());
$('marker').onclick = () => api.invoke('add-marker').catch(() => undefined);
$('screenshot').onclick = () => api.invoke('screenshot').catch(() => undefined);
for (const id of ['recordSplit', 'gameAudio','gameAudioOwnTrack', 'encoder', 'toastEnabled', 'beepEnabled', 'startMinimized', 'closeToTray', 'launchOnStartup',
  'clipAfterSeconds', 'extendOnDoublePress', 'bufferOnlyInGames', 'verticalFaceCam', 'pushToTalk', 'gameFolders', 'lowSpaceWarnGb', 'cameraCorner', 'cameraSize', 'verticalMode', 'evenAudio']) $(id).onchange = () => scheduleSave();
$('cameraEnabled').onchange = () => { renderCamera(); scheduleSave(); };
$('verticalMode').addEventListener('change', () => { renderCamera(); renderSummary(); });
$('remoteStreamDeck').onchange = () => { remote.on = $('remoteStreamDeck').checked; renderRemote(); scheduleSave(); };
$('remotePort').oninput = () => scheduleSave(TYPING_DELAY_MS);
$('copy-remote').onclick = () => navigator.clipboard.writeText($('remote-address').value)
  .then(() => { $('save-note').textContent = 'Address copied. Paste it into the Stream Deck app on the other PC.'; }, (e) => { $('save-note').textContent = errorText(e); });
for (const [id, field, what] of [['copy-shortcut-url', 'shortcut-url', 'Address'], ['copy-shortcut-header', 'shortcut-header', 'Header value']]) {
  $(id).onclick = () => navigator.clipboard.writeText($(field).value)
    .then(() => { $('save-note').textContent = `${what} copied. Paste it into your shortcut.`; }, (e) => { $('save-note').textContent = errorText(e); });
}
$('new-remote-key').onclick = async () => {
  remote = await api.invoke('new-api-key');
  renderRemote();
  $('save-note').textContent = 'New key made. Paste the new address on the other PC.';
};

$('cameraDevice').onchange = () => { settings.cameraDevice = $('cameraDevice').value; scheduleSave(); };
$('captureCard').onchange = () => { renderCard(); renderSummary(); renderEstimate(); scheduleSave(); };
$('captureCardDevice').onchange = () => {
  settings.captureCardDevice = $('captureCardDevice').value;
  settings.captureCardAudio = matchingCardAudio(settings.captureCardDevice);
  renderCard();
  scheduleSave();
};
$('captureCardAudio').onchange = () => { settings.captureCardAudio = $('captureCardAudio').value; scheduleSave(); };
for (const id of ['cardResolution', 'cardFps', 'cardBitrateKbps']) $(id).onchange = () => { renderCardModes(); renderSummary(); renderEstimate(); scheduleSave(); };
$('cardAudioOwnTrack').onchange = () => scheduleSave();
$('cardHdr').onchange = () => scheduleSave();
$('cardPauseWithoutSignal').onchange = () => scheduleSave();
for (const id of ['resolution', 'frameRate', 'bitrateKbps', 'encoderPreset', 'multipass', 'codec']) $(id).onchange = videoFieldChanged;
$('runAsAdmin').onchange = async () => {
  const on = $('runAsAdmin').checked;
  const note = $('admin-note');
  await saveNow();
  if (!on) {
    note.textContent = 'Afterclip will start normally next time. It stays as administrator until then.';
    return;
  }
  if (capture.elevated) {
    note.textContent = 'Afterclip is running as administrator, and will from now on.';
    return;
  }
  try {
    note.textContent = 'Restarting as administrator. This clears the buffer.';
    await api.invoke('restart-admin');
  } catch (e) {
    $('runAsAdmin').checked = false;
    await saveNow();
    note.textContent = errorText(e);
  }
};
$('save-clip').onclick = () => api.invoke('clip', chosenLength).catch(() => undefined);
$('record').onclick = () => api.invoke('toggle-record').catch(() => undefined);
$('open-editor').onclick = () => api.invoke('open-editor', []);
// More than this is too long to trim comfortably in one go.
const MAX_REEL_CLIPS = 30;
$('make-reel').onclick = () => {
  const shown = filtered(media.filter((m) => m.kind === kind)).sort(SORTS.oldest);
  if (!shown.length) { showBanner('Nothing is shown to make a reel from. Try another filter.', 'bad'); return; }
  const files = shown.slice(-MAX_REEL_CLIPS).map((m) => m.file);
  const left = shown.length - files.length;
  api.invoke('open-editor', files)
    .then(() => showBanner(left > 0 ? `Opened the newest ${files.length} in the editor. Trim them, then Merge and save.` : `Opened ${files.length} in the editor. Trim them, then Merge and save.`, 'good'))
    .catch((e) => showBanner(errorText(e), 'bad'));
};
/** Pairs of clips that show the same moment, each with a way to drop the shorter one or keep both. */
async function findOverlaps() {
  const box = $('overlaps');
  const button = $('find-overlaps');
  button.disabled = true;
  button.textContent = 'Looking';
  let pairs = [];
  try {
    pairs = await api.invoke('find-overlaps');
  } catch (e) {
    showBanner(errorText(e), 'bad');
    return;
  } finally {
    button.disabled = false;
    button.textContent = 'Find overlapping clips';
  }
  // A saved clip's name ends with its length, such as _30s, or _full for the whole buffer.
  const length = (name) => {
    const m = /_(d+)s(?:_d+)?.w+$/.exec(name);
    return m ? lengthLabel(Number(m[1])) : 'whole buffer';
  };
  const when = (file) => { const m = media.find((x) => x.file === file); return m ? timeAgo(m.mtime).toLowerCase() : ''; };
  const close = document.createElement('button');
  close.className = 'small';
  close.textContent = 'Done';
  close.onclick = () => { box.hidden = true; };
  const head = document.createElement('div');
  head.className = 'overlaps-head';
  const summary = document.createElement('span');
  summary.textContent = pairs.length
    ? `${pairs.length} ${pairs.length === 1 ? 'pair shows' : 'pairs show'} the same moment. The longer clip of each has all of it.`
    : 'No clips overlap. Each one shows a different moment.';
  head.append(summary, close);
  const rows = pairs.map((p) => {
    const row = document.createElement('div');
    row.className = 'overlap';
    const text = document.createElement('div');
    text.className = 'overlap-text';
    text.textContent = `${parseName(p.keep.name).game}, ${when(p.keep.file)}: the ${length(p.covered.name)} clip is inside the ${length(p.keep.name)} one`;
    text.title = `${p.covered.name}
${p.keep.name}`;
    const drop = document.createElement('button');
    drop.className = 'small';
    drop.textContent = 'Remove shorter';
    drop.title = `Moves ${p.covered.name} to the Recycle Bin`;
    drop.onclick = async () => {
      try {
        await api.invoke('trash', p.covered.file);
        row.remove();
        await refresh();
      } catch (e) { showBanner(errorText(e), 'bad'); }
    };
    const keep = document.createElement('button');
    keep.className = 'small';
    keep.textContent = 'Keep both';
    keep.onclick = async () => {
      await api.invoke('keep-overlap', { keep: p.keep.name, covered: p.covered.name }).catch((e) => showBanner(errorText(e), 'bad'));
      row.remove();
    };
    row.append(text, drop, keep);
    return row;
  });
  box.replaceChildren(head, ...rows);
  box.hidden = false;
}
$('find-overlaps').onclick = findOverlaps;
$('merge-selected').onclick = () => {
  const ordered = media.filter((m) => selected.has(m.file) && m.kind !== 'screenshot').reverse().map((m) => m.file);
  api.invoke('open-editor', ordered);
  selected.clear();
  renderMedia();
};
$('delete-selected').onclick = deleteSelected;
$('clear-selected').onclick = () => { selected.clear(); renderMedia(); };
$('open-studio').onclick = () => api.invoke('open-studio').catch((error) => showBanner(errorText(error), 'bad'));
$('batch-selected').onclick = () => api.invoke('open-studio', [...selected]).catch((error) => showBanner(errorText(error), 'bad'));
api.on('studio-import', () => refresh());
$('restart-admin').onclick = () => api.invoke('restart-admin').catch((e) => showBanner(errorText(e), 'bad'));
$('audio-missing-fix').onclick = () => { showView('settings'); showPane('audio'); };
$('card-preview').onclick = () => $('card-preview').classList.toggle('big');
$('card-hdr-dismiss').onclick = () => { cardHdrDismissed = true; renderStatus(); };
$('card-hdr-on').onclick = () => {
  settings.cardHdr = true;
  $('cardHdr').checked = true;
  scheduleSave();
  renderStatus();
};
$('add-marker-key').onclick = () => { settings.markerKeys.push({ label: '', hotkey: '' }); renderMarkerKeys(); scheduleSave(); };
for (const id of ['toastCorner', 'markerClipSeconds', 'autoClipSource', 'autoClipSeconds']) $(id).onchange = () => scheduleSave();
$('autoClip').onchange = () => { renderAutoClip(); scheduleSave(); updateLiveViews(); };
$('autoClipLevelDb').oninput = () => { renderAutoClip(); scheduleSave(TYPING_DELAY_MS); };
// A restart clears the buffer, so these wait until the switch has settled.
for (const id of ['diskBuffer', 'hdrCapture']) $(id).onchange = () => { renderWeeklyReel(); renderEstimate(); scheduleSave(TYPING_DELAY_MS); };
for (const id of ['freeMemoryInGames', 'weeklyReel']) $(id).onchange = () => { renderWeeklyReel(); scheduleSave(); };
$('weeklyReelTag').oninput = () => scheduleSave(TYPING_DELAY_MS);
$('make-weekly-reel').onclick = async () => {
  const button = $('make-weekly-reel');
  button.disabled = true;
  try {
    await saveNow();
    const reel = await api.invoke('make-weekly-reel');
    if (!reel) showBanner('Not enough clips for a reel. It needs at least two from the last 7 days, with the same video settings.');
    else refresh();
  } catch (e) {
    showBanner(errorText(e), 'bad');
  } finally {
    button.disabled = false;
  }
};
$('add-preset').onclick = () => { settings.presets.push({ seconds: 45, hotkey: '' }); renderPresets(); scheduleSave(); };
$('pick-folder').onclick = async () => { const dir = await api.invoke('pick-folder'); if (dir) { $('outputDir').value = dir; scheduleSave(); } };
$('quality').onchange = () => { applyQuality(); scheduleSave(); };
$('bufferMinutes').onchange = () => { renderEstimate(); scheduleSave(); };
$('quit').onclick = () => api.invoke('quit-app');
$('export-settings').onclick = async () => {
  try {
    if (await api.invoke('export-settings')) $('save-note').textContent = 'Settings exported. Import that file on your other PC, under General.';
  } catch (e) {
    $('save-note').textContent = errorText(e);
  }
};
$('import-settings').onclick = async () => {
  if (saveTimer) await saveNow();
  try {
    // Everything on the page comes from the settings, so the simplest redraw is a fresh page.
    if (await api.invoke('import-settings')) window.location.reload();
  } catch (e) {
    $('save-note').classList.add('warn');
    $('save-note').textContent = errorText(e);
  }
};
$('save-diagnostics').onclick = async () => {

  try {
    if (await api.invoke('save-diagnostics')) $('save-note').textContent = 'Diagnostics saved. Send that zip file with your problem report.';
  } catch (e) {
    $('save-note').textContent = errorText(e);
  }
};

refresh().then((state) => {
  settings = state.settings;
  renderStatus();
  fillForm();
  updateLiveViews();
  renderSectionList();
  api.invoke('display-info').then((d) => {
    display = d;
    fillVideoOptions();
  });
  renderPresets(state.hotkeyFailures);
  renderLengths();
  renderSummary();
  renderEstimate();
  if (state.hotkeyFailures.length) showBanner(`These hotkeys are held by another app: ${state.hotkeyFailures.join(', ')}`, 'bad');
}).finally(() => api.ready());
