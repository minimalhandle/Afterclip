// Builds a release: the signed installer, and the latest.json that existing copies check for updates.
// Only run this when a release has been decided on. It needs the private signing key, which is never committed.
//   node scripts/build-release.js
// The key is read from AFTERCLIP_SIGNING_KEY_FILE, or %USERPROFILE%\.afterclip\updater.key.
// Upload the installer, its .sig file and latest.json to the GitHub release.
const { execSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const root = path.join(__dirname, '..');
const keyFile = process.env.AFTERCLIP_SIGNING_KEY_FILE ?? path.join(os.homedir(), '.afterclip', 'updater.key');
if (!fs.existsSync(keyFile)) throw new Error(`No signing key at ${keyFile}. Existing copies refuse unsigned updates.`);

const { version, plugins } = JSON.parse(fs.readFileSync(path.join(root, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const env = {
  ...process.env,
  TAURI_SIGNING_PRIVATE_KEY: fs.readFileSync(keyFile, 'utf8').trim(),
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD: process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? '',
};
execSync('npm run build:engine', { cwd: root, stdio: 'inherit' });
execSync('npx tauri build --config src-tauri/tauri.release.conf.json', { cwd: root, stdio: 'inherit', env });

const dir = path.join(root, 'src-tauri', 'target', 'release', 'bundle', 'nsis');
const installer = `Afterclip_${version}_x64-setup.exe`;
const signature = path.join(dir, `${installer}.sig`);
if (!fs.existsSync(signature)) throw new Error(`The build didn't sign ${installer}`);

// The address the updater reads ends in latest/download, so each release replaces this for everyone.
const repository = plugins.updater.endpoints[0].split('/releases/')[0];
const latest = {
  version,
  notes: `See ${repository}/releases/tag/v${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    'windows-x86_64': {
      signature: fs.readFileSync(signature, 'utf8').trim(),
      url: `${repository}/releases/download/v${version}/${installer}`,
    },
  },
};
fs.writeFileSync(path.join(dir, 'latest.json'), `${JSON.stringify(latest, null, 2)}\n`);
console.log(`Ready to upload from ${dir}: ${installer}, ${installer}.sig, latest.json`);
