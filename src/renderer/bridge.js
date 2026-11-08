// Connects the UI to Afterclip's backend.
const { core, event } = window.__TAURI__;

window.afterclip = {
  invoke(channel, payload) {
    // Backend errors arrive as plain strings, and the UI reads them as errors.
    return core.invoke(channel.replace(/-/g, '_'), { payload }).catch((e) => { throw new Error(String(e)); });
  },
  on(name, fn) { return event.listen(name, (e) => fn(e.payload)); },
  videoUrl(file) { return core.convertFileSrc(file); },
  // Tells the backend the page has rendered, on the frame after layout, so the window never shows blank.
  ready() { requestAnimationFrame(() => requestAnimationFrame(() => core.invoke('window_ready'))); },
};
