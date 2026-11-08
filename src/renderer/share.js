const api = window.afterclip;
const $ = (id) => document.getElementById(id);
const video = $('share-video');
const hint = 'Drag the handles to trim, or click the filmstrip to seek.';
const clock = (s) => {
  const tenths = Math.round(s * 10);
  return Math.floor(tenths / 600) + ':' + (tenths % 600 / 10).toFixed(1).padStart(4, '0');
};
let clip, duration = 0, start = 0, end = 0, request = 0, busy = false;
const hasTrim = () => start > 0.05 || end < duration - 0.05;
const percent = (s) => (duration ? Math.min(100, s / duration * 100) : 0) + '%';

function note(message, warn = false) {
  $('share-note').textContent = message;
  $('share-note').classList.toggle('warn', warn);
}

function renderActions() {
  $('share-trim').disabled = busy || !hasTrim();
  for (const id of ['share-compress', 'share-vertical', 'share-discord', 'share-vertical-discord']) $(id).disabled = busy;
}

function renderPlayhead() {
  $('share-playhead').style.left = percent(video.currentTime);
}

function renderTrim() {
  for (const [side, time] of [['start', start], ['end', end]]) {
    const handle = $('trim-' + side);
    handle.style.left = percent(time);
    handle.setAttribute('aria-valuemin', '0');
    handle.setAttribute('aria-valuemax', duration);
    handle.setAttribute('aria-valuenow', time);
    handle.setAttribute('aria-valuetext', clock(time));
  }
  $('trim-keep').style.left = percent(start);
  $('trim-keep').style.width = percent(end - start);
  $('trim-before').style.width = percent(start);
  $('trim-after').style.width = percent(duration - end);
  $('trim-from').textContent = clock(start);
  $('trim-to').textContent = clock(end);
  $('trim-length').textContent = hasTrim() ? 'Keeping ' + clock(end - start) + ' of ' + clock(duration) : 'Whole clip · ' + clock(duration);
  renderActions();
  renderPlayhead();
}

function seek(time) {
  if (video.readyState > 0) video.currentTime = time;
}

function moveHandle(side, time) {
  const minimum = Math.min(duration, 0.5);
  if (side === 'start') start = Math.max(0, Math.min(time, end - minimum));
  else end = Math.min(duration, Math.max(time, start + minimum));
  video.pause();
  seek(side === 'start' ? start : end);
  renderTrim();
}

async function loadClip() {
  const generation = ++request;
  $('share-tools').hidden = true;
  video.pause();
  video.removeAttribute('src');
  video.load();
  note('Loading clip…');
  try {
    const source = await api.invoke('share-source');
    if (generation !== request) return;
    clip = source;
    $('share-name').textContent = clip.name;
    $('share-name').title = clip.name;
    $('trim-strip').style.backgroundImage = '';
    video.src = api.videoUrl(clip.file);
    $('share-webhook').replaceChildren(...clip.channels.map((channel) => new Option(channel.name, channel.index)));
    $('share-webhook').hidden = clip.channels.length < 2;
    const [length, tracks] = await Promise.all([
      api.invoke('share-duration', clip.file),
      api.invoke('clip-tracks', clip.file).catch(() => 1),
    ]);
    if (generation !== request) return;
    if (!(length > 0)) throw new Error("Couldn't read the clip length.");
    duration = end = length;
    start = 0;
    $('share-tracks').hidden = tracks < 2;
    $('share-track-list').replaceChildren(...Array.from({ length: tracks }, (_, i) => {
      const label = document.createElement('label');
      const box = document.createElement('input');
      box.type = 'checkbox';
      box.value = i;
      box.checked = i === 0;
      label.append(box, i === 0 ? 'Main mix' : 'Track ' + (i + 1));
      return label;
    }));
    renderTrim();
    $('share-tools').hidden = false;
    setupShareTools(clip);
    note(video.error ? 'Video preview unavailable. You can still use the filmstrip.' : hint);
    api.invoke('preview-strip', clip.file).then((strip) => {
      if (generation === request && strip?.url) $('trim-strip').style.backgroundImage = 'url("' + strip.url + '")';
    }).catch(() => undefined);
  } catch (error) {
    if (generation === request) note(error.message, true);
  }
}

async function runShare(action, working, success, recipe) {
  if (busy || $('share-tools').hidden) return;
  const tracks = [...$('share-track-list').querySelectorAll('input:checked')].map((box) => Number(box.value));
  if (!recipe && action !== 'trim' && !$('share-tracks').hidden && !tracks.length) return note('Tick at least one audio track.', true);
  if (action.endsWith('discord') && !clip.channels.length) return note('Add a Discord channel in Settings → Clips and sharing.', true);
  const payload = { file: clip.file, action, tracks, webhook: Number($('share-webhook').value || 0) };
  if (recipe) payload.recipe = recipe;
  if (action.startsWith('vertical') && activeLayout) payload.layout = activeLayout;
  if (hasTrim()) Object.assign(payload, { start, end });
  const generation = request;
  busy = true;
  renderActions();
  note(working);
  try {
    const output = await api.invoke('share-clip', payload);
    if (generation === request) note(output ? success + ' ' + output.split(/[\\/]/).at(-1) : success);
  } catch (error) {
    if (generation === request) note(error.message, true);
  } finally {
    busy = false;
    renderActions();
  }
}

const bar = $('trim');
function timeAt(x) {
  const bounds = bar.getBoundingClientRect();
  return Math.max(0, Math.min(1, (x - bounds.left) / bounds.width)) * duration;
}
let dragging = null;
bar.addEventListener('pointerdown', (event) => {
  if (event.button !== 0) return;
  const side = { 'trim-start': 'start', 'trim-end': 'end' }[event.target.id];
  if (!side) return seek(timeAt(event.clientX));
  dragging = { side, x: event.clientX, time: side === 'start' ? start : end };
  bar.setPointerCapture(event.pointerId);
  event.target.focus();
  event.preventDefault();
});
bar.addEventListener('pointermove', (event) => {
  if (dragging) moveHandle(dragging.side, dragging.time + timeAt(event.clientX) - timeAt(dragging.x));
});
bar.addEventListener('lostpointercapture', () => { dragging = null; });
for (const side of ['start', 'end']) {
  $('trim-' + side).addEventListener('keydown', (event) => {
    const step = event.shiftKey ? 5 : 0.1;
    const time = side === 'start' ? start : end;
    const target = { ArrowLeft: time - step, ArrowDown: time - step, ArrowRight: time + step, ArrowUp: time + step, Home: 0, End: duration }[event.key];
    if (target === undefined) return;
    event.preventDefault();
    moveHandle(side, target);
  });
}
video.addEventListener('timeupdate', renderPlayhead);
video.addEventListener('seeked', renderPlayhead);
video.addEventListener('error', () => {
  if (!$('share-tools').hidden && !busy) note('Video preview unavailable. You can still use the filmstrip.');
});
$('share-trim').onclick = () => runShare('trim', 'Saving trimmed copy…', 'Saved');
$('share-compress').onclick = () => runShare('compress', 'Compressing…', 'Saved');
$('share-vertical').onclick = () => runShare('vertical', 'Making vertical copy…', 'Saved');
$('share-discord').onclick = () => runShare('discord', 'Sending to Discord…', 'Sent to Discord.');
$('share-vertical-discord').onclick = () => runShare('vertical-discord', 'Making and sending a vertical copy…', 'Sent vertical clip to Discord.');
window.addEventListener('DOMContentLoaded', () => {
  api.on('share-source', loadClip).catch(() => undefined).then(() => {
    api.ready();
    return loadClip();
  });
});
