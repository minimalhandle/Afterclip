// Drives the real panel and editor through WebView2's debugging port, and reads real pixels off the screen,
// because rendering checks that ask the window to draw itself can pass while the screen shows blank.
// Uses a colour card for video. Desktop audio is captured briefly, only to count tracks, then deleted.
//   node src/test/ui.js [screenshot folder]
const { spawn, spawnSync, execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const WebSocket = require('ws');
const { ffmpeg, sleep, check, finish, waitFor, powershell, startApp } = require('./harness');

const PORT = 9333;
const PANEL_URL = /tauri\.localhost\/(index\.html)?$/;
process.env.AFTERCLIP_DEBUG_PORT = String(PORT);
const shots = process.argv[2];

function windowInfo(pid) {
  const script = `Add-Type -AssemblyName System.Drawing;
Add-Type -Name V -Namespace N -MemberDefinition 'public delegate bool EP(System.IntPtr h, System.IntPtr l); [DllImport("user32.dll")] public static extern bool EnumWindows(EP f, System.IntPtr l); [DllImport("user32.dll")] public static extern bool IsWindowVisible(System.IntPtr h); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr h, out uint p); [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetClassName(System.IntPtr h, System.Text.StringBuilder s, int n); [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetWindowText(System.IntPtr h, System.Text.StringBuilder s, int n); [DllImport("user32.dll")] public static extern bool GetWindowRect(System.IntPtr h, out RECT r); [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); public struct RECT { public int X0,Y0,X1,Y1; }';
[void][N.V]::SetProcessDPIAware(); $found = New-Object Collections.ArrayList;
$cb = [N.V+EP]{ param($h,$l) $o=[uint32]0; [void][N.V]::GetWindowThreadProcessId($h,[ref]$o); if ($o -eq ${pid} -and [N.V]::IsWindowVisible($h)) { $c=New-Object Text.StringBuilder 100; [void][N.V]::GetClassName($h,$c,100); if ("$c" -eq 'Tauri Window') { $t=New-Object Text.StringBuilder 100; [void][N.V]::GetWindowText($h,$t,100); $r=New-Object N.V+RECT; [void][N.V]::GetWindowRect($h,[ref]$r);
  $w=$r.X1-$r.X0; $hh=$r.Y1-$r.Y0; $b=New-Object Drawing.Bitmap($w,$hh); $g=[Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.X0,$r.Y0,0,0,$b.Size); $g.Dispose(); $white=0;$n=0; for($y=30;$y -lt $hh-10;$y+=16){for($x=16;$x -lt $w-16;$x+=16){$p=$b.GetPixel($x,$y); if($p.R -gt 235 -and $p.G -gt 235 -and $p.B -gt 235){$white++}; $n++}}; $b.Dispose();
  [void]$found.Add(@{ title="$t"; x=$r.X0; y=$r.Y0; w=$w; h=$hh; whitePct=[math]::Round(100*$white/$n) }) } }; $true }.GetNewClosure();
[void][N.V]::EnumWindows($cb,[IntPtr]::Zero); ConvertTo-Json -Compress -InputObject @($found)`;
  return JSON.parse(powershell(script) || '[]');
}

class Page {
  static async open(match) {
    let seen = 'nothing listening on the debug port';
    const target = await waitFor(async () => {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
      seen = list.map((t) => `${t.type} ${t.url}`).join(', ') || 'no pages';
      return list.find((t) => t.type === 'page' && match.test(t.url));
    }, 15000, `page ${match}`).catch((e) => { throw new Error(`${e.message}: saw ${seen}`); });
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { ws.once('open', resolve); ws.once('error', reject); });
    return new Page(ws);
  }

  constructor(ws) {
    this.ws = ws;
    this.next = 1;
    this.pending = new Map();
    ws.on('message', (raw) => {
      const m = JSON.parse(raw);
      if (m.id && this.pending.has(m.id)) { this.pending.get(m.id)(m); this.pending.delete(m.id); }
    });
  }

  send(method, params = {}) {
    const id = this.next++;
    return new Promise((resolve, reject) => {
      // A page that dies or hangs mustn't stall the suite with the app still running.
      const timer = setTimeout(() => reject(new Error(`Page stopped answering: ${params.expression?.slice(0, 80) ?? method}`)), 20000);
      this.pending.set(id, (m) => { clearTimeout(timer); resolve(m); });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  async eval(expression) {
    const { result } = await this.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    return result.result.value;
  }

  async shot(name) {
    if (!shots) return;
    const { result } = await this.send('Page.captureScreenshot', { format: 'png' });
    fs.writeFileSync(path.join(shots, `${name}.png`), Buffer.from(result.data, 'base64'));
  }

  close() { this.ws.close(); }
}

const audioTracks = (file) => (spawnSync(ffmpeg, ['-hide_banner', '-i', file], { encoding: 'utf8', windowsHide: true }).stderr.match(/Audio: aac/g) ?? []).length;
const lengthOf = (file) => {
  const m = /Duration: (\d+):(\d+):(\d+\.\d+)/.exec(spawnSync(ffmpeg, ['-hide_banner', '-i', file], { encoding: 'utf8', windowsHide: true }).stderr);
  return m ? Number(m[1]) * 3600 + Number(m[2]) * 60 + Number(m[3]) : 0;
};

async function main() {
  if (shots) fs.mkdirSync(shots, { recursive: true });
  const session = await startApp({
    bufferSeconds: 40, width: 1280, height: 720, fps: 30, bitrateKbps: 6000, testPattern: true, micHotkey: '', markerHotkey: '',
    audioSources: [{ kind: 'output', device: 'default', label: 'Desktop audio', mix: true, ownTrack: false, muteWithKey: true }],
  });
  const { app, api, output } = session;
  let panel;
  let editor;
  try {
    await sleep(3000);
    check('starts with no window, in the tray only', windowInfo(app.pid).length === 0);

    await api.call('/test/panel-hold/on', 'POST');
    await api.call('/test/open/panel', 'POST');
    const opened = await waitFor(() => { const w = windowInfo(app.pid); return w.length && w; }, 12000, 'panel window');
    check('the panel opens', opened.length === 1);
    // White text and icons cover a few sample points. A blank page covers nearly all of them.
    check('panel is not blank on the real screen', opened[0].whitePct < 10, `${opened[0].whitePct}% white`);
    const area = JSON.parse(powershell(`Add-Type -Name A -Namespace N -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'; [void][N.A]::SetProcessDPIAware(); Add-Type -AssemblyName System.Windows.Forms; $w = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea; @{ r = $w.Right; b = $w.Bottom } | ConvertTo-Json -Compress`));
    const edge = opened[0];
    check('panel is docked flush into the taskbar corner', edge.x + edge.w === area.r && edge.y + edge.h === area.b, `panel ends ${edge.x + edge.w},${edge.y + edge.h}, work area ends ${area.r},${area.b}, size ${edge.w}x${edge.h}`);

    panel = await Page.open(PANEL_URL);
    await waitFor(() => panel.eval("document.getElementById('capture-title').textContent.includes('primary') || document.getElementById('capture-title').textContent.length > 0"), 5000, 'status');
    await waitFor(() => panel.eval("!document.getElementById('save-clip').disabled"), 15000, 'save button');
    check('panel shows buffer summary', /buffer/.test(await panel.eval("document.getElementById('summary').textContent")));
    check('buffer reads in minutes', (await panel.eval("document.getElementById('bufferMinutes').value")) === '0.67');
    await panel.shot('1-panel-empty');

    // Watches from before the click, because the popup only lasts a moment.
    const toastWatch = spawn('powershell', ['-NoProfile', '-Command', `Add-Type -Name T -Namespace N -MemberDefinition '[DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern System.IntPtr FindWindow(string c, string t);'; $end = (Get-Date).AddSeconds(35); while ((Get-Date) -lt $end) { if ([N.T]::FindWindow('AfterclipToast', 'Afterclip') -ne [IntPtr]::Zero) { 'True'; exit }; Start-Sleep -Milliseconds 100 }; 'False'`], { windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] });
    const toastSeen = new Promise((resolve) => { let out = ''; toastWatch.stdout.on('data', (d) => { out += d; }); toastWatch.on('close', () => resolve(out.trim())); });
    console.log('  letting the buffer fill');
    await sleep(14000);
    await panel.eval("document.getElementById('save-clip').click()");
    await waitFor(() => panel.eval("document.querySelectorAll('#media .item').length === 1"), 15000, 'clip in list');
    check('Save button creates a clip in the list', true);
    const toasts = await toastSeen;
    check('a saved clip shows the popup', toasts === 'True', toasts);
    check('the ring pulses when a clip saves', await panel.eval("document.getElementById('halo').classList.contains('saved')"));
    const ring = await panel.eval("({ filled: parseFloat(document.getElementById('halo-filled').getAttribute('stroke-dasharray')), kept: parseFloat(document.getElementById('halo-selected').getAttribute('stroke-dasharray')), time: document.getElementById('halo-time').textContent })");
    check('the ring shows the buffer filling and the selected length', ring.filled > 0 && ring.kept > 0 && ring.kept <= ring.filled && /^0:\d\d$/.test(ring.time), JSON.stringify(ring));
    await waitFor(() => panel.eval("!!document.querySelector('#media img')?.src"), 10000, 'thumbnail');
    check('clip shows a thumbnail and length', /\d:\d\d/.test(await panel.eval("document.querySelector('#media .length').textContent")));
    await panel.shot('2-panel-clip');

    // A second and third clip, then one more than the home screen shows.
    for (let i = 0; i < 3; i++) await api.clip(2);
    await waitFor(() => panel.eval("/View all \\(4\\)/.test(document.getElementById('view-all').textContent)"), 8000, 'the fourth clip');
    check('home shows only the three newest clips', (await panel.eval("document.querySelectorAll('#media .item').length")) === 3);
    await panel.eval("document.getElementById('view-all').click()");
    const library = await panel.eval("({ items: document.querySelectorAll('#media .item').length, grid: getComputedStyle(document.getElementById('media')).display, ring: getComputedStyle(document.getElementById('halo')).display, editor: !document.getElementById('open-editor').hidden })");
    check('View all opens every clip in a grid, with the editor link', library.items === 4 && library.grid === 'grid' && library.ring === 'none' && library.editor, JSON.stringify(library));
    await panel.shot('2b-library');

    // Copy file, keeping whatever text was on the clipboard before.
    const clipFile = await panel.eval("window.afterclip.invoke('state').then((s) => s.media[0].file)");
    const oldClipboard = powershell('Get-Clipboard -Raw');
    await panel.eval(`window.afterclip.invoke('copy-file', ${JSON.stringify(clipFile)})`);
    const copied = powershell('(Get-Clipboard -Format FileDropList).FullName');
    if (oldClipboard) execFileSync('powershell', ['-NoProfile', '-Command', '$input | Set-Clipboard'], { input: oldClipboard, windowsHide: true });
    check('Copy file puts the clip on the clipboard', copied.toLowerCase() === clipFile.toLowerCase(), copied);
    const refused = await panel.eval("window.afterclip.invoke('start-drag', 'C:\\\\Windows\\\\win.ini').then(() => 'allowed', (e) => e.message)");
    check('dragging refuses files outside your clips', /Not a Afterclip file/.test(refused), refused);

    // A real drag with the real mouse, onto a small test window that accepts dropped files.
    const dropResult = path.join(session.home, 'dropped.txt');
    const dropTarget = spawn('powershell', ['-NoProfile', '-STA', '-Command', `Add-Type -AssemblyName System.Windows.Forms; $f = New-Object Windows.Forms.Form; $f.Text = 'Afterclip drop test'; $f.AllowDrop = $true; $f.TopMost = $true; $f.ShowInTaskbar = $false; $f.StartPosition = 'Manual'; $f.Left = 60; $f.Top = 60; $f.Width = 400; $f.Height = 400; $f.Add_DragEnter({ param($s,$e) $e.Effect = 'Copy' }); $f.Add_DragDrop({ param($s,$e) ($e.Data.GetData('FileDrop') -join ';') | Set-Content -LiteralPath '${dropResult}'; $s.Close() }); $t = New-Object Windows.Forms.Timer; $t.Interval = 20000; $t.Add_Tick({ $f.Close() }); $t.Start(); [void]$f.ShowDialog()`], { windowsHide: false, stdio: 'ignore' });
    await sleep(3000);
    const spot = await panel.eval("(() => { const r = document.querySelector('#media .item .item-text').getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2, scale: devicePixelRatio }; })()");
    const place = windowInfo(app.pid).find((w) => w.title === 'Afterclip');
    const from = { x: Math.round(place.x + spot.x * spot.scale), y: Math.round(place.y + spot.y * spot.scale) };
    powershell(`Add-Type -Name M -Namespace N -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y); [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, System.UIntPtr e); [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'; [void][N.M]::SetProcessDPIAware();
[void][N.M]::SetCursorPos(${from.x}, ${from.y}); Start-Sleep -Milliseconds 300; [N.M]::mouse_event(2,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 150;
foreach ($i in 1..4) { [void][N.M]::SetCursorPos(${from.x} - $i * 8, ${from.y}); Start-Sleep -Milliseconds 60 }; Start-Sleep -Milliseconds 900;
foreach ($i in 1..10) { [void][N.M]::SetCursorPos(${from.x} + (250 - ${from.x}) * $i / 10, ${from.y} + (250 - ${from.y}) * $i / 10); Start-Sleep -Milliseconds 50 }; Start-Sleep -Milliseconds 500; [N.M]::mouse_event(4,0,0,0,[UIntPtr]::Zero)`);
    const dropped = await waitFor(() => fs.existsSync(dropResult) && fs.readFileSync(dropResult, 'utf8').trim(), 6000, 'the dropped file').catch(() => '');
    dropTarget.kill();
    check('dragging a clip drops the real file into another app', dropped.toLowerCase().endsWith('.mp4') && fs.existsSync(dropped), dropped || 'nothing dropped');
    check('dragging leaves the panel responding', (await panel.eval('1 + 1')) === 2);

    await panel.eval("document.getElementById('library-back').click()");
    check('Back returns to the ring', await panel.eval("getComputedStyle(document.getElementById('halo')).display !== 'none'"));

    await panel.eval("document.getElementById('open-settings').click()");
    await waitFor(() => panel.eval("document.querySelectorAll('#audio-list select option').length > 0"), 8000, 'audio devices');
    const counts = await panel.eval(`window.afterclip.invoke('audio-choices').then((c) => ({ outputs: c.outputs.length, inputs: c.inputs.length, apps: c.apps.length, discord: c.apps.some((a) => /discord/i.test(a.id)) }))`);
    check('lists output devices', counts.outputs > 0, `${counts.outputs}`);
    check('lists input devices', counts.inputs > 0, `${counts.inputs}`);
    check('lists running apps', counts.apps > 0, `${counts.apps} apps, Discord running: ${counts.discord}`);

    // A separate track for the desktop source, and an app source if there's one to add.
    await panel.eval(`(() => {
      document.querySelector('[data-add="app"]').click();
      const toggles = [...document.querySelectorAll('#audio-list .audio-row')][0].querySelectorAll('input[type=checkbox]');
      toggles[1].click();
    })()`);
    const rows = await panel.eval("document.querySelectorAll('#audio-list .audio-row').length");
    check('can add an app source', rows === 2, `${rows} sources`);
    const added = await panel.eval(`(() => { const r = document.querySelectorAll('#audio-list .audio-row')[1]; const c = r.querySelectorAll('input[type=checkbox]'); return { mix: c[0].checked, own: c[1].checked }; })()`);
    check('a new app defaults to its own track, out of the main mix', added.mix === false && added.own === true);
    await panel.shot('3-panel-settings-audio');
    check('there is no Save button', await panel.eval("!document.getElementById('save-settings')"));
    await waitFor(() => panel.eval("document.getElementById('save-note').textContent.includes('restarted')"), 10000, 'settings saved by themselves');
    const onDisk = JSON.parse(fs.readFileSync(path.join(session.home, 'userData', 'settings.json'), 'utf8'));
    check('changes save without pressing anything', onDisk.audioSources.length === 2);
    await waitFor(async () => { const s = await api.status(); return s.state === 'ready' && s.buffering; }, 30000, 'capture back after save');
    check('saving audio restarts capture', true);
    await sleep(6000);
    const multi = await api.clip(0);
    const tracks = audioTracks(multi.file);
    check('clips contain the main mix and separate tracks', tracks === 3, `${tracks} audio tracks`);
    const failed = fs.readFileSync(path.join(session.home, 'engine', 'engine.log'), 'utf8').match(/Could not create audio source.*/g);
    check('every audio source started', !failed, failed?.join('; ') ?? '');

    // Mic mute: the source marked in the test's settings, through the panel's button.
    const engineLogNow = () => fs.readFileSync(path.join(session.home, 'engine', 'engine.log'), 'utf8');
    await panel.eval("document.getElementById('mic').click()");
    await waitFor(async () => (await api.status()).micMuted, 5000, 'mic muted');
    check('mic button mutes the marked source', /Mic mute on on 1 source/.test(engineLogNow()));
    check('mic button shows it is muted', await waitFor(() => panel.eval("document.getElementById('mic').classList.contains('muted')"), 3000, 'mic button state'));
    const micRes = await (await api.call('/mic/toggle', 'POST')).json();
    check('the Stream Deck route unmutes', micRes.micMuted === false && /Mic mute off on 1 source/.test(engineLogNow()));
    // Clean Mic, volume, meters, stats and markers, through the real panel.
    await panel.eval("document.getElementById('clean-mic').click()");
    await waitFor(async () => (await api.status()).cleanMic, 5000, 'clean mic on');
    check('Clean Mic turns on its filters', /Clean Mic on on 2 filter/.test(engineLogNow()));
    await (await api.call('/mic/clean', 'POST')).json();
    check('Clean Mic turns off from the Stream Deck route', /Clean Mic off on 2 filter/.test(engineLogNow()));
    const logLength = engineLogNow().length;
    await panel.eval(`(() => { const s = document.querySelector('#audio-list .volume'); s.value = 150; s.dispatchEvent(new Event('input')); })()`);
    check('volume slider shows its value', (await panel.eval("document.querySelector('#audio-list .volume-value').textContent")) === '150%');
    await sleep(1500);
    check('volume saves without restarting capture', JSON.parse(fs.readFileSync(path.join(session.home, 'userData', 'settings.json'), 'utf8')).audioSources[0].volume === 1.5 && (await api.status()).buffering);
    check('volume change does not restart the engine', !/Replay Buffer': stopping/.test(engineLogNow().slice(logLength)));
    const levels = await panel.eval(`window.afterclip.invoke('set-meters', true).then(() => new Promise((r) => setTimeout(r, 500))).then(() => window.afterclip.invoke('levels'))`);
    check('meters report every source', Array.isArray(levels) && levels.length >= 2, JSON.stringify(levels));
    await panel.eval("window.afterclip.invoke('set-meters', false)");
    const stats = await panel.eval("window.afterclip.invoke('stats')");
    check('performance stats are live', stats.fps > 0 && stats.encoderTotal > 0 && stats.diskFreeMb > 0, JSON.stringify(stats));
    const noMarker = await api.call('/marker', 'POST');
    check('a marker needs a recording', noMarker.status === 500);

    check('run as administrator setting exists, off by default', await panel.eval("document.getElementById('runAsAdmin').checked === false"));

    // Quality presets, worked out from the real monitor, on the real encoder.
    const mode = await panel.eval("window.afterclip.invoke('display-info')");
    const value = (id) => `document.getElementById('${id}').value`;
    const pick = async (q) => panel.eval(`(() => { const s = document.getElementById('quality'); s.value = '${q}'; s.dispatchEvent(new Event('change')); return { res: ${value('resolution')}, fr: ${value('frameRate')}, kbps: +${value('bitrateKbps')}, preset: ${value('encoderPreset')}, mp: ${value('multipass')} }; })()`);
    const low = await pick('low');
    check('Low quality preset', low.res === '720' && low.fr === '60' && low.kbps === 8000 && low.preset === 'p1', JSON.stringify(low));
    const medium = await pick('medium');
    check('Medium quality preset', medium.res === '1080' && medium.fr === '60' && medium.kbps === 20000, JSON.stringify(medium));
    const high = await pick('high');
    check('High quality preset', high.res === 'native' && high.kbps === 40000 && high.mp === 'false', JSON.stringify(high));
    const frameOptions = await panel.eval(`[...document.getElementById('frameRate').options].map((o) => o.value).join()`);
    check('frame rates are 60, 120 and native', frameOptions.startsWith('60,120,native'), frameOptions);
    await panel.eval(`(() => { const s = document.getElementById('codec'); s.value = 'h264'; s.dispatchEvent(new Event('change')); })()`);
    check('changing a field makes the profile Custom', (await panel.eval(value('quality'))) === 'custom');
    const full = await pick('full');
    check('Full quality preset uses the monitor at its limits', full.res === 'native' && full.fr === 'native' && full.kbps === 150000 && full.preset === 'p5' && full.mp === 'true', JSON.stringify(full));
    check('every setting section opens', await panel.eval(`(() => { for (const b of document.querySelectorAll('.nav-item')) { b.click(); if (document.querySelector('.pane:not([hidden])')?.dataset.pane !== b.dataset.pane) return false; } document.getElementById('go-back').click(); return true; })()`));
    await panel.shot('3b-panel-quality-full');
    await panel.eval("(() => { const b = document.getElementById('bufferMinutes'); b.value = '0.5'; b.dispatchEvent(new Event('change')); })()");
    await waitFor(async () => (await api.status()).state !== 'ready', 8000, 'restart to begin').catch(() => undefined);
    await waitFor(async () => { const s = await api.status(); return s.state === 'ready' && s.buffering; }, 30000, 'capture back on Full');
    await sleep(8000);
    const best = await api.clip(0);
    const bestInfo = spawnSync(ffmpeg, ['-hide_banner', '-i', best.file], { encoding: 'utf8' }).stderr;
    check('Full quality clip is at the monitor resolution', bestInfo.includes(`${mode.width}x${mode.height}`), `${best.actualSeconds.toFixed(1)}s`);
    const engineLog = fs.readFileSync(path.join(session.home, 'engine', 'engine.log'), 'utf8');
    check('Full quality runs the higher encoder settings', /preset:\s+p5/.test(engineLog) && /bitrate:\s+150000/.test(engineLog) && /multipass:\s+qres/.test(engineLog));

    // The exact clicks that froze the app: the panel's Open editor button, then a clip's Edit menu item.
    await api.call('/test/panel-hold/off', 'POST');
    await panel.eval("document.getElementById('go-back').click(); document.getElementById('open-editor').click()");
    editor = await Page.open(/editor\.html/);
    check('Open editor button works without freezing', true);
    await waitFor(() => panel.eval("!document.getElementById('home').hidden"), 5000, 'home view');
    await waitFor(() => panel.eval("document.querySelectorAll('#media .more').length >= 2"), 10000, 'clips in list');
    await panel.eval(`(() => { document.querySelector('#media .more').click(); [...document.querySelectorAll('.menu button')].find((b) => b.textContent === 'Edit').click(); })()`);
    await waitFor(() => editor.eval("document.querySelectorAll('.segment').length === 1"), 10000, 'clip from Edit menu');
    check('a clip\'s Edit menu item opens it in the editor', true);
    check('the app still answers after opening the editor', (await api.status()).buffering);
    await waitFor(() => editor.eval("document.querySelectorAll('.lib-item').length >= 2"), 10000, 'editor library');
    const editorWin = await waitFor(() => windowInfo(app.pid).find((w) => /Editor/.test(w.title)), 12000, 'editor window');
    check('editor window opens, not blank', editorWin.whitePct < 10, `${editorWin.whitePct}% white`);
    await sleep(2000);
    check('panel hides when it loses focus', !windowInfo(app.pid).some((w) => w.title === 'Afterclip'));

    await waitFor(() => editor.eval("document.getElementById('video').readyState >= 2"), 10000, 'video to load');
    check('editor can load the video', true);
    const moved = await editor.eval(`(async () => { const v = document.getElementById('video'); const before = v.currentTime; v.muted = true; await v.play(); await new Promise((r) => setTimeout(r, 1200)); v.pause(); return v.currentTime - before; })()`);
    check('editor preview actually plays', moved > 0.5, `${moved.toFixed(2)}s advanced`);

    // Trim by dragging the real handle with real mouse events through the debugger.
    const box = await editor.eval(`(() => { const t = document.getElementById('trim').getBoundingClientRect(); const h = document.getElementById('handle-end').getBoundingClientRect(); return { left: t.left, width: t.width, x: h.left + h.width / 2, y: h.top + h.height / 2 }; })()`);
    const dur = await editor.eval('document.getElementById("video").duration');
    const toX = box.left + box.width * (5 / dur);
    for (const [type, x, buttons] of [['mouseMoved', box.x, 0], ['mousePressed', box.x, 1], ['mouseMoved', (box.x + toX) / 2, 1], ['mouseMoved', toX, 1], ['mouseReleased', toX, 0]]) {
      await editor.send('Input.dispatchMouseEvent', { type, x, y: box.y, button: type === 'mouseMoved' && !buttons ? 'none' : 'left', buttons, clickCount: type.startsWith('mouseM') ? 0 : 1 });
    }
    const label = await editor.eval("document.querySelector('.segment-foot span').textContent");
    check('dragging the end handle trims', /^0:0[45]/.test(label), label);
    await editor.shot('4-editor-trim');

    const clips = path.join(output, 'Clips');
    const before = new Set(fs.readdirSync(clips));
    const newFile = () => fs.readdirSync(clips).find((f) => !before.has(f));
    await editor.eval("document.getElementById('export').click()");
    const edited = await waitFor(() => { const f = newFile(); return f && /Saved/.test(fs.existsSync(path.join(clips, f)) ? 'Saved' : '') && f; }, 15000, 'trimmed file');
    await waitFor(() => editor.eval("!document.getElementById('export').disabled"), 15000, 'export to finish');
    const trimmedLength = lengthOf(path.join(clips, edited));
    check('trim saves a new file', /^Edit_/.test(edited) && trimmedLength >= 4 && trimmedLength <= 6.5, `${edited} ${trimmedLength.toFixed(2)}s`);
    check('trim keeps its separate tracks', audioTracks(path.join(clips, edited)) === 3 || audioTracks(path.join(clips, edited)) === audioTracks(multi.file));

    before.add(edited);
    // Add the same clip again, which also proves a merge of two ranges.
    await editor.eval("document.querySelectorAll('.lib-item')[0].click()");
    await waitFor(() => editor.eval("document.querySelectorAll('.segment').length === 2"), 8000, 'second segment');
    await sleep(500);
    await editor.shot('5-editor-merge');
    await editor.eval("document.getElementById('export').click()");
    const merged = await waitFor(() => newFile(), 15000, 'merged file');
    await waitFor(() => editor.eval("document.getElementById('export').textContent !== 'Saving'"), 20000, 'merge to finish');
    const message = await editor.eval("document.getElementById('message').textContent");
    const mergedLength = lengthOf(path.join(clips, merged));
    check('merge saves a longer video', /^Merged_/.test(merged) && mergedLength > trimmedLength, `${merged} ${mergedLength.toFixed(2)}s (${message})`);
    const decode = spawnSync(ffmpeg, ['-hide_banner', '-v', 'error', '-i', path.join(clips, merged), '-f', 'null', '-'], { encoding: 'utf8' });
    check('merged video decodes cleanly', decode.status === 0 && !decode.stderr.trim(), decode.stderr.trim().slice(0, 160));

    const bad = await editor.eval(`window.afterclip.invoke('export-edit', [{ file: 'C:\\\\Windows\\\\notepad.exe', start: 0, end: -1 }]).then(() => 'allowed', (e) => e.message)`);
    check('editor refuses files outside your clips', /Not a Afterclip file/.test(bad), bad);
    const asset = await editor.eval(`fetch(window.afterclip.videoUrl('C:\\\\Windows\\\\win.ini')).then((r) => r.status, () => 'blocked')`);
    check('video access is limited to your clips', asset !== 200, `${asset}`);

    panel.close(); panel = null;
    console.log('  waiting for the hidden panel to free its memory');
    await sleep(23000);
    const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
    check('hidden panel is destroyed after 20s', !targets.some((t) => PANEL_URL.test(t.url)));
  } catch (e) {
    const pages = await fetch(`http://127.0.0.1:${PORT}/json`).then((r) => r.json()).then((l) => l.map((t) => t.url).join(', ')).catch((err) => err.message);
    check('no exceptions', false, `${e.message} | pages: ${pages} | windows: ${JSON.stringify(windowInfo(app.pid))}`);
  } finally {
    panel?.close();
    editor?.close();
    await session.stop();
  }
  finish();
}

main();
