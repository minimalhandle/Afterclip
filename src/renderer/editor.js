const api = window.afterclip;
const $ = (id) => document.getElementById(id);
const video = $('video');
const MIN_LENGTH = 0.5;
const KEYFRAME_SECONDS = 1;

let segments = []; // { id, file, name, url, duration, start, end }
let currentId = null;
let nextId = 1;
let playingAll = false;
let library = [];
const info = new Map();

const clock = (seconds, tenths = false) => {
  const s = Math.max(0, seconds);
  const whole = Math.floor(s);
  const base = `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`;
  return tenths ? `${base}.${Math.floor((s - whole) * 10)}` : base;
};
const displayName = (name) => {
  const m = /^(.*?)_\d{4}-\d{2}-\d{2}_/.exec(name);
  return m ? (/^(Clip|Recording)$/.test(m[1]) ? 'Desktop' : m[1]) : name.replace(/\.[^.]+$/, '');
};
const current = () => segments.find((s) => s.id === currentId);
const setMessage = (text, bad) => {
  $('message').textContent = text;
  $('message').classList.toggle('warn', !!bad);
};

async function infoFor(file) {
  if (!info.has(file)) info.set(file, await api.invoke('thumbnail', file));
  return info.get(file);
}

async function addClip(file) {
  const details = await infoFor(file);
  if (!details) {
    setMessage(`Couldn't read ${file}.`, true);
    return;
  }
  const name = file.split(/[\\/]/).pop();
  const segment = { id: nextId++, file, name, url: details.url, duration: details.duration, start: 0, end: details.duration };
  segments.push(segment);
  select(segment.id);
  renderSegments();
}

function select(id, seekTo) {
  const segment = segments.find((s) => s.id === id);
  if (!segment) return;
  const changed = currentId !== id;
  currentId = id;
  const src = api.videoUrl(segment.file);
  if (video.src !== src) {
    video.src = src;
    video.addEventListener('loadedmetadata', () => { video.currentTime = seekTo ?? segment.start; }, { once: true });
  } else if (changed || seekTo !== undefined) {
    video.currentTime = seekTo ?? segment.start;
  }
  renderSegments();
  renderTrim();
}

function renderTotal() {
  const total = segments.reduce((sum, s) => sum + (s.end - s.start), 0);
  $('total').textContent = clock(total);
  const none = segments.length === 0;
  $('export').disabled = none;
  $('export').textContent = segments.length > 1 ? 'Merge and save' : 'Save video';
  for (const id of ['play', 'set-start', 'set-end']) $(id).disabled = none;
  $('play-all').disabled = segments.length < 2;
  $('player-empty').hidden = !none;
  $('trim').hidden = none;
}

function move(from, to) {
  if (to < 0 || to >= segments.length) return;
  const [segment] = segments.splice(from, 1);
  segments.splice(to, 0, segment);
  renderSegments();
}

function renderSegments() {
  const list = $('segments');
  if (!segments.length) {
    const empty = document.createElement('div');
    empty.className = 'segments-empty';
    empty.textContent = 'Clips you add appear here, in the order they\'ll play.';
    list.replaceChildren(empty);
    renderTotal();
    return;
  }
  list.replaceChildren(...segments.map((s, index) => {
    const card = document.createElement('div');
    card.className = 'segment';
    card.classList.toggle('current', s.id === currentId);
    card.draggable = true;
    card.title = s.name;
    card.onclick = () => select(s.id);
    card.ondragstart = (e) => { e.dataTransfer.setData('text/plain', String(index)); card.classList.add('dragging'); };
    card.ondragend = () => card.classList.remove('dragging');
    card.ondragover = (e) => e.preventDefault();
    card.ondrop = (e) => { e.preventDefault(); move(Number(e.dataTransfer.getData('text/plain')), index); };
    const thumb = document.createElement('div');
    thumb.className = 'thumb';
    const img = document.createElement('img');
    img.alt = '';
    img.src = s.url;
    thumb.append(img);
    const foot = document.createElement('div');
    foot.className = 'segment-foot';
    const length = document.createElement('span');
    length.textContent = clock(s.end - s.start, true);
    const button = (label, aria, fn) => {
      const b = document.createElement('button');
      b.textContent = label;
      b.setAttribute('aria-label', `${aria} ${s.name}`);
      b.title = aria;
      b.onclick = (e) => { e.stopPropagation(); fn(); };
      return b;
    };
    foot.append(
      length,
      button('‹', 'Move earlier', () => move(index, index - 1)),
      button('›', 'Move later', () => move(index, index + 1)),
      button('×', 'Remove', () => {
        segments.splice(index, 1);
        if (s.id === currentId) {
          currentId = null;
          video.removeAttribute('src');
          video.load();
          const next = segments[Math.min(index, segments.length - 1)];
          if (next) { select(next.id); return; }
        }
        renderSegments();
        renderTrim();
      }),
    );
    card.append(thumb, foot);
    return card;
  }));
  renderTotal();
}

function renderTrim() {
  const s = current();
  if (!s) return;
  const pct = (t) => `${(t / s.duration) * 100}%`;
  $('kept').style.left = pct(s.start);
  $('kept').style.width = `${((s.end - s.start) / s.duration) * 100}%`;
  $('handle-start').style.left = pct(s.start);
  $('handle-end').style.left = pct(s.end);
  renderPlayhead();
}

function renderPlayhead() {
  const s = current();
  if (!s) return;
  $('playhead').style.left = `${(video.currentTime / s.duration) * 100}%`;
  $('time').textContent = `${clock(video.currentTime, true)} / ${clock(s.duration, true)}`;
}

function setTrim(which, time) {
  const s = current();
  if (!s) return;
  if (which === 'start') s.start = Math.min(Math.max(0, time), s.end - MIN_LENGTH);
  else s.end = Math.max(Math.min(s.duration, time), s.start + MIN_LENGTH);
  renderTrim();
  renderSegments();
}

function bindHandle(handle, which) {
  handle.onpointerdown = (down) => {
    down.preventDefault();
    handle.setPointerCapture(down.pointerId);
    video.pause();
    const rect = $('trim').getBoundingClientRect();
    handle.onpointermove = (e) => {
      const s = current();
      if (!s) return;
      setTrim(which, ((e.clientX - rect.left) / rect.width) * s.duration);
      video.currentTime = which === 'start' ? s.start : s.end;
    };
    handle.onpointerup = () => { handle.onpointermove = null; handle.onpointerup = null; };
  };
  // Arrow keys nudge a handle, for anyone not using a mouse.
  handle.onkeydown = (e) => {
    const s = current();
    if (!s || !['ArrowLeft', 'ArrowRight'].includes(e.key)) return;
    e.preventDefault();
    const step = e.key === 'ArrowLeft' ? -0.1 : 0.1;
    setTrim(which, (which === 'start' ? s.start : s.end) + step);
    video.currentTime = which === 'start' ? s.start : s.end;
  };
}

function playSegment() {
  const s = current();
  if (!s) return;
  if (video.currentTime < s.start || video.currentTime >= s.end - 0.05) video.currentTime = s.start;
  video.play().catch((e) => setMessage(`This video can't be previewed here: ${e.message}`, true));
}

video.ontimeupdate = () => {
  renderPlayhead();
  const s = current();
  if (!s || video.paused || video.currentTime < s.end) return;
  if (playingAll) {
    const next = segments[segments.indexOf(s) + 1];
    if (next) {
      select(next.id);
      video.addEventListener('seeked', () => video.play(), { once: true });
      return;
    }
    playingAll = false;
  }
  video.pause();
};
video.onplay = () => { $('play').textContent = 'Pause'; };
video.onpause = () => { $('play').textContent = 'Play'; };
video.onerror = () => {
  if (video.getAttribute('src')) setMessage("This video can't be previewed here, but it can still be trimmed and saved.", true);
};
$('play').onclick = () => {
  playingAll = false;
  if (video.paused) playSegment(); else video.pause();
};
$('play-all').onclick = () => {
  playingAll = true;
  select(segments[0].id, segments[0].start);
  video.addEventListener('seeked', () => video.play(), { once: true });
};
$('set-start').onclick = () => setTrim('start', video.currentTime);
$('set-end').onclick = () => setTrim('end', video.currentTime);
$('trim').onclick = (e) => {
  const s = current();
  if (!s || e.target.classList.contains('trim-handle')) return;
  const rect = $('trim').getBoundingClientRect();
  video.currentTime = ((e.clientX - rect.left) / rect.width) * s.duration;
};
bindHandle($('handle-start'), 'start');
bindHandle($('handle-end'), 'end');

$('export').onclick = async () => {
  const button = $('export');
  const label = button.textContent;
  button.disabled = true;
  button.textContent = 'Saving';
  setMessage('Saving your video.');
  try {
    // An end at the very end of a clip keeps everything, including its final partial frame.
    const payload = segments.map((s) => ({ file: s.file, start: s.start, end: s.duration - s.end < 0.05 ? -1 : s.end }));
    const file = await api.invoke('export-edit', payload);
    setMessage(`Saved ${file.split(/[\\/]/).pop()} to your clips.`);
    loadLibrary();
  } catch (e) {
    setMessage(String(e.message ?? e), true);
  } finally {
    button.disabled = false;
    button.textContent = label;
  }
};

async function loadLibrary() {
  const state = await api.invoke('state');
  library = state.media;
  const list = $('library');
  list.replaceChildren(...library.map((m) => {
    const li = document.createElement('li');
    const b = document.createElement('button');
    b.className = 'lib-item';
    b.title = `Add ${m.name} to the timeline`;
    const thumb = document.createElement('div');
    thumb.className = 'thumb';
    const img = document.createElement('img');
    img.alt = '';
    const badge = document.createElement('span');
    badge.className = 'length';
    badge.hidden = true;
    thumb.append(img, badge);
    const title = document.createElement('div');
    title.className = 'item-title';
    title.textContent = displayName(m.name);
    const sub = document.createElement('div');
    sub.className = 'item-sub';
    sub.textContent = new Date(m.mtime).toLocaleString([], { dateStyle: 'short', timeStyle: 'short' });
    b.append(thumb, title, sub);
    b.onclick = () => addClip(m.file);
    li.append(b);
    b.dataset.file = m.file;
    observer.observe(b);
    return li;
  }));
}

const observer = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    if (!entry.isIntersecting) continue;
    observer.unobserve(entry.target);
    infoFor(entry.target.dataset.file).then((details) => {
      if (!details) return;
      entry.target.querySelector('img').src = details.url;
      const badge = entry.target.querySelector('.length');
      badge.textContent = clock(details.duration);
      badge.hidden = false;
    });
  }
});

async function takeFiles() {
  const files = await api.invoke('editor-files');
  for (const file of files) await addClip(file);
}

api.on('editor-files', takeFiles);
api.on('feedback', (f) => { if (f.ok && (f.kind === 'clip' || f.kind === 'recording-saved')) loadLibrary(); });

renderSegments();
Promise.all([loadLibrary(), takeFiles()])
  .catch((e) => setMessage(String(e.message ?? e), true))
  .finally(() => api.ready());
