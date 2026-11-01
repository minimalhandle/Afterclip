// Downloads the official OBS Studio Windows build into vendor/obs, where packaging picks it up.
// Bump OBS_VERSION to update. The hash is GitHub's published digest for the release asset.
const { execFileSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const OBS_VERSION = '33.0.1';
const REPO = 'obsproject/obs-studio';
const vendor = path.join(__dirname, '..', 'vendor');
const target = path.join(vendor, 'obs');
const marker = path.join(target, 'afterclip-obs-version.txt');

async function main() {
  if (fs.existsSync(marker) && fs.readFileSync(marker, 'utf8').trim() === OBS_VERSION) {
    console.log(`OBS ${OBS_VERSION} already in vendor/obs`);
    return;
  }
  const release = await (await fetch(`https://api.github.com/repos/${REPO}/releases/tags/${OBS_VERSION}`)).json();
  const name = `OBS-Studio-${OBS_VERSION}-Windows-x64.zip`;
  const asset = release.assets?.find((a) => a.name === name);
  if (!asset) throw new Error(`${name} not found in release ${OBS_VERSION}`);

  console.log(`Downloading ${name}`);
  const res = await fetch(asset.browser_download_url);
  if (!res.ok) throw new Error(`Download failed: ${res.status}`);
  const zip = Buffer.from(await res.arrayBuffer());
  const sha = crypto.createHash('sha256').update(zip).digest('hex');
  if (!asset.digest) throw new Error('GitHub published no digest, so the download cannot be verified');
  if (asset.digest !== `sha256:${sha}`) throw new Error(`Checksum mismatch: got ${sha}, expected ${asset.digest}`);
  console.log(`Verified sha256 ${sha}`);

  fs.rmSync(target, { recursive: true, force: true });
  fs.mkdirSync(target, { recursive: true });
  const zipFile = path.join(vendor, name);
  fs.writeFileSync(zipFile, zip);
  execFileSync('tar', ['-xf', zipFile, '-C', target]);
  fs.unlinkSync(zipFile);
  if (!fs.existsSync(path.join(target, 'bin', '64bit', 'obs64.exe'))) throw new Error('obs64.exe missing after extraction');
  fs.writeFileSync(marker, OBS_VERSION);
}

main().catch((e) => { console.error(e.message); process.exit(1); });
