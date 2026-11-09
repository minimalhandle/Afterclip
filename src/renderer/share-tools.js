let activeLayout = null;
let recipes = [];
const layoutPreview = $('vertical-preview');
const pinSelection = $('pin-selection');
const angle = $('angle-video');
let showingAngle = false;
let originalMute = false;
let pinStart = null;
let pinPurpose = 'layout';
let savedTranscript = [];

function withNote(task) {
  return async () => {
    try { await task(); }
    catch (error) { note(error.message, true); }
  };
}

function setupShareTools(source) {
  showAngle(false);
  angle.removeAttribute('src');
  angle.load();
  $('angle-offset').value = '0';
  updateAngleButtons();
  recipes = source.recipes || [];
  renderRecipes();
  $('recipe-name').value = '';
  $('recipe-size').value = '';
  $('recipe-even').checked = source.even;
  $('layout-save').disabled = !source.game;
  setLayout(source.layout || null);
  showTranscript(source.transcript || []);
  setupExtras();
  $('show-bookmarks').hidden = !source.hasBookmarks;
  if (!source.hasBookmarks) {
    $('bookmarks').hidden = true;
    $('show-bookmarks').setAttribute('aria-expanded', 'false');
  }
  if (source.at > 0) {
    const jump = () => { if (clip === source) seek(source.at); };
    if (video.readyState > 0) jump();
    else video.addEventListener('loadedmetadata', jump, { once: true });
  }
}

const optionNames = ['recipes', 'layout', 'speech', 'angles', 'edits', 'captions', 'bookmarks', 'phone'];
for (const name of optionNames) {
  $('show-' + name).onclick = () => {
    const opening = $(name).hidden;
    for (const option of optionNames) {
      $(option).hidden = option !== name || !opening;
      $('show-' + option).setAttribute('aria-expanded', String(!$(option).hidden));
    }
    layoutPreview.hidden = $('layout').hidden;
    pinSelection.hidden = true;
    if ($('angles').hidden) showAngle(false);
    drawLayout();
  };
}

function selectedTracks() {
  return [...$('share-track-list').querySelectorAll('input:checked')].map((box) => Number(box.value));
}

function renderRecipes(selected = '') {
  $('recipe-list').replaceChildren(new Option('Choose a recipe', ''), ...recipes.map((recipe) => new Option(recipe.name, recipe.name)));
  $('recipe-list').value = selected;
  $('recipe-run').disabled = !selected;
  $('recipe-delete').disabled = !selected;
}

$('recipe-list').onchange = () => {
  const recipe = recipes.find((r) => r.name === $('recipe-list').value);
  $('recipe-run').disabled = $('recipe-delete').disabled = !recipe;
  if (!recipe) return;
  $('recipe-name').value = recipe.name;
  $('recipe-action').value = recipe.action;
  $('recipe-size').value = recipe.maxMb ?? '';
  $('recipe-even').checked = recipe.even;
  const channel = clip.channels.find((h) => h.name === recipe.channel);
  if (channel) $('share-webhook').value = channel.index;
  for (const box of $('share-track-list').querySelectorAll('input')) box.checked = recipe.tracks.includes(Number(box.value));
  setLayout(recipe.layout || null);
};

$('recipe-save').onclick = withNote(async () => {
  const action = $('recipe-action').value;
  const name = $('recipe-name').value.trim();
  if (!name) throw new Error('Name your share recipe.');
  const channel = clip.channels.find((h) => h.index === Number($('share-webhook').value));
  if (action.endsWith('discord') && !channel) throw new Error('Choose a Discord channel first.');
  const tracks = selectedTracks();
  if (!$('share-tracks').hidden && !tracks.length) throw new Error('Tick at least one audio track.');
  const recipe = { name, action, channel: channel?.name, tracks, even: $('recipe-even').checked,
    maxMb: $('recipe-size').value ? Number($('recipe-size').value) : null,
    layout: action.startsWith('vertical') ? activeLayout : null };
  await api.invoke('save-share-recipe', recipe);
  recipes = [...recipes.filter((r) => r.name !== name), recipe];
  renderRecipes(name);
  note('Saved ' + name + '.');
});

$('recipe-run').onclick = () => runShare('recipe', 'Running share recipe…', 'Recipe finished.', $('recipe-list').value);
$('recipe-delete').onclick = withNote(async () => {
  const name = $('recipe-list').value;
  if (!name) return;
  await api.invoke('delete-share-recipe', name);
  recipes = recipes.filter((r) => r.name !== name);
  renderRecipes();
  note('Removed ' + name + '.');
});

function setLayout(layout) {
  activeLayout = layout;
  $('layout-crop').value = Math.round((layout?.crop ?? 0.5) * 100);
  $('layout-pin').disabled = (layout?.pins.length ?? 0) >= 2;
  drawLayout();
}

function drawLayout() {
  if (layoutPreview.hidden || video.readyState < 2) return;
  const ctx = layoutPreview.getContext('2d');
  const { width, height } = layoutPreview;
  const kept = Math.min(video.videoWidth, video.videoHeight * width / height);
  ctx.drawImage(video, (video.videoWidth - kept) * (activeLayout?.crop ?? 0.5), 0, kept, video.videoHeight, 0, 0, width, height);
  activeLayout?.pins.forEach(([x, y, w, h], i) => {
    const sourceW = w * video.videoWidth;
    const sourceH = h * video.videoHeight;
    const pinW = width * 0.42;
    const pinH = Math.min(height / 4, pinW * sourceH / sourceW);
    ctx.drawImage(video, x * video.videoWidth, y * video.videoHeight, sourceW, sourceH, width * (i ? 0.54 : 0.04), height * 0.04, pinW, pinH);
  });
}

for (const event of ['loadeddata', 'seeked', 'timeupdate']) video.addEventListener(event, drawLayout);
$('layout-crop').oninput = () => setLayout({ crop: Number($('layout-crop').value) / 100, pins: activeLayout?.pins ?? [] });
$('layout-reset').onclick = () => {
  pinSelection.hidden = true;
  setLayout({ crop: 0.5, pins: [] });
};
$('layout-save').onclick = withNote(async () => {
  await api.invoke('save-game-layout', { game: clip.game, layout: activeLayout ?? { crop: 0.5, pins: [] } });
  note('Saved vertical layout for ' + clip.game + '.');
});

$('layout-pin').onclick = () => {
  beginRegion('layout');
};

function beginRegion(purpose) {
  if (!video.videoWidth) return note('Wait for the video preview before selecting a region.', true);
  pinPurpose = purpose;
  video.pause();
  pinSelection.hidden = false;
  pinSelection.width = pinSelection.clientWidth;
  pinSelection.height = pinSelection.clientHeight;
  note(purpose === 'layout' ? 'Drag a box around the minimap, kill feed, or webcam.' : 'Drag a box around what you want to blur.');
}

function videoBounds() {
  const scale = Math.min(pinSelection.width / video.videoWidth, pinSelection.height / video.videoHeight);
  const w = video.videoWidth * scale;
  const h = video.videoHeight * scale;
  return { x: (pinSelection.width - w) / 2, y: (pinSelection.height - h) / 2, w, h };
}

function pinPoint(event) {
  const box = pinSelection.getBoundingClientRect();
  const frame = videoBounds();
  const clamp = (x) => Math.min(1, Math.max(0, x));
  return [clamp((event.clientX - box.left - frame.x) / frame.w), clamp((event.clientY - box.top - frame.y) / frame.h)];
}

function pinRect(event) {
  const [x, y] = pinPoint(event);
  return [Math.min(x, pinStart[0]), Math.min(y, pinStart[1]), Math.abs(x - pinStart[0]), Math.abs(y - pinStart[1])];
}

pinSelection.onpointerdown = (event) => {
  if (event.button !== 0) return;
  pinStart = pinPoint(event);
  pinSelection.setPointerCapture(event.pointerId);
};
pinSelection.onpointermove = (event) => {
  if (!pinStart) return;
  const frame = videoBounds();
  const [x, y, w, h] = pinRect(event);
  const ctx = pinSelection.getContext('2d');
  ctx.clearRect(0, 0, pinSelection.width, pinSelection.height);
  ctx.strokeStyle = '#9f8bff';
  ctx.lineWidth = 3;
  ctx.strokeRect(frame.x + x * frame.w, frame.y + y * frame.h, w * frame.w, h * frame.h);
};
pinSelection.onpointerup = (event) => {
  if (!pinStart) return;
  const rect = pinRect(event);
  pinStart = null;
  if (rect[2] < 0.01 || rect[3] < 0.01) return note('Drag a larger box around the region.', true);
  pinSelection.hidden = true;
  if (pinPurpose === 'redaction') return setBlurRegion(rect);
  setLayout({ crop: activeLayout?.crop ?? 0.5, pins: [...(activeLayout?.pins ?? []), rect] });
  note('Region pinned in the vertical preview.');
};
pinSelection.onpointercancel = () => { pinStart = null; pinSelection.hidden = true; };

function showTranscript(lines) {
  savedTranscript = lines;
  $('speech-lines').replaceChildren(...lines.map((line) => {
    const button = document.createElement('button');
    button.className = 'small';
    button.textContent = clock(line.start) + ' · ' + line.text;
    button.onclick = () => seek(line.start);
    return button;
  }));
}

$('speech-run').onclick = withNote(async () => {
  const source = clip;
  const tracks = selectedTracks();
  if (!$('share-tracks').hidden && !tracks.length) throw new Error('Tick at least one audio track.');
  $('speech-run').disabled = true;
  note('Transcribing on this PC…');
  try {
    const lines = await api.invoke('transcribe-clip', { file: source.file, tracks });
    if (clip === source) {
      showTranscript(lines);
      note(lines.length ? 'Transcript saved. Search it in View all.' : 'No speech found in the selected tracks.');
    }
  } finally {
    $('speech-run').disabled = false;
  }
});

function updateAngleButtons() {
  $('angle-sync').disabled = $('angle-switch').disabled = $('angle-play').disabled = !angle.getAttribute('src');
}

function updateAngle(force = false) {
  if (!showingAngle || !angle.readyState) return;
  const time = video.currentTime + (Number($('angle-offset').value) || 0);
  const available = time >= 0 && time < angle.duration;
  angle.hidden = !available;
  video.muted = available || originalMute;
  if (!available) { angle.pause(); return; }
  if (force || Math.abs(angle.currentTime - time) > 0.2) angle.currentTime = time;
  if (video.paused) angle.pause();
  else if (angle.paused) angle.play().catch(() => undefined);
}

function showAngle(show) {
  if (show && !showingAngle) originalMute = video.muted;
  if (!show && showingAngle) video.muted = originalMute;
  showingAngle = show;
  angle.hidden = !show;
  if (show) updateAngle(true);
  else angle.pause();
  $('angle-switch').textContent = show ? 'Show angle 1' : 'Show angle 2';
}

for (const event of ['timeupdate', 'seeked', 'play', 'pause']) video.addEventListener(event, () => updateAngle(event === 'seeked'));
angle.addEventListener('loadedmetadata', () => updateAngle(true));
$('angle-offset').oninput = () => updateAngle(true);
$('angle-switch').onclick = () => showAngle(!showingAngle);
$('angle-play').onclick = () => { if (video.paused) video.play().catch((error) => note(error.message, true)); else video.pause(); };
$('angle-pick').onclick = withNote(async () => {
  const file = await api.invoke('pick-angle');
  if (!file) return;
  showAngle(false);
  angle.src = api.videoUrl(file);
  angle.volume = video.volume;
  $('angle-offset').value = '0';
  updateAngleButtons();
  note('Second angle loaded. Match audio, or set the offset by hand. Exports use angle 1.');
});
$('angle-sync').onclick = withNote(async () => {
  const source = clip;
  $('angle-sync').disabled = true;
  note('Matching the two audio tracks…');
  try {
    const match = await api.invoke('sync-angle', source.file);
    if (clip === source) {
      $('angle-offset').value = match.offset.toFixed(2);
      updateAngle(true);
      note('Matched audio. Angle 2 offset: ' + match.offset.toFixed(2) + ' s. Check the result by switching angles.');
    }
  } finally {
    updateAngleButtons();
  }
});
