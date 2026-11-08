const { execFileSync } = require('node:child_process');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const folder = path.resolve(__dirname, '../vendor/speech');
const runtime = path.join(folder, 'runtime');
const binaries = ['whisper-cli.exe', 'whisper.dll', 'ggml.dll', 'ggml-base.dll', 'ggml-cpu.dll'];
const model = 'ggml-tiny.bin';
const archive = 'whisper-bin-x64.zip';
const hashes = {
  [archive]: 'b1514ebc099765e39fa37eb780b92a140a94c86bb0b3b3d98226b38825979732',
  [model]: 'be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21',
};
function verified(file, expected) {
  return fs.existsSync(file) && createHash('sha256').update(fs.readFileSync(file)).digest('hex') === expected;
}
function download(name, url) {
  const file = path.join(folder, name);
  if (hashes[name] && verified(file, hashes[name])) return;
  execFileSync('curl.exe', ['-fsSL', '--retry', '2', '-o', file, url], { stdio: 'inherit' });
  if (hashes[name] && !verified(file, hashes[name])) throw new Error('Unexpected SHA-256 for ' + name);
}

fs.mkdirSync(runtime, { recursive: true });
download(archive, 'https://github.com/ggml-org/whisper.cpp/releases/download/v1.8.2/' + archive);
download(model, 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/' + model);
if (!fs.existsSync(path.join(folder, 'LICENSE'))) {
  download('LICENSE', 'https://raw.githubusercontent.com/ggml-org/whisper.cpp/v1.8.2/LICENSE');
}
execFileSync('tar.exe', ['-xf', path.join(folder, archive), '-C', runtime, '--strip-components', '1', ...binaries.map((name) => 'Release/' + name)], { stdio: 'inherit' });
for (const name of [model, 'LICENSE']) fs.copyFileSync(path.join(folder, name), path.join(runtime, name));
// The upstream Windows binary uses the same redistributable already supplied for OBS.
const engine = path.resolve(__dirname, '../vendor/engine/bin/64bit');
for (const name of fs.readdirSync(engine).filter((name) => /^(msvcp|vcruntime).*\.dll$/i.test(name))) {
  fs.copyFileSync(path.join(engine, name), path.join(runtime, name));
}
console.log('Offline speech runtime ready');
