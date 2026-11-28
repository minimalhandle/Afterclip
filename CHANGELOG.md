# Patch notes

Afterclip shows the section for its own version after an update, and the release script puts it in the
update itself. Each version starts with `## ` and its number. Lines that start with `- ` show as a list.

## 1.7.0

- Press twice for a longer clip: pressing a clip key again within 10 seconds saves one clip from the first press to the second, instead of two that show the same moment. The shorter clip goes to the Recycle Bin, and the longer one keeps its star and tags. Turn it off in Settings > Video.
- Upload and copy link: in Edit and share, send a clip to your own file host and copy the link it answers with. Set the HTTPS address, and a token if it needs one, in Settings > Clips and sharing. Both are left out of exported settings and diagnostics.
- Settings warns when the buffer would take over a quarter of your PC's memory, which can slow games down.
- View all fills in faster: new thumbnails and scrub previews are made a few at a time when no game is running, and one at a time during a game.
- Cached thumbnails of clips you deleted or renamed in File Explorer are cleaned up by themselves.
- Tools no longer rebuilds its list every second, so its buttons don't flicker under the mouse.
- When the browser behind Afterclip's windows crashes again a few seconds after the first time, as a game overlay can cause, the panel still comes back by itself.
- Capture cards: a console sending HDR records in the right colours with Console sends HDR on, in 10-bit where the card sends it, and Afterclip suggests turning it on when the picture looks washed out.
- Capture cards: the card's sound is recorded through Windows' own device for it when there is one, the panel shows a small live preview of the card's picture, and Settings greys out sizes and frame rates the card can't send.
- Capture cards: a card that wasn't ready when capture started, such as one still starting up, is opened again every 30 seconds until its picture arrives.
- Audio sources whose device got a new id, such as after reinstalling Voicemeeter or VB-Cable, are found again by name. One that's really gone shows a warning instead of recording nothing.
- Failed update checks wait longer after each failure instead of retrying every few minutes while offline.

## 1.6.0

- Lighter on games: the game hook now copies frames only at your capture frame rate, so a game running at 144 or 240 fps no longer pays for frames Afterclip never uses.
- New installs start lighter: Medium quality (1080p at 60 fps, 20 Mbps), which needs about half the memory of High, and Only buffer during games on, so nothing is captured on the desktop. Your current settings stay as they are.
- When memory gets tight during a game, the buffer moves to disk sooner, before Windows starts taking memory from the game.
- Full quality now says it uses the most GPU and can lower your frame rate in games.
- Push-to-talk checks its key half as often, with no difference you can hear.
- Optional and off until you say yes: count yourself as an Afterclip user. Once a day it sends afterclip.app a random ID and the version number, nothing else. Change it any time in Settings > General, under Privacy.
- Capture cards: record a console or a second PC through a capture card, such as an Elgato 4K X, instead of this PC. Turn on Record a capture card in Settings > Video.
- The card has its own resolution, frame rate and quality, up to 4K whatever your monitor is, so your settings for PC games stay as they are.
- Switch between this PC and the card from the tray, a hotkey or the new Capture Card Stream Deck key.
- The card's sound has its own volume, level meter and optional track. Afterclip warns if a mic source is really the card, which would record it twice.
- Clips, recordings and screenshots from the card are named CaptureCard, and go in a CaptureCard folder with game folders on.
- The panel and a popup say when the card sends no picture, usually because another app has it open.
- While the card shows NO SIGNAL for a minute, such as with the console off, the buffer pauses so your graphics card isn't encoding a blank screen. It starts again within a few seconds of a picture arriving. Turn it off in Settings > Video, under the card.
- Webcams and capture cards can be chosen while capture is stopped, such as while waiting for a game.
- With the Stream Deck app installed, Afterclip offers to install its plugin, and to update it when a newer one comes with an update. It's in Settings > Controls too.
- Fixed turning the webcam on when Afterclip started with it off, and turning it off now turns its light off.
- Fixed Send to Discord from Edit and share never finishing.

## 1.5.0

- Clip loud moments: during a game, Afterclip can save a clip by itself when your mic or the game stays loud for a moment, tagged #auto. Turn it on in Settings > Controls, and set how loud with the live meter.
- Named markers: give a hotkey a name, such as clutch. During a recording it adds a chapter with that name, and otherwise it saves a clip tagged with it.
- Make a reel: in View all, filter to your favourites or a tag, then Make a reel opens them in the editor, oldest first, to trim and merge into one video.
- The editor draws how loud each part of a clip is under the trim handles, and Loudest moment jumps to the loudest part.
- Find overlapping clips: in View all, finds clips saved so close together that they show the same moment, and offers to remove the shorter one. Favourites are never suggested.
- The Clip saved popup appears on the monitor you're using, which during a game is the game's monitor, in the corner you choose in Settings > General.
- Thumbnails you've already seen in View all load straight away, instead of waiting for new ones to be made.
- Delete is back in the clip menu, and right-clicking a clip opens the same menu.
- Undo after deleting puts clips back from the Recycle Bin, with their favourite star, game and tags.
- The Delete key deletes the clips you've selected.
- After a game closes, the home page says how many clips that session saved, and Review shows just those.
- Keep tagged clips, in Settings > Clips and sharing, stops the storage limit and cleanup removing clips you've tagged. Clips tagged #auto still get cleaned up.
- Keep the buffer on disk, in Settings > Video: a long buffer is written to your save drive as it goes, so it uses almost no memory. The last 40 seconds stay in memory, so short clips still save at once.
- When memory runs low during a game, Afterclip moves the buffer to disk so the game keeps its memory. Nothing in the buffer is lost, and it goes back into memory when the game closes. Turn this off in Settings > Video.
- HDR, in Settings > Video: while Windows HDR is on, captures HDR games in HDR, as HEVC or AV1. Thumbnails, screenshots and shared copies are turned into SDR, so they never look washed out. On an HDR monitor, the desktop and SDR games are also captured at the brightness Windows shows them.
- A game whose anti-cheat keeps refusing game capture, such as Call of Duty, is no longer retried every 2 seconds all session, which could make it stutter. After a minute, Afterclip records its monitor instead, until the game opens again.
- Audio tracks are named after their sources, such as Microphone or Discord, so video editors show the names instead of Track 2.
- Weekly reel, in Settings > Clips and sharing: once a week, the last 30 seconds of each of the week's favourites, or clips with a tag you choose, are joined into one video tagged #reel.

## 1.4.0

- The clip menu is down to Edit and share, Rename, Tags and Show in folder. Trimming and every video export live in Edit and share.
- Audio Sources shows matching Sleekmeeter channel names when Sleekmeeter is installed.

- Recover recent gameplay from disk checkpoints, import existing clips or watch a folder, and queue batches with saved share recipes.
- Redact video and audio, and export an edit with synchronized angle cuts.
- Share a clip to your phone on Wi-Fi, check audio track routing alongside Voicemeeter labels, and hide protected games when they lose focus.

- Edit and share opens in its own window with video playback and one-click vertical Discord sharing.
- Save share recipes, reframe vertical clips with pinned regions, and switch between synchronized clip angles.

- Trim a clip from its menu and save or share the selected range.
- Even out audio levels and choose which recorded tracks go into share copies.
- Add named Discord channels and pick a destination when sending.
- Save and star a clip in one press with a hotkey or Stream Deck key.
- Hover over a clip in View all to preview it. Keep recording after you save and the library size limit are already available in Settings.

## 1.3.1

- HEVC clips now play on iPhones and Macs, including in Discord. They used to play on PCs only.
- Clips you saved with HEVC before this update are fixed too, in the background, without re-encoding. It waits while a game is open.
- Settings > Video says which devices can play each codec.

## 1.3.0

- Vertical mode: capture in 9:16 all the time, in Settings > Video or for one game. Clips, recordings and screenshots save vertical straight away, with no re-encoding.
- In vertical mode your webcam goes across the top, with the game below. Turn the webcam off and the game fills the whole picture.
- Hover previews show vertical clips properly instead of squashing them.
- What's new: after each update, Afterclip shows what changed. An update that's ready to install shows its notes too, and they're always in Settings > General.

## 1.2.0

- Keep recording after you save, so what happens just after you press is in the clip too.
- Only buffer during games, to free memory and GPU on the desktop.
- Your webcam in any corner of every clip and recording.
- Push to talk: your mic is only in clips while you hold a key or mouse button.
- Tags, hover to preview, and a folder for each game.
- Capture comes back by itself after an encoder error, a crash or a driver update.
- A warning when disk space is low, export and import of settings, and Save diagnostics.
- A Stream Deck on another PC can control Afterclip, plus new keys and Stream Deck + dials.

## 1.1.0

- A cleaner panel, and settings in six sections.
- Send to Discord, Compress, vertical copies and screenshots.
- Controller support, windowed games and game profiles.
- Search, filters, favourites, renaming and deleting several clips at once.
- Storage rules and automatic updates.
- Stream Deck plugin 0.7, with redesigned keys.
