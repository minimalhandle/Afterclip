// Connects the real plugin to the running Afterclip, with a fake Stream Deck, and presses no keys.
// It only reads status, so it's safe at any time and saves nothing.
//   node src/test/plugin-live-check.js
const { spawn } = require('node:child_process');
const path = require('node:path');
const { WebSocketServer } = require('ws');

async function main() {
  const wss = new WebSocketServer({ port: 0, host: '127.0.0.1' });
  await new Promise((r) => wss.once('listening', r));
  const images = new Map();
  const connected = new Promise((resolve) => wss.on('connection', (ws) => {
    ws.on('message', (raw) => {
      const m = JSON.parse(raw);
      if (m.event === 'setImage') images.set(m.context, decodeURIComponent(m.payload.image));
    });
    resolve(ws);
  }));
  const pluginDir = path.resolve(__dirname, '..', '..', 'streamdeck', 'com.afterclip.clips.sdPlugin');
  const plugin = spawn(process.execPath, ['main.cjs', '-port', wss.address().port, '-pluginUUID', 'live', '-registerEvent', 'registerPlugin', '-info', '{}'], { cwd: pluginDir, stdio: 'inherit' });
  const deck = await connected;
  const keys = { one: 'save-1-min', two: 'save-2-min', full: 'save-full', rec: 'record' };
  for (const [context, action] of Object.entries(keys)) {
    deck.send(JSON.stringify({ event: 'willAppear', action: `com.afterclip.clips.${action}`, context, payload: { settings: {} } }));
  }
  await new Promise((r) => setTimeout(r, 3000));
  let ok = true;
  for (const context of Object.keys(keys)) {
    const labels = [...(images.get(context) ?? '').matchAll(/>([^<]+)</g)].map((m) => m[1]).join(' ');
    console.log(`${keys[context]}: ${labels}`);
    if (!labels || labels.includes('OFF')) ok = false;
  }
  console.log(ok ? 'CONNECTED: keys show live state from the running Afterclip' : 'NOT CONNECTED: keys show OFF');
  plugin.kill();
  wss.close();
  process.exit(ok ? 0 : 1);
}

main();
