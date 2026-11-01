# Afterclip

Clip game moments with hotkeys or Stream Deck keys, and record full sessions. Works with NVIDIA, AMD and Intel GPUs, or the CPU.

Afterclip doesn't need OBS installed and never starts the OBS app. It has three parts of its own:

- **`afterclip.exe`:** the app, written in Rust with Tauri. Its window uses Windows' built-in WebView2.
- **`afterclip-engine.exe`:** a windowless capture program built on OBS's libobs libraries.
- **`afterclip-media.exe`:** a small helper that trims clips and makes thumbnails, one job per process.

## Build and run

Needs Node, Rust, and Visual Studio with the C++ tools.

```bash
npm install
```

```bash
npm start
```

```bash
npm run dist
```

That builds the installer in `src-tauri/target/release/bundle/nsis/`.

`npm run build:engine` downloads the official OBS release and its FFmpeg headers, verifies their checksums, compiles the engine and media helper against them, and assembles `vendor/engine` with only the files they need (about 55 MB). To change version, edit `OBS_VERSION` in `scripts/fetch-obs.js` and `DEPS_VERSION` in `scripts/build-engine.js`. The build stops if the headers don't match OBS's libraries.

## Default hotkeys

| Keys | Action |
| --- | --- |
| Alt+Shift+1 | Clip last 15s |
| Alt+Shift+2 | Clip last 30s |
| Alt+Shift+3 | Clip last 60s |
| Alt+Shift+4 | Clip whole buffer |
| Alt+Shift+R | Start or stop recording |

You can change these and add more clip lengths in the app. A rising double beep confirms a saved clip while you're in a game.

## Settings

Settings are grouped into **General**, **Buffer**, **Video**, **Audio**, **Hotkeys** and **Storage**, and save as you change them. Buffer, audio and video changes restart capture, which clears the buffer.

Under **Video**, a profile (Low, Medium, High, Full quality) sets resolution, visual quality, frame rate, encoder preset and multipass together, and changing any of them makes the profile Custom. Native follows your primary monitor. If the chosen encoder or codec can't start, Afterclip falls back to one that can and shows which one it's using. HEVC and AV1 give smaller files, but not every app can play them, and the editor's preview may not.

## Using it

Afterclip runs in the tray and opens no window at startup. Click its tray icon for the panel, which shows what's being captured, your clips, and settings. The panel closes when you click away, and frees its memory 20 seconds later. Starting Afterclip again while it's running also opens the panel, including when it's running as administrator.

The panel shows your three newest clips. **View all** opens every clip and recording in a grid, with **Open editor** at the top. Drag any of them into another app, such as Discord, as you would from File Explorer, or choose **Copy file** from its menu and paste it.

The ring on the panel is the replay buffer. Its colour shows how much has filled, and the white part, starting at the arrowhead, is what saving would keep. It turns red while recording.

The tray menu and the panel both open the **editor**. Add clips from the left, drag the handles to trim, drag cards to reorder, and save. Saving is lossless and never changes the originals, so a trim starts on the nearest keyframe (up to 1 second early), and only clips with matching video settings can merge.

## Audio

In Settings, each source is an output device, an input device, or a single app such as Discord. Any source can go in the **main mix** (track 1, which ordinary players use) and on its **own track**, up to 5 own tracks. Own tracks survive trimming and merging. An app that's also playing through a recorded output device is already in that device's audio, so new app sources start out of the main mix to avoid hearing it twice. An app source that isn't running is kept, and starts recording when the app appears.

**Volume and meters:** every source has a slider, from 0 to 200%, that changes straight away without restarting capture, and a live meter that only runs while the Audio section is open.

**Clean Mic** adds noise suppression and a noise gate to the sources ticked **Mute key**, in recordings only. Its gate opens at -32 dB, so a very quiet mic may need its volume raised.

**Recording:** long recordings can split into a new file on a timer. `Alt+Shift+K`, the panel's flag button or the Stream Deck key adds a marker, which becomes a chapter in the recording. Clips don't have markers.

**Performance** shows frame rate, frames lost, the capture engine's CPU and memory, and free disk space, and warns on the panel if recent frames are being lost. It only measures while on screen.

**Mic mute** silences the sources ticked **Mute key** in your recordings and clips, and doesn't affect your mic anywhere else, such as calls. Use `Alt+Shift+M`, the mic button in the panel, or the Stream Deck key. By default the key covers inputs, but not virtual cables, which usually carry apps like Discord. It applies from the moment you press it, so anything already in the buffer keeps your mic.

**Game audio** is on by default. While a game runs, Afterclip records that game's own audio, wherever it's routed, including by a mixer like Voicemeeter or SleekMeeter. It checks about every 3 seconds where the game is playing. If the main mix already records that device, the game stays out of the mix so it isn't heard twice. Changing devices mid-game doesn't restart capture. It can't see inside a mixer's routing: if you record a mixer output like Voicemeeter Out B1 that also carries the game, the game can be heard twice, and turning game audio off fixes that.

## What gets recorded

Afterclip checks the foreground window every second.

1. **No fullscreen app:** it records the primary monitor.
2. **Fullscreen app found:** this covers exclusive and borderless fullscreen. It tries game capture, and records that app's monitor underneath so there's footage during the hook.
3. **Hook works:** the app's picture replaces the monitor, monitor capture turns off to save GPU, and the status shows `Game`.
4. **Hook fails:** after 10 seconds the status shows `Monitor fallback`, and it keeps retrying the hook.
5. **Administrator:** if a game runs elevated, or the hook fails, a banner offers **Restart as administrator**. Restarting clears the buffer, so it only happens when you click it.

**Run as administrator** in General keeps it elevated. Windows asks once, and Afterclip then registers a scheduled task (`Afterclip-Elevated-<your SID>`) so later starts don't prompt. It only does this when installed in Program Files, which is why the installer installs there and asks for permission itself. Turning the setting off removes the task the next time Afterclip runs elevated, and uninstalling always removes it.

Browsers and video players in fullscreen count as normal desktop use. Windowed games aren't detected, so they're only recorded if they're on the primary monitor.

## How it works

- **Windows:** both windows stay hidden until their page reports it has rendered, on a dark background. A window that doesn't report in 4 seconds is reloaded, and anything unusual is logged to `%LOCALAPPDATA%\Afterclip\ui.log`.
- **Engine link:** the app talks to the engine over private stdin/stdout pipes. If the app dies for any reason, the pipes close, and the engine finalizes any recording and exits.
- **Clip lengths:** the engine holds one rolling buffer as long as your setting, kept in RAM. Each clip saves that buffer, then the media helper losslessly cuts the last N seconds, so any length up to the buffer works.
- **Accuracy:** the keyframe interval is 1s, so a clip can run up to about 1s long but never starts late.
- **Record mode:** recording shares the buffer's encoder, so both can run together for no extra encode.
- **Encoder fallback:** it tries your chosen encoder, then every other available one, so a broken driver costs quality instead of the buffer.
- **Files:** saved to `Videos\Afterclip\Clips` and `Videos\Afterclip\Recordings`. Thumbnails are cached in `.thumbs` beside them.
- **Network:** the engine listens on no ports. OBS's game capture plugin connects out to `obsproject.com` to update its game compatibility list.

## Stream Deck

Double-click `streamdeck\dist\com.afterclip.clips.streamDeckPlugin` to install. It adds keys for 15 and 30 seconds, for 1, 2, 3, 5, 10, 15, 20 and 30 minutes, **Save Full Buffer**, **Record**, **Mute Mic In Clips**, and **Save Custom Length**, with the length set per key. Keys show the live state, and show `OFF` when Afterclip isn't running. A clip can't be longer than your buffer, so a 2-minute key with a 1-minute buffer saves 1 minute. The plugin talks to Afterclip through a token-protected API on `127.0.0.1:4467`.

To rebuild the installer, run `npm run plugin`.

## Tests

Build first with `npm run dist`. The end-to-end and capture tests refuse to run while a fullscreen app is in the foreground, because they hook fullscreen apps, take focus and press keys. **Close your game before running them.**

Runs the real Stream Deck plugin against a fake Stream Deck and a fake Afterclip, and is safe to run during a game:

```bash
npm run test:plugin
```

Checks the media helper against ffmpeg:

```bash
npm run test:media
```

Drives the real app on a colour card, so nothing from your screen or microphone is captured:

```bash
npm run test:e2e
```

Records your real desktop to a temp folder, measures only colour, and deletes the clips. It also opens a fullscreen magenta window for about 20 seconds:

```bash
npm run test:capture
```

Drives the real panel and editor, and reads pixels off the screen to catch blank windows. It briefly captures desktop audio and one running app, only to count tracks, then deletes the clips:

```bash
npm run test:ui
```

Set `AFTERCLIP_APP_EXE` to test a specific `afterclip.exe`. Setting `AFTERCLIP_DEBUG_PORT` opens WebView2's debugging port, which only these tests should do. An elevated Afterclip ignores all of these test settings, so run the tests from a normal, non-administrator terminal.

## Licences

See [THIRD_PARTY.md](THIRD_PARTY.md). The engine and media helper link to GPL code, which adds obligations when you distribute Afterclip.
