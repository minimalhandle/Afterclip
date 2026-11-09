const api = window.afterclip;
const $ = (id) => document.getElementById(id);
let state;
let recipeNames = '';
let initialized = false;

function note(text, warning = false) {
  $('studio-note').textContent = text;
  $('studio-note').classList.toggle('warn', warning);
}

function action(task) {
  return async (event) => {
    const button = event?.currentTarget;
    if (button) button.disabled = true;
    try { await task(); }
    catch (error) { note(error.message, true); }
    finally { if (button) button.disabled = false; }
  };
}

function fileName(path) {
  return path.split(/[\\/]/).at(-1);
}

async function refreshStudio() {
  state = await api.invoke('studio-state');
  const { prefs, jobs, selection } = state;
  if (!initialized) {
    $('recovery-seconds').value = prefs.recoverySeconds;
    $('recovery-size').value = prefs.recoveryMb || 500;
    initialized = true;
  }
  $('batch-selection').textContent = selection.length
    ? selection.length + ' clips selected in the library.'
    : 'Select clips in View all, then click Batch export.';
  const names = JSON.stringify(state.recipes);
  if (recipeNames !== names) {
    recipeNames = names;
    const previous = $('batch-recipe').value;
    $('batch-recipe').replaceChildren(...state.recipes.map((name) => new Option(name, name)));
    if (state.recipes.includes(previous)) $('batch-recipe').value = previous;
  }
  $('batch-add').disabled = !selection.length || !state.recipes.length;
  $('batch-game').checked = state.pauseInGame;
  $('batch-pause').textContent = state.paused ? 'Resume queue' : 'Pause queue';
  $('batch-jobs').replaceChildren(...jobs.map((job) => {
    const line = document.createElement('div');
    line.className = 'row studio-job';
    const label = document.createElement('span');
    label.textContent = fileName(job.file) + ' · ' + job.recipe + ' · ' + job.status;
    line.append(label);
    if (job.status === 'failed') {
      label.title = job.detail;
      label.textContent += ' · ' + job.detail;
      const retry = document.createElement('button');
      retry.className = 'small';
      retry.textContent = 'Retry';
      retry.onclick = action(() => api.invoke('control-batch', [state.paused, state.pauseInGame, job.id]));
      line.append(retry);
    }
    return line;
  }));
  $('recovery-restore').disabled = !state.recovery;
  const checkpoint = state.previousReplay ? 'Previous session checkpoint ' : 'Last checkpoint ';
  $('recovery-latest').textContent = state.recovery ? checkpoint + new Date(state.recovery).toLocaleString() : 'No recovery checkpoint yet.';
  $('watch-path').textContent = prefs.watchFolder || 'No watched folder.';
  $('watch-stop').disabled = !prefs.watchFolder;
}

$('batch-add').onclick = action(async () => {
  await api.invoke('enqueue-batch', { files: state.selection, recipe: $('batch-recipe').value });
  note('Clips queued. A pause takes effect after the active clip finishes.');
  await refreshStudio();
});
$('batch-pause').onclick = action(async () => {
  await api.invoke('control-batch', [!state.paused, state.pauseInGame, null]);
  await refreshStudio();
});
$('batch-game').onchange = action(() => api.invoke('control-batch', [state.paused, $('batch-game').checked, null]));
$('recovery-save').onclick = action(async () => {
  await api.invoke('configure-recovery', [Number($('recovery-seconds').value), Number($('recovery-size').value) || 500]);
  note('Replay recovery settings saved.');
});
$('recovery-restore').onclick = action(async () => {
  const path = await api.invoke('restore-replay');
  note('Restored ' + fileName(path) + ' to Clips.');
});
$('recovery-now').onclick = action(async () => {
  await api.invoke('checkpoint-now');
  note('Replay checkpoint saved.');
  await refreshStudio();
});
$('import-clips').onclick = action(async () => {
  const result = await api.invoke('import-clips');
  if (result.count || result.errors.length) note('Imported ' + result.count + ' clips. ' + result.errors.join(' '), result.errors.length > 0);
});
$('watch-folder').onclick = action(async () => { await api.invoke('set-watch-folder', [true, null]); await refreshStudio(); });
$('watch-stop').onclick = action(async () => { await api.invoke('set-watch-folder', [false, null]); await refreshStudio(); });

const roles = ['audio-game', 'audio-mic', 'audio-chat'];
$('audio-load').onclick = action(async () => {
  const setup = await api.invoke('audio-setup');
  const channels = setup.mixer ? [...setup.mixer.inputs, ...setup.mixer.outputs] : [];
  $('mixer-labels').textContent = channels.length
    ? channels.map(({ channel, name }) => name ? channel + ' · ' + name : channel).join('  /  ')
    : setup.mixerError;
  for (const role of roles) {
    const choices = [new Option('Keep current setup', '')];
    if (role === 'audio-game') choices.push(new Option('Automatic game audio', '-1'));
    for (const source of setup.sources) choices.push(new Option(source.label + (source.ownTrack ? ' · Separate' : ''), source.index));
    $(role).replaceChildren(...choices);
  }
  $('audio-roles').hidden = false;
});
$('audio-apply').onclick = action(async () => {
  await api.invoke('apply-audio-roles', roles.map((role) => $(role).value === '' ? null : Number($(role).value)));
  note('Audio roles saved. Capture is restarting if the track layout changed.');
});
$('audio-test').onclick = action(async () => {
  note('Play game and chat audio and speak into your mic now.');
  const tracks = await api.invoke('test-audio-tracks');
  $('audio-results').replaceChildren(...tracks.map((track) => {
    const line = document.createElement('p');
    line.textContent = 'Track ' + track.track + ' · ' + (track.error || (track.silent ? 'No signal' : track.db + ' dB peak'))
      + (track.duplicate ? ' · Matches track ' + track.duplicate : '');
    return line;
  }));
  note(tracks.length ? 'Audio test complete.' : 'No audio tracks in the capture.', !tracks.length);
});

refreshStudio().catch((error) => note(error.message, true));
setInterval(() => { if (!document.hidden) refreshStudio().catch((error) => note(error.message, true)); }, 1000);
api.ready();
