// End-to-end check of the full app on a colour card, so nothing from the screen or microphone is captured.
// Drives it through the local API, a real global hotkey, and the Stream Deck plugin talking to a fake Stream Deck.
//   node src/test/e2e.js
const { spawn, spawnSync, execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { WebSocketServer } = require('ws');
const { API_BASE, root, ffmpeg, sleep, check, finish, waitFor, powershell, startApp } = require('./harness');

const HOTKEY = 'Ctrl+Alt+Shift+F9'; // Unlikely to be bound, and doesn't switch keyboard layout.

function pressHotkey() {
  // Ctrl+Alt+Shift+F9 through keybd_event, which reaches RegisterHotKey like a physical press.
  const script = `Add-Type -Name K -Namespace N -MemberDefinition '[DllImport("user32.dll")] public static extern void keybd_event(byte v, byte s, uint f, System.UIntPtr e);';
$k = 0x11,0x12,0x10,0x78; foreach ($v in $k) { [N.K]::keybd_event($v,0,0,[UIntPtr]::Zero) }; [array]::Reverse($k); foreach ($v in $k) { [N.K]::keybd_event($v,0,2,[UIntPtr]::Zero) }`;
  execFileSync('powershell', ['-NoProfile', '-Command', script], { stdio: 'ignore' });
}

async function main() {
  const session = await startApp({
    bufferSeconds: 25, width: 1280, height: 720, fps: 30, bitrateKbps: 6000, testPattern: true, audioSources: [], micHotkey: '', markerHotkey: '',
    presets: [{ seconds: 5, hotkey: HOTKEY }, { seconds: 7, hotkey: 'Alt+Shift+F10' }, { seconds: 8, hotkey: 'Ctrl+Alt+Numpad7' }],
    recordHotkey: 'Ctrl+Alt+Shift+F8',
  });
  const { api, output, tokenFile, token } = session;
  const clips = () => {
    const dir = path.join(output, 'Clips');
    return fs.existsSync(dir) ? fs.readdirSync(dir) : [];
  };
  let pluginProc;
  let wss;
  let enginePid;
  try {
    check('app starts buffering', true);
    const first = await api.status();
    check('all hotkey formats register', first.hotkeyFailures.length === 0, first.hotkeyFailures.join(', '));

    const ps = `$p = Get-CimInstance Win32_Process -Filter "Name='afterclip-engine.exe' AND ParentProcessId=${session.app.pid}" | Select-Object -First 1;
Add-Type -Name W -Namespace N -MemberDefinition '[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, System.IntPtr l); public delegate bool EnumProc(System.IntPtr h, System.IntPtr l); [DllImport("user32.dll")] public static extern bool IsWindowVisible(System.IntPtr h); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr h, out uint p);';
$script:visible = 0; [N.W]::EnumWindows({ param($h,$l) $o = [uint32]0; [void][N.W]::GetWindowThreadProcessId($h, [ref]$o); if ($o -eq $p.ProcessId -and [N.W]::IsWindowVisible($h)) { $script:visible++ }; $true }, [IntPtr]::Zero) | Out-Null;
@{ id = $p.ProcessId; path = $p.ExecutablePath; ports = @(Get-NetTCPConnection -OwningProcess $p.ProcessId -State Listen -ErrorAction SilentlyContinue).Count; visible = $script:visible; ownObs = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($p.ParentProcessId) AND Name='obs64.exe'").Count } | ConvertTo-Json -Compress`;
    const info = JSON.parse(powershell(ps));
    enginePid = info.id;
    check('runs the bundled engine', !!info.path && /afterclip-engine\.exe$/i.test(info.path) && !/program files\\obs/i.test(info.path), info.path);
    check('launches no OBS app', info.ownObs === 0);
    check('engine listens on no network ports', info.ports === 0, `${info.ports}`);
    check('engine shows no windows', info.visible === 0, `${info.visible}`);

    const noToken = await fetch(`${API_BASE}/clip/5`, { method: 'POST' });
    check('API refuses a missing token', noToken.status === 401);
    const web = await fetch(`${API_BASE}/status`, { headers: { Origin: 'https://example.com' } });
    check('API refuses a web page', web.status === 401 && !web.headers.get('access-control-allow-origin'));

    console.log('  filling buffer for 30s');
    await sleep(30000);
    for (const want of [5, 10]) {
      const r = await api.clip(want);
      check(`clip ${want}s length`, r.actualSeconds >= want - 0.5 && r.actualSeconds <= want + 1.5, `${r.actualSeconds.toFixed(2)}s`);
      const decode = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', r.file, '-f', 'null', '-'], { encoding: 'utf8' });
      check(`clip ${want}s decodes cleanly`, decode.status === 0 && !decode.stderr.trim(), decode.stderr.trim().slice(0, 200));
    }
    const full = await api.clip(0);
    check('full-buffer clip', full.actualSeconds >= 20 && full.actualSeconds <= 28, `${full.actualSeconds.toFixed(2)}s`);
    const over = await api.clip(600);
    check('clip longer than buffer is capped', over.actualSeconds <= 28, `${over.actualSeconds.toFixed(2)}s`);
    const rapid = await Promise.all([api.clip(5), api.clip(5)]);
    check('rapid presses give separate files', rapid[0].file !== rapid[1].file && rapid.every((r) => fs.existsSync(r.file)));

    const beforeKey = clips().length;
    pressHotkey();
    await waitFor(() => clips().length > beforeKey, 15000, 'hotkey clip');
    check('global hotkey clip', true);

    await api.toggleRecord();
    await sleep(3000);
    const marked = await (await api.call('/marker', 'POST')).json();
    check('marker added while recording', marked.marker === 1, JSON.stringify(marked));
    await sleep(2000);
    await (await api.call('/marker', 'POST')).json();
    await sleep(3000);
    const during = await api.status();
    check('buffer keeps running during recording', during.buffering && during.recording);
    const clipWhileRecording = await api.clip(5);
    check('can clip during recording', fs.existsSync(clipWhileRecording.file), `${clipWhileRecording.actualSeconds.toFixed(2)}s`);
    await api.toggleRecord();
    const recDir = path.join(output, 'Recordings');
    const rec = await waitFor(() => !fs.existsSync(recDir) ? null : fs.readdirSync(recDir)[0], 15000, 'recording file');
    await waitFor(async () => !(await api.status()).recording, 15000, 'recording to stop');
    const recInfo = spawnSync(ffmpeg, ['-hide_banner', '-i', path.join(recDir, rec)], { encoding: 'utf8' }).stderr;
    const m = /Duration: (\d+):(\d+):(\d+\.\d+)/.exec(recInfo);
    const recSeconds = m ? Number(m[1]) * 3600 + Number(m[2]) * 60 + Number(m[3]) : 0;
    check('recording length', recSeconds >= 6 && recSeconds <= 20, `${recSeconds.toFixed(2)}s`);
    check('markers become chapters in the file', /Chapter #0:0/.test(recInfo) && /Chapter #0:1/.test(recInfo) && /Marker 2/.test(recInfo));
    const staging = fs.readdirSync(path.join(output, '.staging'));
    check('staging folder empty', staging.length === 0, staging.join(', '));

    // Stream Deck side: a fake host speaks the SDK protocol to the real plugin process.
    wss = new WebSocketServer({ port: 0, host: '127.0.0.1' });
    await new Promise((resolve) => wss.once('listening', resolve));
    const received = [];
    const connected = new Promise((resolve) => wss.on('connection', (ws) => {
      ws.on('message', (raw) => received.push(JSON.parse(raw)));
      resolve(ws);
    }));
    pluginProc = spawn(process.execPath, ['main.cjs', '-port', wss.address().port, '-pluginUUID', 'test-uuid', '-registerEvent', 'registerPlugin', '-info', '{}'], {
      cwd: path.join(root, 'streamdeck', 'com.afterclip.clips.sdPlugin'), stdio: 'inherit',
      env: { ...process.env, AFTERCLIP_TOKEN_FILE: tokenFile, AFTERCLIP_API: API_BASE },
    });
    const sdws = await connected;
    await waitFor(() => received.some((x) => x.event === 'registerPlugin'), 5000, 'plugin register');
    check('plugin registers', received.find((x) => x.event === 'registerPlugin').uuid === 'test-uuid');
    const event = (name, action, context, settings = {}) => sdws.send(JSON.stringify({ event: name, action, context, payload: { settings } }));
    event('willAppear', 'com.afterclip.clips.clip', 'k1', { seconds: 7 });
    event('willAppear', 'com.afterclip.clips.record', 'k2');
    await waitFor(() => received.filter((x) => x.event === 'setImage').length >= 2, 5000, 'key images');
    check('plugin draws keys', decodeURIComponent(received.find((x) => x.context === 'k1' && x.event === 'setImage').payload.image).includes('>7<'));
    // A key confirms by drawing, starting from a picture marked for this.
    const shown = (context) => received.some((x) => x.context === context && x.event === 'setImage' && /data-mark="(confirmed|pulsed)"/.test(decodeURIComponent(x.payload.image)));
    const beforeDeck = clips().length;
    event('keyDown', 'com.afterclip.clips.clip', 'k1', { seconds: 7 });
    await waitFor(() => shown('k1'), 15000, 'clip key ok');
    check('Stream Deck clip key', clips().length === beforeDeck + 1);
    event('keyDown', 'com.afterclip.clips.record', 'k2');
    await waitFor(() => shown('k2'), 15000, 'record key ok');
    await waitFor(async () => (await api.status()).recording, 8000, 'recording on');
    await waitFor(() => received.filter((x) => x.context === 'k2' && x.event === 'setImage').some((x) => decodeURIComponent(x.payload.image).includes('STOP')), 6000, 'STOP image');
    check('Stream Deck record key shows live state', true);
    event('keyDown', 'com.afterclip.clips.record', 'k2');
    await waitFor(async () => !(await api.status()).recording, 15000, 'recording off');
    check('Stream Deck record key stops', true);
  } catch (e) {
    check('no exceptions', false, e.message);
  } finally {
    pluginProc?.kill();
    wss?.close();
    await session.stop();
    const left = powershell(`@(Get-Process -Id ${enginePid ?? 0} -ErrorAction SilentlyContinue).Count`);
    check('engine exits when the app is killed', left === '0', `${left} left`);
  }
  finish();
}

main();
