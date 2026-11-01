// Shared helpers for tests that drive the real Afterclip exe from outside, in throwaway folders.
const { execFileSync, spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const root = path.resolve(__dirname, '..', '..');
const exe = process.env.AFTERCLIP_APP_EXE ?? path.join(root, 'src-tauri', 'target', 'release', 'afterclip.exe');
const ffmpeg = require(path.join(root, 'node_modules', 'ffmpeg-static'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const TEST_PORT = '4567';
const API_BASE = `http://127.0.0.1:${TEST_PORT}`;

const failures = [];
function check(name, ok, detail = '') {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name} ${detail}`);
  if (!ok) failures.push(name);
}

function finish() {
  console.log(failures.length ? `\n${failures.length} FAILED: ${failures.join('; ')}` : '\nALL PASSED');
  process.exit(failures.length ? 1 : 0);
}

async function waitFor(fn, ms, label) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const value = await Promise.resolve().then(fn).catch(() => undefined);
    if (value) return value;
    await sleep(200);
  }
  throw new Error(`Timed out waiting for ${label}`);
}

function powershell(script) {
  // Hidden, because a console window takes focus, which closes the panel being tested.
  return execFileSync('powershell', ['-NoProfile', '-Command', script], { encoding: 'utf8', windowsHide: true }).trim();
}

/** The tests hook fullscreen apps, take focus and press keys, so they must never run during someone's game. */
function refuseIfFullscreenAppRunning() {
  const script = `Add-Type -AssemblyName System.Windows.Forms;
Add-Type -Name G -Namespace N -MemberDefinition '[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow(); [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); [DllImport("user32.dll")] public static extern bool GetWindowRect(System.IntPtr h, out RECT r); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr h, out uint p); public struct RECT { public int L, T, R, B; }';
[void][N.G]::SetProcessDPIAware(); $h = [N.G]::GetForegroundWindow(); $r = New-Object N.G+RECT; [void][N.G]::GetWindowRect($h, [ref]$r);
$s = [System.Windows.Forms.Screen]::FromHandle($h).Bounds; $o = [uint32]0; [void][N.G]::GetWindowThreadProcessId($h, [ref]$o); $n = (Get-Process -Id $o).ProcessName;
if ($r.L -le $s.Left -and $r.T -le $s.Top -and $r.R -ge $s.Right -and $r.B -ge $s.Bottom -and $n -ne 'explorer') { $n }`;
  const running = powershell(script);
  if (running) {
    console.error(`Not running: ${running} is fullscreen. These tests hook fullscreen apps and take focus, so close it first.`);
    process.exit(2);
  }
}

/** Starts Afterclip hidden with these settings, and resolves when its buffer is running. */
async function startApp(settings) {
  refuseIfFullscreenAppRunning();
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'afterclip-home-'));
  const output = path.join(home, 'out');
  const userData = path.join(home, 'userData');
  fs.mkdirSync(userData, { recursive: true });
  fs.writeFileSync(path.join(userData, 'settings.json'), JSON.stringify({ outputDir: output, ...settings }));
  // AFTERCLIP_HOME makes this a separate test copy, with its own folders and port, beside any real running app.
  const app = spawn(exe, [], { stdio: 'ignore', env: { ...process.env, AFTERCLIP_HOME: home, AFTERCLIP_API_PORT: TEST_PORT } });

  const tokenFile = path.join(userData, 'api-token.txt');
  await waitFor(() => fs.existsSync(tokenFile), 20000, 'API token');
  const token = fs.readFileSync(tokenFile, 'utf8').trim();
  const call = (route, method = 'GET') => fetch(`${API_BASE}${route}`, { method, headers: { Authorization: `Bearer ${token}` } });
  const api = {
    call,
    status: async () => (await call('/status')).json(),
    clip: async (seconds) => {
      const res = await call(`/clip/${seconds}`, 'POST');
      const body = await res.json();
      if (!res.ok) throw new Error(body.error);
      return body;
    },
    toggleRecord: async () => (await call('/record/toggle', 'POST')).json(),
  };
  await waitFor(async () => (await api.status()).buffering, 30000, 'buffer to start');

  const stop = async () => {
    // A forced kill closes the engine's pipes, which is the case that must never strand it.
    try { execFileSync('taskkill', ['/PID', String(app.pid), '/T', '/F'], { stdio: 'ignore' }); } catch { /* already gone */ }
    await sleep(1500);
    fs.rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
  };
  return { app, api, home, output, tokenFile, token, stop };
}

module.exports = { API_BASE, root, exe, ffmpeg, sleep, check, finish, waitFor, powershell, startApp };
