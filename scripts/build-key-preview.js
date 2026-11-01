// Renders the Stream Deck keys to branding/streamdeck-keys-preview.png through Microsoft Edge, for the README.
//   node scripts/build-key-preview.js [output.png]
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { keySvg, confirmSvg } = require('../streamdeck/com.afterclip.clips.sdPlugin/key-art.cjs');

const root = path.join(__dirname, '..');
const output = path.resolve(process.argv[2] ?? path.join(root, 'branding', 'streamdeck-keys-preview.png'));
const faces = [
  keySvg({ big: '15', small: 'SEC' }), keySvg({ big: '30', small: 'SEC' }), keySvg({ big: '1', small: 'MIN' }), keySvg({ big: '5', small: 'MIN' }), keySvg({ big: 'FULL', small: 'BUFFER' }),
  keySvg({ symbol: 'record', small: 'REC' }), keySvg({ symbol: 'stop', small: 'STOP', tone: 'active' }), keySvg({ symbol: 'mic', small: 'MIC ON' }), keySvg({ symbol: 'micMuted', small: 'MUTED', tone: 'active' }), keySvg({ symbol: 'clean', small: 'CLEAN MIC' }),
  keySvg({ symbol: 'marker', small: 'MARKER' }), keySvg({ symbol: 'panel', small: 'OPEN' }), keySvg({ symbol: 'folder', small: 'CLIPS' }), keySvg({ symbol: 'edit', small: 'EDIT LAST' }), keySvg({ symbol: 'copy', small: 'COPY LAST' }),
  keySvg({ big: '1', small: 'SAVING', tone: 'active' }), confirmSvg('SAVED', 0.35), confirmSvg('SAVED', 0.6), confirmSvg('SAVED', 1), keySvg({ symbol: 'alert', small: 'FAILED', tone: 'fail' }),
];
// Each key's gradients share ids, so each stays a separate image to keep them apart.
const cells = faces.map((face) => `<img src="data:image/svg+xml;charset=utf8,${encodeURIComponent(face)}">`).join('');
const page = `<!doctype html><style>html,body{margin:0;background:#000}body{display:grid;grid-template-columns:repeat(5,144px);gap:10px;padding:16px;width:792px;height:638px;box-sizing:border-box}img{width:144px;height:144px;border-radius:16px}</style>${cells}`;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'afterclip-keys-'));
const html = path.join(dir, 'keys.html');
fs.writeFileSync(html, page);
const edge = ['ProgramFiles(x86)', 'ProgramFiles'].map((v) => path.join(process.env[v] ?? '', 'Microsoft', 'Edge', 'Application', 'msedge.exe')).find((p) => fs.existsSync(p));
execFileSync(edge, ['--headless=new', '--disable-gpu', '--hide-scrollbars', `--user-data-dir=${path.join(dir, 'edge')}`, `--screenshot=${output}`, '--window-size=792,638', '--virtual-time-budget=1000', `file:///${html.replaceAll('\\', '/')}`], { stdio: 'ignore' });
fs.rmSync(dir, { recursive: true, force: true });
console.log(output);
