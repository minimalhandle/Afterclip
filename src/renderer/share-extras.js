let blurRegion = null;
let cuts = [];
let phoneTimer = null;

function setupExtras() {
  blurRegion = null;
  pinSelection.hidden = true;
  $('redact-audio').value = '';
  $('redact-from').value = start.toFixed(2);
  $('redact-to').value = end.toFixed(2);
  const tracks = [...$('share-track-list').querySelectorAll('input')];
  $('redact-track').replaceChildren(new Option('All audio tracks', ''),
    ...(tracks.length ? tracks.map((track, index) => new Option(index === 0 ? 'Main mix' : 'Track ' + (index + 1), index))
      : [new Option('Main mix', '0')]));
  $('bookmark-results').replaceChildren();
  $('angle-follow').checked = false;
  cuts = [];
  renderCuts();
}

function setBlurRegion(rect) {
  blurRegion = rect;
  note('Blur region selected. Set the time range and save a redacted copy.');
}

$('redact-region').onclick = () => beginRegion('redaction');
$('redact-clear').onclick = () => { blurRegion = null; pinSelection.hidden = true; note('Blur region cleared.'); };
$('redact-mark-from').onclick = () => { $('redact-from').value = video.currentTime.toFixed(2); };
$('redact-mark-to').onclick = () => { $('redact-to').value = video.currentTime.toFixed(2); };

async function runEdit(command, payload, message) {
  if (busy) return;
  busy = true;
  renderActions();
  const generation = request;
  note(message);
  try {
    const output = await api.invoke(command, payload);
    if (generation === request) note('Saved ' + output.split(/[\\/]/).at(-1));
  } catch (error) {
    if (generation === request) note(error.message, true);
  } finally {
    busy = false;
    renderActions();
  }
}

$('redact-export').onclick = () => {
  const mode = $('redact-audio').value;
  if (!blurRegion && !mode) return note('Draw a blur region or choose an audio edit first.', true);
  const from = Number($('redact-from').value);
  const to = Number($('redact-to').value);
  const track = $('redact-track').value;
  const blur = blurRegion ? { rect: blurRegion, from, to } : null;
  const audio = mode ? { track: track === '' ? null : Number(track), bleep: mode === 'bleep', from, to } : null;
  runEdit('render-clip', { file: clip.file, start, end, blur, audio, vertical: false }, 'Saving redacted copy…');
};

function renderCuts() {
  $('angle-cuts').replaceChildren(...cuts.map((cut, index) => {
    const remove = document.createElement('button');
    remove.className = 'small';
    remove.textContent = clock(cut.time) + ' → Angle ' + cut.angle + ' ×';
    remove.title = 'Remove cut';
    remove.onclick = () => { cuts.splice(index, 1); renderCuts(); };
    return remove;
  }));
}

$('angle-cut').onclick = () => {
  if (!angle.getAttribute('src')) return note('Choose and match a second clip first.', true);
  if (cuts.length >= 24) return note('Remove a cut before adding another.', true);
  const time = Math.round(video.currentTime * 100) / 100;
  if (time < start - 0.005 || time >= end) return note('Move the playhead inside the export range.', true);
  const cut = { time: Math.max(start, time), angle: Number($('angle-cut-choice').value) };
  cuts = [...cuts.filter((previous) => Math.abs(previous.time - cut.time) >= 0.09), cut].sort((a, b) => a.time - b.time);
  renderCuts();
};
$('angle-export').onclick = () => {
  if (!cuts.length) return note('Add a cut where you want the video to switch angles.', true);
  runEdit('export-angles', { file: clip.file, start, end, offset: Number($('angle-offset').value), cuts,
    continuousAudio: $('angle-audio').checked }, 'Rendering the angle edit…');
};
function previewCuts() {
  if (!$('angle-follow').checked || $('angles').hidden || !angle.getAttribute('src')) return;
  const cut = cuts.findLast((cut) => cut.time <= video.currentTime);
  const second = cut?.angle === 2;
  if (second !== showingAngle) showAngle(second);
}
video.addEventListener('timeupdate', previewCuts);
video.addEventListener('seeked', previewCuts);
$('angle-follow').onchange = previewCuts;

$('bookmark-load').onclick = withNote(async () => {
  const source = clip;
  const marks = await api.invoke('clip-bookmarks', source.file);
  if (source !== clip) return;
  const rows = marks.map((mark) => {
    const row = document.createElement('div');
    row.className = 'row';
    const preview = document.createElement('button');
    preview.className = 'small';
    preview.textContent = clock(mark.time) + ' · ' + mark.title;
    preview.onclick = () => seek(Math.min(mark.time, duration));
    const range = document.createElement('button');
    range.className = 'small';
    range.textContent = 'Set trim';
    range.onclick = () => {
      const seconds = (id) => Math.min(120, Math.max(0, Number($(id).value) || 0));
      start = Math.max(0, Math.min(mark.time - seconds('bookmark-before'), duration - 0.5));
      end = Math.min(duration, Math.max(start + 0.5, mark.time + seconds('bookmark-after')));
      video.pause();
      seek(start);
      renderTrim();
      note('Bookmark range selected. Save the trimmed copy when ready.');
    };
    row.append(preview, range);
    return row;
  });
  $('bookmark-results').replaceChildren(...rows);
  if (!rows.length) $('bookmark-results').textContent = 'No automatic bookmarks in this recording.';
});

function clearPhoneLink(text) {
  clearTimeout(phoneTimer);
  $('phone-qr').hidden = true;
  $('phone-qr').removeAttribute('src');
  $('phone-link').textContent = text;
}

$('phone-start').onclick = withNote(async () => {
  const url = await api.invoke('phone-share', clip.file);
  const qr = qrcode(0, 'M');
  qr.addData(url);
  qr.make();
  $('phone-qr').src = qr.createDataURL(5, 20);
  $('phone-qr').hidden = false;
  $('phone-link').textContent = url + ' · Expires in 10 minutes.';
  clearTimeout(phoneTimer);
  phoneTimer = setTimeout(() => clearPhoneLink('Phone link expired.'), 600000);
});
$('phone-stop').onclick = withNote(async () => {
  await api.invoke('stop-phone-share');
  clearPhoneLink('Phone sharing stopped.');
});
