// Compiles engine/afterclip-engine.cpp against the official OBS build, then assembles vendor/engine:
// the engine plus only the files it needs. Needs Visual Studio with the C++ tools.
const { execFileSync, execSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const obsDir = path.join(root, 'vendor', 'obs');
const obsBin = path.join(obsDir, 'bin', '64bit');
const srcDir = path.join(root, 'vendor', 'obs-src');
const buildDir = path.join(root, 'vendor', 'engine-build');
const outDir = path.join(root, 'vendor', 'engine');
const outBin = path.join(outDir, 'bin', '64bit');
const engineSource = path.join(root, 'engine', 'afterclip-engine.cpp');
const depsDir = path.join(root, 'vendor', 'obs-deps');
const DEPS_VERSION = '2026-10-23';

// Keep in step with MODULES in afterclip-engine.cpp.
const MODULES = ['image-source', 'obs-ffmpeg', 'obs-filters', 'obs-nvenc', 'obs-outputs', 'obs-qsv11', 'obs-x264', 'win-capture', 'win-dshow', 'win-wasapi'];
// Helpers OBS plugins launch by path from beside the exe, so no import table mentions them.
const HELPERS = ['obs-ffmpeg-mux.exe', 'obs-amf-test.exe', 'obs-nvenc-test.exe', 'obs-qsv-test.exe'];
// Loaded at runtime by name.
const RUNTIME_DLLS = ['libobs-d3d11.dll', 'libobs-winrt.dll'];

function findVcvars() {
  const vswhere = path.join(process.env['ProgramFiles(x86)'], 'Microsoft Visual Studio', 'Installer', 'vswhere.exe');
  const install = execFileSync(vswhere, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath'], { encoding: 'utf8' }).trim();
  if (!install) throw new Error('Visual Studio C++ tools not found');
  return { install, vcvars: path.join(install, 'VC', 'Auxiliary', 'Build', 'vcvars64.bat') };
}

function msvc(vcvars, command) {
  return execSync(`"${vcvars}" >nul 2>nul && ${command}`, { encoding: 'utf8', shell: 'cmd.exe', maxBuffer: 64 * 1024 * 1024, cwd: buildDir });
}

async function fetchHeaders() {
  if (fs.existsSync(path.join(srcDir, 'libobs', 'obs.h'))) return;
  const version = fs.readFileSync(path.join(obsDir, 'afterclip-obs-version.txt'), 'utf8').trim();
  console.log(`Downloading OBS ${version} source for its headers`);
  const res = await fetch(`https://github.com/obsproject/obs-studio/archive/refs/tags/${version}.zip`);
  if (!res.ok) throw new Error(`Source download failed: ${res.status}`);
  const zip = path.join(root, 'vendor', 'obs-src.zip');
  const tmp = path.join(root, 'vendor', 'obs-src-tmp');
  fs.writeFileSync(zip, Buffer.from(await res.arrayBuffer()));
  fs.mkdirSync(tmp, { recursive: true });
  execFileSync('tar', ['-xf', zip, '-C', tmp, `obs-studio-${version}/libobs`]);
  fs.renameSync(path.join(tmp, `obs-studio-${version}`), srcDir);
  fs.rmSync(tmp, { recursive: true, force: true });
  fs.unlinkSync(zip);
}

/** Reads an FFmpeg library's major.minor from its version headers. */
function headerVersion(lib) {
  // Most libraries keep the major version in version_major.h, but not all of them have that file.
  const text = ['version.h', 'version_major.h']
    .map((f) => path.join(depsDir, 'include', lib, f))
    .filter((f) => fs.existsSync(f))
    .map((f) => fs.readFileSync(f, 'utf8'))
    .join('\n');
  const name = lib.replace('lib', '').toUpperCase();
  const major = new RegExp(`LIB${name}_VERSION_MAJOR\\s+(\\d+)`).exec(text)[1];
  const minor = new RegExp(`LIB${name}_VERSION_MINOR\\s+(\\d+)`).exec(text)[1];
  return `${major}.${minor}`;
}

/** OBS's dependency bundle has the FFmpeg headers. The release must match the DLLs that OBS ships. */
async function fetchFfmpegHeaders() {
  if (fs.existsSync(path.join(depsDir, 'include', 'libavcodec', 'avcodec.h'))) return;
  const name = `windows-deps-${DEPS_VERSION}-x64.zip`;
  const releases = await (await fetch(`https://api.github.com/repos/obsproject/obs-deps/releases/tags/${DEPS_VERSION}`)).json();
  const asset = releases.assets?.find((a) => a.name === name);
  if (!asset?.digest) throw new Error(`${name} not found`);
  console.log(`Downloading ${name} for the FFmpeg headers`);
  const zipBytes = Buffer.from(await (await fetch(asset.browser_download_url)).arrayBuffer());
  const sha = crypto.createHash('sha256').update(zipBytes).digest('hex');
  if (asset.digest !== `sha256:${sha}`) throw new Error(`Checksum mismatch on ${name}`);
  fs.mkdirSync(depsDir, { recursive: true });
  const zip = path.join(depsDir, name);
  fs.writeFileSync(zip, zipBytes);
  execFileSync('tar', ['-xf', zip, '-C', depsDir, 'include/libavcodec', 'include/libavformat', 'include/libavutil', 'include/libswscale']);
  fs.unlinkSync(zip);
}

/** Checks the headers against the actual DLLs, because a mismatch would crash at runtime instead of failing here. */
function assertHeadersMatch() {
  for (const [lib, dll] of [['libavcodec', 'avcodec-62.dll'], ['libavformat', 'avformat-62.dll'], ['libavutil', 'avutil-60.dll'], ['libswscale', 'swscale-9.dll']]) {
    const ps = `(Get-Item '${path.join(obsBin, dll)}').VersionInfo.FileVersion`;
    const actual = execFileSync('powershell', ['-NoProfile', '-Command', ps], { encoding: 'utf8' }).trim();
    const headers = headerVersion(lib);
    if (actual && !actual.startsWith(`${headers}.`)) throw new Error(`${lib} headers are ${headers} but ${dll} is ${actual}`);
  }
}

/** The OBS release ships DLLs without import libraries, so generate them from each DLL's exports. */
function importLib(vcvars, dll) {
  const base = path.basename(dll, '.dll');
  const exports = msvc(vcvars, `dumpbin /exports "${path.join(obsBin, dll)}"`);
  const names = [...exports.matchAll(/^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]+\s+(\w+)/gm)].map((m) => m[1]);
  if (!names.length) throw new Error(`Could not read exports from ${dll}`);
  fs.writeFileSync(path.join(buildDir, `${base}.def`), `LIBRARY ${base}\nEXPORTS\n${names.join('\n')}\n`);
  msvc(vcvars, `lib /nologo /def:${base}.def /out:${base}.lib /machine:x64`);
  return `${base}.lib`;
}

function compile(vcvars) {
  fs.mkdirSync(buildDir, { recursive: true });
  // The real obsconfig.h comes from OBS's CMake step. Nothing the engine uses depends on it.
  fs.writeFileSync(path.join(buildDir, 'obsconfig.h'), '#pragma once\n');

  importLib(vcvars, 'obs.dll');
  const mediaLibs = ['avcodec-62.dll', 'avformat-62.dll', 'avutil-60.dll', 'swscale-9.dll'].map((d) => importLib(vcvars, d));
  msvc(vcvars, `cl /nologo /std:c++17 /EHsc /O2 /MT /W3 /D_CRT_SECURE_NO_WARNINGS /I"${path.join(depsDir, 'include')}" "${path.join(root, 'engine', 'afterclip-media.cpp')}" `
    + `/Fe:afterclip-media.exe /link /SUBSYSTEM:WINDOWS /ENTRY:mainCRTStartup ${mediaLibs.join(' ')} shell32.lib`);

  // /MT keeps the engine itself free of the VC++ runtime. /SUBSYSTEM:WINDOWS means no console can ever flash.
  msvc(vcvars, `cl /nologo /std:c++17 /EHsc /O2 /MT /W3 /D_CRT_SECURE_NO_WARNINGS /I"${path.join(srcDir, 'libobs')}" /I"${buildDir}" "${engineSource}" `
    + '/Fe:afterclip-engine.exe /link /SUBSYSTEM:WINDOWS /ENTRY:mainCRTStartup obs.lib ole32.lib user32.lib shell32.lib');
}

function dependenciesOf(vcvars, file) {
  const out = msvc(vcvars, `dumpbin /dependents "${file}"`);
  return [...out.matchAll(/^\s+(\S+\.dll)\s*$/gim)].map((m) => m[1]);
}

function copyDir(from, to) {
  fs.cpSync(from, to, {
    recursive: true,
    // English only: a background engine shows no text, and locales are most of the data size.
    filter: (src) => {
      if (src.endsWith('.pdb')) return false;
      const rel = path.relative(from, src).replace(/\\/g, '/');
      return !/^locale\/(?!en-US)/.test(rel);
    },
  });
}

function assemble(vcvars, install) {
  fs.rmSync(outDir, { recursive: true, force: true });
  fs.mkdirSync(outBin, { recursive: true });
  fs.copyFileSync(path.join(buildDir, 'afterclip-engine.exe'), path.join(outBin, 'afterclip-engine.exe'));
  fs.copyFileSync(path.join(buildDir, 'afterclip-media.exe'), path.join(outBin, 'afterclip-media.exe'));

  for (const module of MODULES) {
    const from = path.join(obsDir, 'core', module);
    const dataFrom = path.join(from, 'data');
    const to = path.join(outDir, 'core', module);
    fs.mkdirSync(to, { recursive: true });
    fs.copyFileSync(path.join(from, `${module}.dll`), path.join(to, `${module}.dll`));
    if (fs.existsSync(dataFrom)) copyDir(dataFrom, path.join(to, 'data'));
  }
  copyDir(path.join(obsDir, 'data', 'libobs'), path.join(outDir, 'data', 'libobs'));
  fs.copyFileSync(path.join(obsDir, 'data', 'obs-studio', 'license', 'gplv2.txt'), path.join(outDir, 'LICENSE-OBS-gplv2.txt'));
  fs.copyFileSync(path.join(obsDir, 'afterclip-obs-version.txt'), path.join(outDir, 'afterclip-obs-version.txt'));

  // Follow real import tables from the engine and every plugin, so only needed DLLs ship.
  const available = new Map(fs.readdirSync(obsBin).map((f) => [f.toLowerCase(), f]));
  const queue = [
    path.join(outBin, 'afterclip-engine.exe'),
    ...MODULES.map((m) => path.join(outDir, 'core', m, `${m}.dll`)),
  ];
  for (const name of [...RUNTIME_DLLS, ...HELPERS]) {
    fs.copyFileSync(path.join(obsBin, name), path.join(outBin, name));
    queue.push(path.join(outBin, name));
  }
  const seen = new Set();
  while (queue.length) {
    for (const dep of dependenciesOf(vcvars, queue.pop())) {
      const real = available.get(dep.toLowerCase());
      if (!real || seen.has(real)) continue;
      seen.add(real);
      fs.copyFileSync(path.join(obsBin, real), path.join(outBin, real));
      queue.push(path.join(outBin, real));
    }
  }

  // OBS's DLLs need the VC++ runtime, which a clean PC may not have. Microsoft allows shipping it app-local.
  const redistRoot = path.join(install, 'VC', 'Redist', 'MSVC');
  const crt = fs.readdirSync(redistRoot)
    .map((v) => path.join(redistRoot, v, 'x64', 'Microsoft.VC143.CRT'))
    .filter((p) => fs.existsSync(p))
    .pop();
  if (!crt) throw new Error('VC++ redistributable not found in Visual Studio');
  for (const f of fs.readdirSync(crt)) {
    if (/^(msvcp140|msvcp140_1|msvcp140_2|vcruntime140|vcruntime140_1)\.dll$/i.test(f)) fs.copyFileSync(path.join(crt, f), path.join(outBin, f));
  }
}

async function main() {
  const { install, vcvars } = findVcvars();
  await fetchHeaders();
  await fetchFfmpegHeaders();
  assertHeadersMatch();
  compile(vcvars);
  assemble(vcvars, install);
  let total = 0;
  const walk = (d) => fs.readdirSync(d, { withFileTypes: true }).forEach((e) => {
    const p = path.join(d, e.name);
    if (e.isDirectory()) walk(p); else total += fs.statSync(p).size;
  });
  walk(outDir);
  console.log(`vendor/engine: ${(total / 1048576).toFixed(1)} MB`);
}

main().catch((e) => { console.error(e.stdout || e.message); process.exit(1); });
