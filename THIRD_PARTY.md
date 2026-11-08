# Third-party software

| Component | How Afterclip uses it | Licence | Source |
| --- | --- | --- | --- |
| libobs and plugins from OBS Studio 33.0.1 | `afterclip-engine.exe` links to them directly | GPL-2.0-or-later | https://github.com/obsproject/obs-studio |
| FFmpeg libraries, as shipped with OBS | Used by OBS's plugins, and linked by `afterclip-media.exe` to trim clips and make thumbnails | GPL-2.0-or-later for this build | https://github.com/obsproject/obs-deps |
| Tauri and its Rust dependencies | The app's window, tray and backend | MIT or Apache-2.0 | https://github.com/tauri-apps/tauri |
| Microsoft Visual C++ runtime | Redistributable DLLs that OBS's libraries need | Microsoft redistribution terms | Visual Studio |
| whisper.cpp 1.8.2 and Whisper tiny | Local transcription of selected audio tracks | MIT | https://github.com/ggml-org/whisper.cpp and https://github.com/openai/whisper |
| FFmpeg 6.1.1 static Windows binary | Separate command-line tool for redacted, captioned and multi-angle exports | GPL v3 for this binary | https://www.gyan.dev/ffmpeg/builds/ and https://github.com/FFmpeg/FFmpeg/commit/e38092ef93 |
| QR Code Generator | Renders temporary phone transfer links locally | MIT | https://github.com/kazuhikoarase/qrcode-generator |

Afterclip itself is licensed under GPL-2.0-or-later: see [LICENSE](LICENSE). Because `afterclip-engine.exe` and `afterclip-media.exe` link to GPL libraries, they're GPL works: distribute `engine/` under the GPL along with them. OBS's licence text ships at `engine/LICENSE-OBS-gplv2.txt`.
