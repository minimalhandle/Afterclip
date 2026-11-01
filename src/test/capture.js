// Checks real capture, which the colour-card test can't. It records the actual desktop to a temp folder,
// measures only colour, and deletes the clips without saving a frame. It opens a fullscreen magenta window
// for about 20 seconds as a stand-in for a game that can't be hooked, on a non-primary monitor if there is one.
//   node src/test/capture.js
const { execFile, spawn } = require('node:child_process');
const { ffmpeg, sleep, check, finish, waitFor, powershell, startApp } = require('./harness');

function averageColour(file) {
  return new Promise((resolve, reject) => {
    execFile(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-sseof', '-1', '-i', file, '-vf', 'scale=1:1:flags=area', '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'],
      { encoding: 'buffer', windowsHide: true }, (err, stdout) => (err || stdout.length < 3 ? reject(err ?? new Error('no frame')) : resolve([stdout[0], stdout[1], stdout[2]])));
  });
}
const isMagenta = ([r, g, b]) => r > 200 && g < 60 && b > 200;

function monitors() {
  const json = powershell("Add-Type -Name D -Namespace N -MemberDefinition '[DllImport(\"user32.dll\")] public static extern bool SetProcessDPIAware();'; [void][N.D]::SetProcessDPIAware(); Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Screen]::AllScreens | ForEach-Object { @{ primary = $_.Primary; x = $_.Bounds.X; y = $_.Bounds.Y; w = $_.Bounds.Width; h = $_.Bounds.Height } } | ConvertTo-Json -Compress");
  return [JSON.parse(json)].flat();
}

function openStandIn({ x, y, w, h }) {
  const script = `Add-Type -AssemblyName System.Windows.Forms;
Add-Type -Name K -Namespace N -MemberDefinition '[DllImport("user32.dll")] public static extern void keybd_event(byte v, byte s, uint f, System.UIntPtr e); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr h); [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();';
[void][N.K]::SetProcessDPIAware();
$f = New-Object System.Windows.Forms.Form; $f.FormBorderStyle = 'None'; $f.StartPosition = 'Manual'; $f.BackColor = [System.Drawing.Color]::FromArgb(255,0,255); $f.Text = 'Afterclip capture test'; $f.ShowInTaskbar = $false;
$f.Bounds = New-Object System.Drawing.Rectangle(${x},${y},${w},${h}); $f.TopMost = $true;
$f.Add_Shown({ [N.K]::keybd_event(0x12,0,0,[UIntPtr]::Zero); [N.K]::keybd_event(0x12,0,2,[UIntPtr]::Zero); [void][N.K]::SetForegroundWindow($f.Handle) });
[System.Windows.Forms.Application]::Run($f)`;
  // No windowsHide here: it would apply to the form too, and a hidden window is never in the foreground.
  return spawn('powershell', ['-NoProfile', '-Command', script], { stdio: 'ignore' });
}

async function main() {
  const all = monitors();
  const primary = all.find((m) => m.primary);
  const session = await startApp({ bufferSeconds: 20, width: primary.w, height: primary.h, fps: 30, bitrateKbps: 8000, audioSources: [], presets: [], recordHotkey: '' });
  const { api } = session;
  let standIn;
  try {
    await sleep(7000);
    const desktop = await api.clip(4);
    const dc = await averageColour(desktop.file);
    // A black capture encodes to about 4,0,5, so the bar has to sit well above that.
    check('desktop clip is not black', dc.some((v) => v > 12), `average rgb ${dc.join(',')}`);
    check('starts on primary monitor', (await api.status()).capture.mode === 'desktop');

    const target = all.find((m) => !m.primary && m.w > m.h) ?? primary;
    console.log(`  fullscreen stand-in on ${target === primary ? 'primary' : 'another'} monitor`);
    standIn = openStandIn(target);
    const mode = async () => (await api.status()).capture.mode;
    await waitFor(async () => (await mode()) !== 'desktop', 10000, 'fullscreen detection');
    check('fullscreen app detected', true, (await api.status()).capture.app);
    const settled = await waitFor(async () => { const m = await mode(); return ['fallback', 'game'].includes(m) && m; }, 16000, 'fallback');
    check('falls back when the hook fails', settled === 'fallback', settled);
    await sleep(3000);
    const fallback = await api.clip(3);
    const fc = await averageColour(fallback.file);
    check("records the app's monitor", isMagenta(fc), `average rgb ${fc.join(',')} expected magenta`);

    spawn('taskkill', ['/PID', String(standIn.pid), '/T', '/F'], { stdio: 'ignore' });
    await waitFor(async () => (await mode()) === 'desktop', 8000, 'return to desktop');
    check('returns to primary monitor when closed', true);
    await sleep(3000);
    const back = await api.clip(2);
    const bc = await averageColour(back.file);
    check('desktop is captured again after', !isMagenta(bc) && bc.some((v) => v > 12), `average rgb ${bc.join(',')}`);
  } catch (e) {
    check('no exceptions', false, e.message);
  } finally {
    if (standIn) spawn('taskkill', ['/PID', String(standIn.pid), '/T', '/F'], { stdio: 'ignore' });
    await session.stop();
  }
  finish();
}

main();
