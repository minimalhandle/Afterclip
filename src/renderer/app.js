const api = window.afterclip;
const $ = (id) => document.getElementById(id);
let settings;
let status = { state: 'stopped', buffering: false, recording: false };
let capture = { mode: 'desktop' };
let media = [];
let kind = 'clip';
let chosenLength = null;
let audioChoices = { outputs: [], inputs: [], apps: [] };
const selected = new Set();
const info = new Map(); // file|mtime -> { url, duration }

const errorText = (e) => String(e.message ?? e);
const lengthLabel = (s) => (s > 0 ? (s >= 60 && s % 60 === 0 ? `${s / 60}m` : `${s}s`) : 'All');
const clockLabel = (seconds) => {
  const s = Math.round(seconds);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};
const bufferLabel = (seconds) => (seconds >= 60 ? `${+(seconds / 60).toFixed(1)}m` : `${seconds}s`);

function showBanner(text, tone) {
  const b = $('banner');
  b.textContent = text;
  b.className = `banner ${tone ?? ''}`;
  b.hidden = false;
  clearTimeout(showBanner.timer);
  showBanner.timer = setTimeout(() => { b.hidden = true; }, 6000);
}

function renderStatus() {
  const card = $('capture-card');
  const ready = status.state === 'ready' && status.buffering;
  card.className = `capture-card ${status.recording ? 'recording' : ready ? 'live' : status.state === 'error' ? 'bad' : ''}`;
  const labels = {
    desktop: 'Recording primary monitor',
    detecting: `Connecting to ${capture.app ?? 'game'}`,
    game: capture.app ?? 'Game',
    fallback: capture.app ?? 'Fullscreen app',
  };
  let title = labels[capture.mode] ?? 'Recording';
  let detail = capture.mode === 'game' ? 'Game capture' : capture.mode === 'fallback' ? 'Recording its monitor' : capture.mode === 'detecting' ? 'Recording its monitor for now' : 'Waiting for a game';
  if (status.state === 'error') { title = 'Capture stopped'; detail = status.message ?? ''; }
  else if (!ready) { title = 'Starting capture'; detail = ''; }
  else if (status.recording) { detail = `Full recording running · ${detail}`; }
  $('capture-title').textContent = title;
  $('capture-detail').textContent = detail;
  $('capture-detail').title = detail;

  const rec = $('record');
  rec.textContent = status.recording ? 'Stop' : 'Record';
  rec.classList.toggle('on', status.recording);
  rec.disabled = status.state !== 'ready';
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
  renderHalo();
}

// The ring: its full length is the buffer, the colour is what's filled so far, and the white part, starting
// at the arrowhead, is what Save would keep. It only ticks while the buffer is still filling.
let haloTimer = null;
function renderHalo() {
  if (!settings) return;
  const buffer = Math.max(1, settings.bufferSeconds);
  const filled = status.buffering && status.bufferingSince ? Math.min(buffer, Math.max(0, (Date.now() - status.bufferingSince) / 1000)) : 0;
  const wanted = chosenLength > 0 ? chosenLength : buffer;
  const kept = Math.min(wanted, filled);
  const percent = (seconds) => (100 * seconds) / buffer;
  $('halo').classList.toggle('recording', status.recording);
  $('halo-filled').setAttribute('stroke-dasharray', `${percent(filled)} 100`);
  $('halo-selected').setAttribute('stroke-dasharray', `${percent(kept)} 100`);
  $('halo-filled').classList.toggle('empty', filled <= 0);
  $('halo-selected').classList.toggle('empty', kept <= 0);
  $('halo-time').textContent = clockLabel(filled);
  $('halo-note').textContent = !status.buffering ? 'Not buffering'
    : wanted >= buffer ? 'Whole buffer selected'
      : kept < wanted ? `Filling to ${lengthLabel(wanted)}`
        : `Last ${lengthLabel(wanted)} selected`;
  const filling = status.buffering && filled < buffer;
  if (filling && !haloTimer) haloTimer = setInterval(renderHalo, 1000);
  if (!filling && haloTimer) { clearInterval(haloTimer); haloTimer = null; }
}

function pulseHalo() {
  const halo = $('halo');
  halo.classList.remove('saved');
  // Reading layout restarts the animation when clips are saved close together.
  void halo.offsetWidth;
  halo.classList.add('saved');
}

function renderSummary() {
  if (!settings) return;
  const quality = settings.quality === 'full' ? 'Full' : settings.quality.charAt(0).toUpperCase() + settings.quality.slice(1);
  $('summary').textContent = `${quality} · ${bufferLabel(settings.bufferSeconds)} buffer · ${settings.fps} FPS`;
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
  renderHalo();
}

const parseName = (name) => {
  const stem = name.replace(/\.[^.]+$/, '');
  const m = /^(.*?)_(\d{4}-\d{2}-\d{2})_(\d{2})-(\d{2})/.exec(stem);
  if (!m) return { game: stem };
  return { game: /^(Clip|Recording)$/.test(m[1]) ? 'Desktop' : m[1].replace(/^(Merged|Edit)$/, '$1 clip') };
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

function applyInfo(thumb, result) {
  thumb.querySelector('img').src = result.url;
  const badge = thumb.querySelector('.length');
  badge.textContent = clockLabel(result.duration);
  badge.hidden = false;
}

function closeMenus() {
  document.querySelectorAll('.menu').forEach((m) => m.remove());
}

function renderSelection() {
  $('selection').hidden = selected.size === 0;
  $('selection-count').textContent = `${selected.size} selected`;
}

// Dragging a clip hands it to Windows, which then carries it into any app, as File Explorer does.
const DRAG_START_PIXELS = 6;
let justDragged = false;
function wireDrag(li, m) {
  li.addEventListener('dragstart', (e) => e.preventDefault());
  li.addEventListener('mousedown', (down) => {
    if (down.button !== 0 || down.target.closest('.more, input, .menu')) return;
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

const HOME_CLIPS = 3;
let libraryOpen = false;
function setLibrary(open) {
  libraryOpen = open;
  $('home').classList.toggle('library-open', open);
  $('library-head').hidden = !open;
  $('brand').hidden = open;
  $('summary').hidden = open;
  $('view-all').hidden = open;
  $('open-editor').hidden = !open;
  renderMedia();
}

function renderMedia() {
  const all = media.filter((m) => m.kind === kind);
  const shown = libraryOpen ? all : all.slice(0, HOME_CLIPS);
  $('view-all').textContent = `View all (${all.length})`;
  $('library-title').textContent = kind === 'clip' ? 'All clips' : 'All recordings';
  const empty = $('empty');
  empty.hidden = shown.length > 0;
  empty.textContent = kind === 'clip' ? 'No clips yet. Save your first moment with the button above or a hotkey.' : 'No recordings yet. Press Record to capture a full session.';
  observer.disconnect();
  $('media').replaceChildren(...shown.map((m) => {
    const li = document.createElement('li');
    li.className = 'item';
    li.classList.toggle('selected', selected.has(m.file));
    const thumb = document.createElement('button');
    thumb.className = 'thumb';
    thumb.setAttribute('aria-label', `Play ${m.name}`);
    thumb.title = 'Click to play, or drag into another app';
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

    const text = document.createElement('div');
    text.className = 'item-text';
    const title = document.createElement('div');
    title.className = 'item-title';
    title.textContent = parseName(m.name).game;
    const sub = document.createElement('div');
    sub.className = 'item-sub';
    sub.textContent = `${timeAgo(m.mtime)} · ${sizeLabel(m.size)}`;
    sub.title = m.name;
    text.append(title, sub);

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
    const pick = document.createElement('input');
    pick.type = 'checkbox';
    pick.title = 'Select to edit together';
    pick.setAttribute('aria-label', `Select ${m.name}`);
    pick.checked = selected.has(m.file);
    pick.onchange = () => {
      if (pick.checked) selected.add(m.file); else selected.delete(m.file);
      li.classList.toggle('selected', pick.checked);
      renderSelection();
    };
    actions.append(more, pick);
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
    item('Play', () => api.invoke('open', m.file)),
    item('Edit', () => api.invoke('open-editor', [m.file])),
    item('Copy file', () => api.invoke('copy-file', m.file).then(() => showBanner('Copied. Paste it into any app.', 'good'), (e) => showBanner(errorText(e), 'bad'))),
    item('Show in folder', () => api.invoke('reveal', m.file)),
    item('Delete', async () => {
      await api.invoke('trash', m.file).catch((e) => showBanner(errorText(e), 'bad'));
      selected.delete(m.file);
      refresh();
    }, 'danger'),
  );
  return menu;
}

async function refresh() {
  const state = await api.invoke('state');
  status = state.status;
  capture = state.capture;
  media = state.media;
  // Only this comes from the backend after startup: the form owns the rest while it's being edited.
  if (settings) {
    settings.micMuted = state.settings.micMuted;
    settings.cleanMic = state.settings.cleanMic;
  }
  renderEncoderNote(state.encoderInUse);
  const files = new Set(media.map((m) => m.file));
  for (const f of selected) if (!files.has(f)) selected.delete(f);
  renderStatus();
  renderMedia();
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
    for (const c of choices) select.add(new Option(c.name, c.id));
    // Keep a saved source listed even if it isn't available right now, such as Discord being closed.
    if (source.device && !choices.some((c) => c.id === source.device)) {
      select.add(new Option(`${source.label} (not running)`, source.device));
    }
    select.value = source.device;
    select.onchange = () => {
      source.device = select.value;
      source.label = select.selectedOptions[0]?.text ?? source.label;
      checkAudio();
      scheduleSave();
    };
    const del = document.createElement('button');
    del.className = 'more';
    del.textContent = '×';
    del.setAttribute('aria-label', `Remove ${source.label}`);
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
let lastStats = null;

/** Meters and stats run only while on screen, so they cost nothing during play. */
function updateLiveViews() {
  const visible = !document.hidden && document.hasFocus();
  const metersWanted = visible && currentPane === 'audio' && !$('settings').hidden;
  if (metersWanted && !meterTimer) {
    api.invoke('set-meters', true).catch(() => undefined);
    meterTimer = setInterval(pollLevels, METER_MS);
  } else if (!metersWanted && meterTimer) {
    clearInterval(meterTimer);
    meterTimer = null;
    api.invoke('set-meters', false).catch(() => undefined);
    document.querySelectorAll('[data-meter]').forEach((f) => { f.style.transform = 'scaleX(0)'; });
  }
  const paneOpen = !$('settings').hidden && currentPane === 'performance';
  const homeOpen = $('settings').hidden;
  const wanted = visible && (paneOpen || homeOpen) ? (paneOpen ? PANE_STATS_MS : HOME_STATS_MS) : 0;
  if (statsTimer?.ms !== wanted) {
    if (statsTimer) clearInterval(statsTimer.id);
    statsTimer = wanted ? { ms: wanted, id: setInterval(pollStats, wanted) } : null;
    lastStats = null;
    if (wanted) pollStats();
  }
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
  const previous = lastStats ?? { encoderTotal: 0, encoderSkipped: 0, renderTotal: 0, renderLagged: 0 };
  const share = (bad, total) => (total > 0 ? (bad / total) * 100 : 0);
  const encoder = share(s.encoderSkipped - previous.encoderSkipped, s.encoderTotal - previous.encoderTotal);
  const render = share(s.renderLagged - previous.renderLagged, s.renderTotal - previous.renderTotal);
  lastStats = s;
  const problem = encoder > OVERLOAD_PERCENT
    ? 'Encoder overloaded: lower the quality or use a faster preset.'
    : render > OVERLOAD_PERCENT ? 'GPU is too busy, so some frames are being missed.' : '';
  const fps = Math.round(s.fps);
  const lost = s.encoderSkipped + s.renderLagged;
  const strip = $('perf-strip');
  strip.textContent = problem || `${fps} fps · ${lost} frames lost · CPU ${s.cpu.toFixed(1)}% · ${sizeFromMb(s.memoryMb)}`;
  strip.classList.toggle('warn', !!problem);
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
  const mb = await api.invoke('estimate', { bufferSeconds: Number($('bufferMinutes').value) * 60, bitrateKbps: Number($('bitrateKbps').value) });
  const note = $('estimate');
  note.textContent = `Uses about ${mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb} MB`} of memory at this quality and length.`;
  note.classList.toggle('warn', mb >= LARGE_BUFFER_MB);
}

const LARGE_BUFFER_MB = 4096;
const BITRATES = [[8000, 'Low (8 Mbps)'], [20000, 'Medium (20 Mbps)'], [40000, 'High (40 Mbps)'], [60000, 'Very high (60 Mbps)'], [80000, 'Ultra (80 Mbps)'], [100000, 'Extreme (100 Mbps)'], [150000, 'Maximum (150 Mbps)']];
const HEIGHTS = [2160, 1440, 1080, 720];
const QUALITY = {
  low: { resolution: '720', frameRate: '60', kbps: 8000, preset: 'p1', multipass: false, blurb: 'Smallest files and the least work for your PC.' },
  medium: { resolution: '1080', frameRate: '60', kbps: 20000, preset: 'p3', multipass: false, blurb: 'A good balance for sharing.' },
  high: { resolution: 'native', frameRate: '60', kbps: 40000, preset: 'p3', multipass: false, blurb: 'Your monitor\'s full resolution, with low encode load.' },
  full: { resolution: 'native', frameRate: 'native', kbps: 150000, preset: 'p5', multipass: true, blurb: 'The most your monitor and encoder can give. Files are very large and it uses the most memory, so shorten the buffer if needed.' },
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

function fillForm() {
  for (const k of ['encoder', 'outputDir', 'quality', 'codec', 'encoderPreset']) $(k).value = settings[k];
  $('multipass').value = String(settings.multipass);
  fillVideoOptions();
  describeQuality();
  const minutes = +(settings.bufferSeconds / 60).toFixed(2);
  ensureOption($('bufferMinutes'), minutes, `Custom (${settings.bufferSeconds} seconds)`);
  $('bufferMinutes').value = minutes;
  for (const k of ['gameAudio', 'gameAudioOwnTrack', 'runAsAdmin', 'toastEnabled', 'beepEnabled', 'launchOnStartup', 'startMinimized', 'closeToTray']) $(k).checked = settings[k];
  bindHotkey($('recordHotkey'), () => settings.recordHotkey, (v) => { settings.recordHotkey = v; scheduleSave(); });
  bindHotkey($('micHotkey'), () => settings.micHotkey, (v) => { settings.micHotkey = v; scheduleSave(); });
  bindHotkey($('markerHotkey'), () => settings.markerHotkey, (v) => { settings.markerHotkey = v; scheduleSave(); });
  ensureOption($('recordSplit'), settings.recordSplitSeconds, `Every ${settings.recordSplitSeconds} seconds`);
  $('recordSplit').value = settings.recordSplitSeconds;
  $('game-level').replaceChildren(levelControl(-1, settings.gameAudioVolume, (v) => { settings.gameAudioVolume = v; }));
}

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
    if (failures !== lastFailures) { lastFailures = failures; renderPresets(r.hotkeyFailures); }
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
  settings.bufferSeconds = Math.round(Number($('bufferMinutes').value) * 60);
  for (const k of ['gameAudio', 'gameAudioOwnTrack', 'runAsAdmin', 'toastEnabled', 'beepEnabled', 'launchOnStartup', 'startMinimized', 'closeToTray']) settings[k] = $(k).checked;
  for (const k of ['quality', 'resolution', 'frameRate', 'codec', 'encoderPreset', 'encoder']) settings[k] = $(k).value;
  settings.multipass = $('multipass').value === 'true';
  settings.outputDir = $('outputDir').value;
}

const icon = (path) => `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${path}</svg>`;
const SECTIONS = [
  ['general', 'General', 'Startup, tray, administrator, notifications', '<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.2 2.2M16.2 16.2l2.2 2.2M5.6 18.4l2.2-2.2M16.2 7.8l2.2-2.2"/>'],
  ['buffer', 'Buffer', 'How far back you can save', '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>'],
  ['video', 'Video', 'Quality, resolution, frame rate, encoder', '<rect x="3" y="6" width="13" height="12" rx="2.5"/><path d="M16 10l5-3v10l-5-3"/>'],
  ['audio', 'Audio', 'Game audio, devices, apps, tracks', '<path d="M4 10v4h4l5 4V6L8 10zM16.5 9a4 4 0 0 1 0 6M19 6.5a8 8 0 0 1 0 11"/>'],
  ['hotkeys', 'Hotkeys', 'Clip lengths and keyboard shortcuts', '<rect x="3" y="6" width="18" height="12" rx="2.5"/><path d="M7 10h.01M11 10h.01M15 10h.01M7 14h10"/>'],
  ['recording', 'Recording', 'Split long recordings and markers', '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="3.5" fill="currentColor" stroke="none"/>'],
  ['performance', 'Performance', 'Live frame rate, dropped frames, CPU', '<path d="M3 13h4l3 7 4-16 3 9h4"/>'],
  ['storage', 'Storage', 'Where clips are saved', '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>'],
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
api.on('capture', (c) => { capture = c; renderStatus(); });
api.on('panel-shown', () => { refresh(); updateLiveViews(); });
window.addEventListener('focus', updateLiveViews);
document.addEventListener('visibilitychange', updateLiveViews);
api.on('feedback', (f) => {
  if (f.kind === 'clip' && f.ok) {
    pulseHalo();
    if (f.short) showBanner(`Only ${Math.round(f.actualSeconds)}s was buffered, so the clip is shorter than the ${f.requestedSeconds}s you asked for.`);
    else showBanner(`Saved ${Math.round(f.actualSeconds)}s clip`, 'good');
  } else if (f.kind === 'recording-saved') {
    showBanner('Recording saved', 'good');
  } else if (f.kind === 'exported') {
    showBanner('Edited clip saved', 'good');
  } else if (f.kind === 'clean-mic' && f.ok) {
    showBanner(f.cleanMic ? 'Clean Mic is on' : 'Clean Mic is off', 'good');
  } else if (f.kind === 'marker' && f.ok) {
    showBanner(`Marker ${f.number} added`, 'good');
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
$('view-all').onclick = () => setLibrary(true);
$('library-back').onclick = () => setLibrary(false);
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
for (const id of ['recordSplit', 'gameAudio','gameAudioOwnTrack', 'encoder', 'toastEnabled', 'beepEnabled', 'startMinimized', 'closeToTray', 'launchOnStartup']) $(id).onchange = () => scheduleSave();
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
$('merge-selected').onclick = () => {
  const ordered = media.filter((m) => selected.has(m.file)).reverse().map((m) => m.file);
  api.invoke('open-editor', ordered);
  selected.clear();
  renderMedia();
};
$('clear-selected').onclick = () => { selected.clear(); renderMedia(); };
$('restart-admin').onclick = () => api.invoke('restart-admin').catch((e) => showBanner(errorText(e), 'bad'));
$('add-preset').onclick = () => { settings.presets.push({ seconds: 45, hotkey: '' }); renderPresets(); scheduleSave(); };
$('pick-folder').onclick = async () => { const dir = await api.invoke('pick-folder'); if (dir) { $('outputDir').value = dir; scheduleSave(); } };
$('quality').onchange = () => { applyQuality(); scheduleSave(); };
$('bufferMinutes').onchange = () => { renderEstimate(); scheduleSave(); };
$('quit').onclick = () => api.invoke('quit-app');

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
