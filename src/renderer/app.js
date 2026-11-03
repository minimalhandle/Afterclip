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
let activeProfile = null;
let activeBuffer = null; // The running game's profile can use a different buffer from Settings.
let renaming = null;
let tagging = null;
let cameraChoices; // Undefined until asked for, and null if capture couldn't list them.
const strips = new Map(); // file|mtime -> { url, frames }, or a promise while one is being made
let favouritesOnly = false;
let update = { status: 'idle' };
let appVersion = '';
let storageUsed = 0;

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
  const shining = card.classList.contains('saved');
  card.className = `capture-card ${status.recording ? 'recording' : ready ? 'live' : status.state === 'error' ? 'bad' : ''}`;
  card.classList.toggle('saved', shining);
  const labels = {
    desktop: 'Recording primary monitor',
    detecting: `Connecting to ${capture.app ?? 'game'}`,
    game: capture.app ?? 'Game',
    fallback: capture.app ?? 'Fullscreen app',
  };
  let title = labels[capture.mode] ?? 'Recording';
  let detail = capture.mode === 'game' ? 'Game capture' : capture.mode === 'fallback' ? 'Recording its monitor' : capture.mode === 'detecting' ? 'Recording its monitor for now' : 'Waiting for a game';
  if (status.state === 'error') { title = 'Capture stopped'; detail = status.message ?? ''; }
  else if (status.waitingForGame) { title = 'Waiting for a game'; detail = 'The buffer starts when a game opens'; }
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
  renderBuffer();
}

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
    : !status.buffering ? (status.waitingForGame ? 'Off until a game opens' : 'Not buffering')
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
  $('summary').textContent = activeProfile
    ? `${settings.gameProfiles.find((p) => p.exe === activeProfile)?.name ?? activeProfile} profile · ${bufferLabel(activeBuffer ?? settings.bufferSeconds)} buffer`
    : `${quality} · ${bufferLabel(settings.bufferSeconds)} buffer · ${settings.fps} FPS`;
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
function wireScrub(thumb, m, key) {
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
      scrub.style.backgroundSize = `${strip.frames * 100}% 100%`;
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
    Promise.resolve(strips.get(key)).then(show);
  };
  thumb.addEventListener('mouseenter', () => { over = true; timer = setTimeout(load, SCRUB_DELAY_MS); });
  thumb.addEventListener('mousemove', (e) => {
    const box = thumb.getBoundingClientRect();
    x = Math.min(0.999, Math.max(0, (e.clientX - box.left) / box.width));
    const known = strips.get(key);
    if (known && !(known instanceof Promise)) show(known);
  });
  thumb.addEventListener('mouseleave', () => {
    over = false;
    clearTimeout(timer);
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
  if (failed.length) showBanner(`Couldn't delete ${failed.length} of ${files.length}. They may be open in another app.`, 'bad');
  else showBanner(`Moved ${files.length} to the Recycle Bin`, 'good');
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

function filtered(all) {
  const words = $('library-search').value.trim().toLowerCase();
  const game = $('library-game').value;
  const tag = $('library-tag').value;
  return all
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
  $('view-all').textContent = `View all (${all.length})`;
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
    if (m.kind !== 'screenshot') wireScrub(thumb, m, key);

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
  const video = m.kind !== 'screenshot';
  menu.append(
    item(video ? 'Play' : 'Open', () => api.invoke('open', m.file)),
    ...(video ? [item('Edit', () => api.invoke('open-editor', [m.file]))] : []),
    item('Rename', () => { renaming = m.file; renderMedia(); }),
    item('Tags', () => { tagging = m.file; renderMedia(); }),
    item('Copy file', () => api.invoke('copy-file', m.file).then(() => showBanner('Copied. Paste it into any app.', 'good'), (e) => showBanner(errorText(e), 'bad'))),
    item('Send to Discord', () => {
      if (!settings.discordWebhook) return showBanner('Add your Discord webhook in Settings, under Clips and sharing.', 'bad');
      showBanner('Sending to Discord. A long clip is made smaller first, and you can keep playing.');
      return api.invoke('send-to-discord', m.file).catch(() => undefined);
    }),
    ...(video ? [
      item('Compress', () => {
        showBanner('Making a smaller copy. A long clip can take a while, and you can keep playing.');
        api.invoke('compress-clip', m.file).catch(() => undefined);
      }),
      item('Make vertical copy', () => {
        showBanner('Making a vertical copy for phones. A long clip can take a while, and you can keep playing.');
        api.invoke('vertical-clip', m.file).catch(() => undefined);
      }),
    ] : []),
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
  activeProfile = state.activeProfile ?? null;
  activeBuffer = state.activeBufferSeconds ?? null;
  update = state.update ?? update;
  appVersion = state.version ?? appVersion;
  storageUsed = state.storageUsedBytes ?? 0;
  controllersSeen = state.controllers ?? controllersSeen;
  renderControllersSeen();
  renderUpdate();
  renderStorageUsed();
  // Only this comes from the backend after startup: the form owns the rest while it's being edited.
  if (settings) {
    settings.micMuted = state.settings.micMuted;
    settings.cleanMic = state.settings.cleanMic;
    // The Stream Deck can switch the webcam too.
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
  const none = cameraChoices ? 'No webcam found' : cameraChoices === undefined ? 'Looking for webcams' : status.waitingForGame ? 'Start a game to list webcams' : 'Webcams can be listed once capture starts';
  if (!select.options.length) select.add(new Option(none, ''));

  select.value = settings.cameraDevice;
  if (!settings.cameraDevice && choices.length) select.value = choices[0].id;
  const on = $('cameraEnabled').checked;
  for (const id of ['cameraDevice', 'cameraCorner', 'cameraSize', 'verticalFaceCam']) $(id).disabled = !on;
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

/* Updates */

const UPDATE_COUNTDOWN_REFRESH_MS = 30000;
let updateTimer = null;
function renderUpdate() {
  const { status: state, version, message, installsAt } = update;
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
    card.append(
      head,
      record,
      profileSelect('Quality', p.quality, [USE_DEFAULT, ['low', 'Low'], ['medium', 'Medium'], ['high', 'High'], ['full', 'Full quality']], (v) => { p.quality = optional(v); }),
      profileSelect('Frame rate', p.frameRate, [USE_DEFAULT, ['60', '60 fps'], ['120', '120 fps'], ['native', 'Native']], (v) => { p.frameRate = optional(v); }),
      profileSelect('Buffer', p.bufferSeconds, [USE_DEFAULT, ['60', '1 minute'], ['120', '2 minutes'], ['300', '5 minutes'], ['600', '10 minutes'], ['1800', '30 minutes']], (v) => { p.bufferSeconds = v ? Number(v) : undefined; }),
      profileSelect('Game audio', p.gameAudio === undefined || p.gameAudio === null ? '' : String(p.gameAudio), [USE_DEFAULT, ['true', 'On'], ['false', 'Off']], (v) => { p.gameAudio = v ? v === 'true' : undefined; }),
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
const DISCORD_HELP = $('discord-note').textContent;

function renderDiscordNote() {
  const typed = $('discordWebhook').value.trim();
  const wrong = typed && !DISCORD_WEBHOOK.test(typed);
  $('discord-note').textContent = wrong ? 'That doesn\'t look like a Discord webhook address. It starts with https://discord.com/api/webhooks/' : DISCORD_HELP;
  $('discord-note').classList.toggle('warn', !!wrong);
}

function fillForm() {
  for (const k of ['encoder', 'outputDir', 'quality', 'codec', 'encoderPreset']) $(k).value = settings[k];
  ensureOption($('storageLimitGb'), settings.storageLimitGb, `${settings.storageLimitGb} GB`);
  $('storageLimitGb').value = settings.storageLimitGb;
  ensureOption($('deleteAfterDays'), settings.deleteAfterDays, `After ${settings.deleteAfterDays} days`);
  $('deleteAfterDays').value = settings.deleteAfterDays;
  $('discordWebhook').value = settings.discordWebhook;
  renderDiscordNote();
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
  ensureOption($('clipAfterSeconds'), settings.clipAfterSeconds, `${settings.clipAfterSeconds} seconds`);
  ensureOption($('cameraSize'), settings.cameraSize, `${settings.cameraSize}%`);
  ensureOption($('lowSpaceWarnGb'), settings.lowSpaceWarnGb, `${settings.lowSpaceWarnGb} GB`);
  for (const k of ['clipAfterSeconds', 'cameraCorner', 'cameraSize', 'lowSpaceWarnGb']) $(k).value = settings[k];
  bindTalkKey($('pushToTalkKey'));
  renderCamera();
  bindHotkey($('recordHotkey'), () => settings.recordHotkey, (v) => { settings.recordHotkey = v; scheduleSave(); });
  bindHotkey($('micHotkey'), () => settings.micHotkey, (v) => { settings.micHotkey = v; scheduleSave(); });
  bindHotkey($('markerHotkey'), () => settings.markerHotkey, (v) => { settings.markerHotkey = v; scheduleSave(); });
  bindHotkey($('screenshotHotkey'), () => settings.screenshotHotkey, (v) => { settings.screenshotHotkey = v; scheduleSave(); });
  ensureOption($('recordSplit'), settings.recordSplitSeconds, `Every ${settings.recordSplitSeconds} seconds`);
  $('recordSplit').value = settings.recordSplitSeconds;
  $('game-level').replaceChildren(levelControl(-1, settings.gameAudioVolume, (v) => { settings.gameAudioVolume = v; }));
}

const SWITCHES = ['gameAudio', 'gameAudioOwnTrack', 'runAsAdmin', 'toastEnabled', 'beepEnabled', 'launchOnStartup', 'startMinimized', 'closeToTray', 'autoUpdate',
  'bufferOnlyInGames', 'cameraEnabled', 'verticalFaceCam', 'pushToTalk', 'gameFolders'];
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
  settings.storageLimitGb = Number($('storageLimitGb').value);
  settings.deleteAfterDays = Number($('deleteAfterDays').value);
  settings.discordWebhook = $('discordWebhook').value.trim();
  settings.bufferSeconds = Math.round(Number($('bufferMinutes').value) * 60);
  for (const k of SWITCHES) settings[k] = $(k).checked;
  for (const k of ['quality', 'resolution', 'frameRate', 'codec', 'encoderPreset', 'encoder', 'cameraCorner']) settings[k] = $(k).value;
  for (const k of ['clipAfterSeconds', 'cameraSize', 'lowSpaceWarnGb']) settings[k] = Number($(k).value);
  settings.cameraDevice = $('cameraDevice').value || settings.cameraDevice;
  settings.multipass = $('multipass').value === 'true';
  settings.outputDir = $('outputDir').value;
}

const icon = (path) => `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${path}</svg>`;
const SECTIONS = [
  ['general', 'General', 'Startup, notifications, updates', '<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.2 2.2M16.2 16.2l2.2 2.2M5.6 18.4l2.2-2.2M16.2 7.8l2.2-2.2"/>'],
  ['video', 'Video', 'Quality, buffer, webcam, performance', '<rect x="3" y="6" width="13" height="12" rx="2.5"/><path d="M16 10l5-3v10l-5-3"/>'],
  ['audio', 'Audio', 'Push to talk, game audio, devices', '<path d="M4 10v4h4l5 4V6L8 10zM16.5 9a4 4 0 0 1 0 6M19 6.5a8 8 0 0 1 0 11"/>'],
  ['controls', 'Controls', 'Hotkeys, clip lengths, controller', '<rect x="3" y="6" width="18" height="12" rx="2.5"/><path d="M7 10h.01M11 10h.01M15 10h.01M7 14h10"/>'],
  ['games', 'Games', 'Per-game settings and auto-record', '<rect x="2.5" y="7" width="19" height="11" rx="4.5"/><path d="M7 11v3M5.5 12.5h3M15.5 11.5h.01M18 13.5h.01"/>'],
  ['clips', 'Clips and sharing', 'Save location, folders, Discord', '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>'],
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
  if (id === 'video') loadCameraChoices();
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
    showBanner('Sent to Discord', 'good');
  } else if (f.kind === 'storage') {
    showBanner(`Moved ${f.removed} old ${f.removed === 1 ? 'file' : 'files'} to the Recycle Bin, as your storage settings ask.`, 'good');
  } else if (f.kind === 'exported') {
    showBanner('Edited clip saved', 'good');
  } else if (f.kind === 'clean-mic' && f.ok) {
    showBanner(f.cleanMic ? 'Clean Mic is on' : 'Clean Mic is off', 'good');
  } else if (f.kind === 'marker' && f.ok) {
    showBanner(`Marker ${f.number} added`, 'good');
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
$('library-search').oninput = () => renderMedia();
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
$('autoUpdate').onchange = () => scheduleSave();
$('storageLimitGb').onchange = () => scheduleSave();
$('deleteAfterDays').onchange = () => scheduleSave();
$('discordWebhook').oninput = () => { renderDiscordNote(); scheduleSave(TYPING_DELAY_MS); };
$('profile-browse').onclick = async () => { const exe = await api.invoke('pick-game'); if (exe) addProfile(exe); };
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
$('screenshot').onclick = () => api.invoke('screenshot').catch(() => undefined);
for (const id of ['recordSplit', 'gameAudio','gameAudioOwnTrack', 'encoder', 'toastEnabled', 'beepEnabled', 'startMinimized', 'closeToTray', 'launchOnStartup',
  'clipAfterSeconds', 'bufferOnlyInGames', 'verticalFaceCam', 'pushToTalk', 'gameFolders', 'lowSpaceWarnGb', 'cameraCorner', 'cameraSize']) $(id).onchange = () => scheduleSave();
$('cameraEnabled').onchange = () => { renderCamera(); scheduleSave(); };
$('cameraDevice').onchange = () => { settings.cameraDevice = $('cameraDevice').value; scheduleSave(); };
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
  const ordered = media.filter((m) => selected.has(m.file) && m.kind !== 'screenshot').reverse().map((m) => m.file);
  api.invoke('open-editor', ordered);
  selected.clear();
  renderMedia();
};
$('delete-selected').onclick = deleteSelected;
$('clear-selected').onclick = () => { selected.clear(); renderMedia(); };
$('restart-admin').onclick = () => api.invoke('restart-admin').catch((e) => showBanner(errorText(e), 'bad'));
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
