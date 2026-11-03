// Runs the real Stream Deck plugin against a fake Stream Deck and a fake Afterclip, so it needs neither
// running and can't touch the desktop.
//   node src/test/plugin-test.js
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { WebSocketServer } = require('ws');

const root = path.resolve(__dirname, '..', '..');
const pluginDir = path.join(root, 'streamdeck', 'com.afterclip.clips.sdPlugin');
const failures = [];
const check = (name, ok, detail = '') => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name} ${detail}`);
  if (!ok) failures.push(name);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, ms, label) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (fn()) return;
    await sleep(25);
  }
  throw new Error(`Timed out waiting for ${label}`);
}

const ACTION = (name) => `com.afterclip.clips.${name}`;
const svgOf = (message) => decodeURIComponent(message.payload.image);

async function main() {
  const manifest = JSON.parse(fs.readFileSync(path.join(pluginDir, 'manifest.json'), 'utf8'));
  const names = manifest.Actions.map((a) => a.Name);
  for (const wanted of ['Save Last 15 Sec', 'Save Last 30 Sec', 'Save Last 1 Min', 'Save Last 2 Min', 'Save Last 3 Min', 'Save Last 5 Min', 'Save Last 10 Min', 'Save Last 15 Min', 'Save Last 20 Min', 'Save Last 30 Min', 'Save Full Buffer', 'Record', 'Mute Mic In Clips', 'Clean Mic', 'Add Marker', 'Save Custom Length', 'Open Afterclip', 'Open Clips Folder', 'Edit Last Clip', 'Copy Last Clip', 'Take Screenshot', 'Compress Last Clip', 'Send Last Clip to Discord', 'Mic Volume', 'Clip Length Dial']) check(`has ${wanted} action`, names.includes(wanted));
  const dials = manifest.Actions.filter((a) => a.Controllers.includes('Encoder'));
  check('the dials are Stream Deck + encoders with a touch strip', dials.length === 2 && dials.every((a) => a.Encoder?.layout === '$B1' && !a.Controllers.includes('Keypad')));
  const missing = manifest.Actions.flatMap((a) => [a.Icon, a.States[0].Image])
    .filter((image) => !['.svg', '.png'].some((ext) => fs.existsSync(path.join(pluginDir, image + ext))));
  check('every icon file exists', missing.length === 0, missing.join(', '));

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'afterclip-plugin-'));
  const tokenFile = path.join(dir, 'token.txt');
  fs.writeFileSync(tokenFile, 'secret-token');
  const state = { buffering: true, recording: false, micMuted: false, cleanMic: false, micVolume: 100, bufferingSince: Date.now() - 30000, bufferSeconds: 120, clipDelay: 0, fail: false, noClips: false, noWebhook: false };
  const routes = [];
  const api = http.createServer(async (req, res) => {
    const send = (code, body) => { res.writeHead(code, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(body)); };
    if (req.headers.authorization !== 'Bearer secret-token') return send(401, { error: 'unauthorized' });
    if (req.url === '/status') return send(200, state);
    routes.push(`${req.method} ${req.url}`);
    if (state.fail) return send(500, { error: 'Replay buffer is not running' });
    if (state.noClips && req.url.startsWith('/last-clip/')) return send(500, { error: 'No clips yet' });
    if (req.url === '/mic/clean') { state.cleanMic = !state.cleanMic; return send(200, { cleanMic: state.cleanMic }); }
    if (req.url === '/marker') return state.recording ? send(200, { marker: 1 }) : send(500, { error: 'Start a recording' });
    if (req.url === '/mic/toggle') { state.micMuted = !state.micMuted; return send(200, { micMuted: state.micMuted }); }
    if (req.url === '/record/toggle') { state.recording = !state.recording; return send(200, { recording: state.recording }); }
    if (req.url.startsWith('/mic/volume/')) {
      state.micVolume = Math.min(200, Math.max(0, state.micVolume + Number(req.url.split('/').pop())));
      return send(200, { micVolume: state.micVolume });
    }
    if (req.url === '/last-clip/discord' && state.noWebhook) return send(500, { error: 'Add your Discord webhook in Settings, under Clips and sharing.' });
    await sleep(state.clipDelay);
    send(200, { actualSeconds: 1 });
  });
  await new Promise((r) => api.listen(0, '127.0.0.1', r));
  const wss = new WebSocketServer({ port: 0, host: '127.0.0.1' });
  await new Promise((r) => wss.once('listening', r));

  const received = [];
  const connected = new Promise((resolve) => wss.on('connection', (ws) => {
    ws.on('message', (raw) => received.push(JSON.parse(raw)));
    resolve(ws);
  }));
  const plugin = spawn(process.execPath, ['main.cjs', '-port', wss.address().port, '-pluginUUID', 'test', '-registerEvent', 'registerPlugin', '-info', '{}'], {
    cwd: pluginDir, stdio: 'inherit',
    env: { ...process.env, AFTERCLIP_TOKEN_FILE: tokenFile, AFTERCLIP_API: `http://127.0.0.1:${api.address().port}` },
  });
  const deck = await connected;
  const event = (name, action, context, settings = {}, extra = {}) => deck.send(JSON.stringify({ event: name, action, context, payload: { settings, ...extra } }));
  const lastFeedback = (context) => received.filter((m) => m.context === context && m.event === 'setFeedback').at(-1)?.payload;
  const lastImage = (context) => received.filter((m) => m.context === context && m.event === 'setImage').at(-1);
  // Counts each confirmation or failure the key has shown, from the first picture of each.
  const results = (context, name) => received.filter((m) => m.context === context && m.event === 'setImage' && (name === 'showOk' ? /data-mark="(confirmed|pulsed)"/ : /data-mark="failed"/).test(svgOf(m))).length;

  try {
    await waitFor(() => received.some((m) => m.event === 'registerPlugin'), 5000, 'register');
    check('registers with Stream Deck', true);

    const layout = [
      ['s15', 'save-15-sec', '/clip/15', '>15<'], ['s30', 'save-30-sec', '/clip/30', '>30<'],
      ['one', 'save-1-min', '/clip/60', '>1<'], ['two', 'save-2-min', '/clip/120', '>2<'], ['m3', 'save-3-min', '/clip/180', '>3<'],
      ['m5', 'save-5-min', '/clip/300', '>5<'], ['m10', 'save-10-min', '/clip/600', '>10<'], ['m15', 'save-15-min', '/clip/900', '>15<'],
      ['m20', 'save-20-min', '/clip/1200', '>20<'], ['m30', 'save-30-min', '/clip/1800', '>30<'], ['full', 'save-full', '/clip/0', '>FULL<'],
    ];
    for (const [context, action] of layout) event('willAppear', ACTION(action), context);
    event('willAppear', ACTION('record'), 'rec');
    event('willAppear', ACTION('clip'), 'custom', { seconds: 45 });
    await waitFor(() => ['one', 'two', 'full', 'rec', 'custom'].every((c) => lastImage(c)), 5000, 'key images');
    for (const [context, action, , label] of layout) check(`${action} key shows its label`, svgOf(lastImage(context)).includes(label));
    // A key shows OFF until its first status answer, which takes a moment with this many keys.
    await waitFor(() => svgOf(lastImage('one')).includes('#7c6cff'), 4000, 'live key colour');
    check('keys use the purple theme', svgOf(lastImage('one')).includes('#120f2e') && !svgOf(lastImage('one')).includes('#ff9a3d'));
    check('keys draw the logo ring around the number', /stroke="#7c6cff"/.test(svgOf(lastImage('one'))) && svgOf(lastImage('one')).includes('>1<'));
    check('custom key shows its length', svgOf(lastImage('custom')).includes('>45<'));
    await waitFor(() => svgOf(lastImage('rec')).includes('>REC<'), 4000, 'record idle');
    check('record key shows ready to start', true);

    for (const [context, action, route] of layout) {
      routes.length = 0;
      event('keyDown', ACTION(action), context);
      await waitFor(() => results(context, 'showOk') === 1, 3000, `${action} ok`);
      check(`${action} key saves the right length`, routes.join() === `POST ${route}`, routes.join());
    }
    routes.length = 0;
    event('keyDown', ACTION('clip'), 'custom', { seconds: 45 });
    await waitFor(() => results('custom', 'showOk') === 1, 3000, 'custom ok');
    check('custom key saves its length', routes.join() === 'POST /clip/45', routes.join());
    await waitFor(() => svgOf(lastImage('custom')).includes('>45<'), 3000, 'key back to its length');
    const savedFrames = received.filter((m) => m.context === 'custom' && m.event === 'setImage').map(svgOf).filter((svg) => svg.includes('>SAVED<'));
    check('a save plays the animation in purple, ending on SAVED', savedFrames.length === 9 && savedFrames.every((svg) => svg.includes('#3a2bb8')));
    check('the animation avoids effects Stream Deck might not draw', !/filter|clip-path|<animate/.test(savedFrames.join()));
    check('the green tick is gone', !received.some((m) => m.event === 'showOk' || m.event === 'showAlert'));

    state.clipDelay = 600;
    routes.length = 0;
    event('keyDown', ACTION('save-1-min'), 'one');
    await waitFor(() => svgOf(lastImage('one')).includes('SAVING'), 500, 'saving face');
    check('key shows it is saving', true);
    event('keyDown', ACTION('save-1-min'), 'one');
    await waitFor(() => results('one', 'showOk') === 2, 3000, 'second ok');
    check('a double press saves once', routes.length === 1, routes.join());
    await waitFor(() => !svgOf(lastImage('one')).includes('SAVING'), 3000, 'face reset');
    state.clipDelay = 0;

    event('keyDown', ACTION('record'), 'rec');
    await waitFor(() => results('rec', 'showOk') === 1, 3000, 'record ok');
    await waitFor(() => svgOf(lastImage('rec')).includes('STOP'), 3000, 'STOP face');
    check('record key shows it is recording', svgOf(lastImage('rec')).includes('#3a2bb8'));
    event('keyDown', ACTION('record'), 'rec');
    await waitFor(() => svgOf(lastImage('rec')).includes('>REC<'), 3000, 'REC face');
    check('record key stops', !state.recording);

    event('willAppear', ACTION('mic'), 'mic');
    await waitFor(() => lastImage('mic') && svgOf(lastImage('mic')).includes('MIC ON'), 4000, 'mic on face');
    check('mic key shows the mic is on', true);
    routes.length = 0;
    event('keyDown', ACTION('mic'), 'mic');
    await waitFor(() => results('mic', 'showOk') === 1, 3000, 'mic ok');
    check('mic key calls the mute route', routes.join() === 'POST /mic/toggle', routes.join());
    await waitFor(() => svgOf(lastImage('mic')).includes('MUTED'), 3000, 'muted face');
    check('mic key shows it is muted', state.micMuted);
    event('keyDown', ACTION('mic'), 'mic');
    await waitFor(() => svgOf(lastImage('mic')).includes('MIC ON'), 3000, 'mic back on');
    check('mic key unmutes', !state.micMuted);

    event('willAppear', ACTION('clean-mic'), 'clean');
    await waitFor(() => lastImage('clean') && svgOf(lastImage('clean')).includes('CLEAN MIC'), 4000, 'clean face');
    event('keyDown', ACTION('clean-mic'), 'clean');
    await waitFor(() => results('clean', 'showOk') === 1 && svgOf(lastImage('clean')).includes('#3a2bb8'), 3000, 'clean on');
    check('Clean Mic key turns on and shows it', state.cleanMic);
    event('keyDown', ACTION('clean-mic'), 'clean');
    await waitFor(() => !svgOf(lastImage('clean')).includes('#3a2bb8'), 3000, 'clean off');
    check('Clean Mic key turns off', !state.cleanMic);

    event('willAppear', ACTION('marker'), 'mark');
    await waitFor(() => lastImage('mark') && svgOf(lastImage('mark')).includes('#9a95bd'), 4000, 'marker dim');
    check('marker key is dim without a recording', true);
    event('keyDown', ACTION('marker'), 'mark');
    await waitFor(() => results('mark', 'showAlert') === 1, 3000, 'marker alert');
    check('marker key says there is no recording', svgOf(lastImage('mark')).includes('>NO REC<'));
    state.recording = true;
    await waitFor(() => !svgOf(lastImage('mark')).includes('#9a95bd'), 4000, 'marker live');
    event('keyDown', ACTION('marker'), 'mark');
    await waitFor(() => results('mark', 'showOk') === 1, 3000, 'marker ok');
    await waitFor(() => svgOf(lastImage('mark')).includes('>MARKED<'), 2000, 'marked');
    check('marker key works while recording', true);
    state.recording = false;

    for (const [context, action, route, label] of [['open', 'open-panel', '/open/panel', '>OPEN<'], ['folder', 'open-clips', '/open/clips', '>CLIPS<'], ['edit', 'edit-last', '/last-clip/edit', '>EDIT LAST<'], ['copy', 'copy-last', '/last-clip/copy', '>COPY LAST<'], ['shot', 'screenshot', '/screenshot', '>SHOT<']]) {
      event('willAppear', ACTION(action), context);
      await waitFor(() => lastImage(context) && svgOf(lastImage(context)).includes(label), 4000, `${action} face`);
      routes.length = 0;
      event('keyDown', ACTION(action), context);
      await waitFor(() => results(context, 'showOk') === 1, 3000, `${action} ok`);
      check(`${action} key calls its route`, routes.join() === `POST ${route}`, routes.join());
    }
    await waitFor(() => svgOf(lastImage('copy')).includes('>COPIED<'), 2000, 'copied');
    check('copy key confirms', true);
    await waitFor(() => svgOf(lastImage('copy')).includes('>COPY LAST<'), 3000, 'copy back');
    state.noClips = true;
    event('keyDown', ACTION('copy-last'), 'copy');
    await waitFor(() => results('copy', 'showAlert') === 1, 3000, 'no clips');
    check('copy key says when there are no clips', svgOf(lastImage('copy')).includes('>NO CLIPS<'));
    state.noClips = false;

    // Jobs on the last clip: the key shows it's working until the job is done, and a second press waits.
    state.clipDelay = 600;
    for (const [context, action, route, busy, done] of [['shrink', 'compress-last', '/last-clip/compress', 'WORKING', 'READY'], ['send', 'discord-last', '/last-clip/discord', 'SENDING', 'SENT']]) {
      event('willAppear', ACTION(action), context);
      await waitFor(() => lastImage(context) && !svgOf(lastImage(context)).includes('>OFF<'), 4000, `${action} face`);
      routes.length = 0;
      event('keyDown', ACTION(action), context);
      await waitFor(() => svgOf(lastImage(context)).includes(`>${busy}<`), 500, `${action} busy`);
      event('keyDown', ACTION(action), context);
      await waitFor(() => results(context, 'showOk') === 1, 3000, `${action} ok`);
      check(`${action} key shows it's working, and runs once`, routes.join() === `POST ${route}`, routes.join());
      await waitFor(() => svgOf(lastImage(context)).includes(`>${done}<`), 2000, `${action} done`);
    }
    state.clipDelay = 0;
    state.noWebhook = true;
    event('keyDown', ACTION('discord-last'), 'send');
    await waitFor(() => results('send', 'showAlert') === 1, 3000, 'no webhook');
    check('Discord key says when there is no webhook', svgOf(lastImage('send')).includes('>NO HOOK<'));
    state.noWebhook = false;

    // Stream Deck + dials draw on the touch strip.
    event('willAppear', ACTION('mic-dial'), 'knob');
    await waitFor(() => lastFeedback('knob')?.value === '100%', 4000, 'mic dial face');
    check('mic dial shows the volume, with a purple bar', lastFeedback('knob').indicator.value === 50 && lastFeedback('knob').indicator.bar_fill_c === '#7c6cff');
    routes.length = 0;
    event('dialRotate', ACTION('mic-dial'), 'knob', {}, { ticks: -3 });
    await waitFor(() => lastFeedback('knob')?.value === '85%', 3000, 'turned down');
    check('turning the mic dial changes the volume by 5% a tick', routes.join() === 'POST /mic/volume/-15', routes.join());
    event('dialDown', ACTION('mic-dial'), 'knob');
    await waitFor(() => lastFeedback('knob')?.value === 'Muted', 3000, 'dial muted');
    check('pressing the mic dial mutes the mic in clips', state.micMuted);
    event('touchTap', ACTION('mic-dial'), 'knob');
    await waitFor(() => lastFeedback('knob')?.value === '85%', 3000, 'dial unmuted');
    check('tapping the strip unmutes it', !state.micMuted);

    event('willAppear', ACTION('clip-dial'), 'lengths', { seconds: 60 });
    await waitFor(() => lastFeedback('lengths')?.value === 'Last 1 min', 4000, 'clip dial face');
    event('dialRotate', ACTION('clip-dial'), 'lengths', {}, { ticks: 2 });
    await waitFor(() => lastFeedback('lengths')?.value === 'Last 5 min', 3000, 'clip dial turned');
    check('turning the clip dial picks a length and keeps it', received.some((m) => m.context === 'lengths' && m.event === 'setSettings' && m.payload.seconds === 300));
    check('the clip dial bar shows how full the buffer is', lastFeedback('lengths').indicator.value >= 24 && lastFeedback('lengths').indicator.value <= 30, String(lastFeedback('lengths').indicator.value));
    routes.length = 0;
    event('dialDown', ACTION('clip-dial'), 'lengths');
    await waitFor(() => lastFeedback('lengths')?.value === 'Saved', 3000, 'dial saved');
    check('pressing the clip dial saves that length', routes.join() === 'POST /clip/300', routes.join());
    await waitFor(() => lastFeedback('lengths')?.value === 'Last 5 min', 3000, 'dial back');

    state.fail = true;

    event('keyDown', ACTION('save-full'), 'full');
    await waitFor(() => results('full', 'showAlert') === 1, 3000, 'alert');
    check('a failed save says so', svgOf(lastImage('full')).includes('>FAILED<'));
    state.fail = false;

    state.buffering = false;
    await waitFor(() => svgOf(lastImage('two')).includes('#9a95bd'), 4000, 'dim');
    check('clip keys dim when the buffer stops', true);
    state.buffering = true;

    api.closeAllConnections();
    await new Promise((r) => api.close(r));
    await waitFor(() => svgOf(lastImage('rec')).includes('>OFF<') && svgOf(lastImage('one')).includes('>OFF<'), 5000, 'off faces');
    check('keys show OFF when Afterclip is closed', true);
    event('keyDown', ACTION('save-1-min'), 'one');
    await waitFor(() => results('one', 'showAlert') === 1, 4000, 'off alert');
    check('pressing a key while Afterclip is closed says so', svgOf(lastImage('one')).includes('>NOT OPEN<'));
    await sleep(1500);
    const beforeKeyCount = received.filter((m) => m.event === 'setImage').length;
    await sleep(2500);
    check('an unchanged key is not redrawn', received.filter((m) => m.event === 'setImage').length === beforeKeyCount);
  } catch (e) {
    check('no exceptions', false, e.message);
  } finally {
    plugin.kill();
    wss.close();
    api.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
  console.log(failures.length ? `\n${failures.length} FAILED: ${failures.join('; ')}` : '\nALL PASSED');
  process.exit(failures.length ? 1 : 0);
}

main();
