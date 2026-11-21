// Builds a local test copy, versioned like 1.6.0-dev, so it's never mistaken for a release. A dev version counts
// as older than its release, so the real 1.6.0 still installs over it from the app's own updater.
//   npm run dist
// It isn't signed and makes no latest.json. Releases come from scripts/build-release.js.
const { execSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const { version } = JSON.parse(fs.readFileSync(path.join(root, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const dev = `${version.replace(/-.*$/, '')}-dev`;
const bundle = path.join(root, 'src-tauri', 'target', 'release', 'bundle', 'nsis');

// A release's signature and latest.json beside a dev installer would match nothing, so they go.
for (const name of fs.existsSync(bundle) ? fs.readdirSync(bundle) : []) {
  if (name.endsWith('.sig') || name === 'latest.json') fs.rmSync(path.join(bundle, name));
}

const config = path.join(root, 'src-tauri', 'target', 'tauri.dev.conf.json');
fs.mkdirSync(path.dirname(config), { recursive: true });
fs.writeFileSync(config, `${JSON.stringify({ version: dev })}\n`);
execSync('npm run build:engine', { cwd: root, stdio: 'inherit' });
execSync(`npx tauri build --config "${config}"`, { cwd: root, stdio: 'inherit' });
console.log(`Local test build: ${path.join(bundle, `Afterclip_${dev}_x64-setup.exe`)}`);
