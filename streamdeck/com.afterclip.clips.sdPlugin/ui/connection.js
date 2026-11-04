// The Address part of every key's settings: where the plugin finds Afterclip, shared by all its keys.
// Empty means Afterclip on this PC. The plugin says how the connection is going, and this shows it.
function afterclipConnection(ws, uuid) {
  const box = document.getElementById('connection');
  box.innerHTML = '<label>Address <input id="address" type="password" spellcheck="false" placeholder="Empty for this PC"></label>'
    + '<p><button id="connect" type="button">Connect</button></p>'
    + '<p id="link" role="status"></p>'
    + '<p>To control Afterclip on another PC, turn on Allow a Stream Deck on another PC in its Settings, under Controls, and paste the address it shows. Leave this empty for Afterclip on this PC.</p>';
  const address = document.getElementById('address');
  const shown = document.getElementById('link');
  const say = (text, bad) => { shown.textContent = text; shown.className = bad ? 'bad' : 'good'; };
  const send = (event, payload, context = uuid) => ws.send(JSON.stringify({ event, context, payload }));
  const ask = () => send('sendToPlugin', { ask: 'status' });

  ws.addEventListener('open', () => { send('getGlobalSettings'); ask(); });
  ws.addEventListener('message', ({ data }) => {
    const { event, payload } = JSON.parse(data);
    if (event === 'didReceiveGlobalSettings') address.value = payload.settings?.address ?? '';
    if (event !== 'sendToPropertyInspector') return;
    const where = payload.host ? `at ${payload.host}` : 'on this PC';
    const words = {
      local: ['Connected to Afterclip on this PC.'],
      ok: [`Connected to Afterclip ${where}.`],
      offline: [payload.host
        ? `Can't reach Afterclip ${where}. Check it's running there, that Allow a Stream Deck on another PC is on, and that Windows Firewall lets it in.`
        : 'Afterclip isn\'t running on this PC. To control another PC, paste its address above.', true],
      key: ['Afterclip says the key is wrong. Copy the address from its Settings again.', true],
      bad: ['That address has no key in it. Copy the whole address from Afterclip\'s Settings, under Controls.', true],
    }[payload.link] ?? ['Checking.'];
    say(...words);
  });
  const connect = () => {
    send('setGlobalSettings', { address: address.value.trim() });
    say('Checking.');
    setTimeout(ask, 300);
  };
  document.getElementById('connect').onclick = connect;
  address.addEventListener('keydown', (e) => { if (e.key === 'Enter') connect(); });
}
