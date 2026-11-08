const fs = require('node:fs');
const path = require('node:path');

const runtime = path.resolve(__dirname, '../vendor/studio');
fs.mkdirSync(runtime, { recursive: true });
fs.copyFileSync(require('ffmpeg-static'), path.join(runtime, 'ffmpeg.exe'));
const ffmpegDir = path.dirname(require('ffmpeg-static'));
for (const file of fs.readdirSync(ffmpegDir).filter((name) => /^(LICENSE|ffmpeg\.exe\.(LICENSE|README))/i.test(name))) {
  fs.copyFileSync(path.join(ffmpegDir, file), path.join(runtime, file));
}
fs.copyFileSync(require.resolve('qrcode-generator'), path.resolve(__dirname, '../src/renderer/qrcode.js'));
