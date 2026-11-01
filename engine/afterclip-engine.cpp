// Afterclip's capture engine: a windowless host for libobs, with no OBS app, Qt or network socket.
// Protocol: one JSON object per line. Commands arrive on stdin and get {"id":N,"ok":bool,...} back on
// stdout, and {"event":...} lines report output changes. When stdin closes, which includes the parent
// dying for any reason, the engine finalizes any recording and exits, so it can never be orphaned.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <objbase.h>
#include <shellapi.h>
#include <share.h>

#include <obs.h>
#include <obs-audio-controls.h>
#include <util/platform.h>

#include <atomic>
#include <memory>
#include <tuple>

#include <cstdarg>
#include <cstdio>
#include <deque>
#include <iostream>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace {

const char *const MODULES[] = {"image-source", "obs-ffmpeg", "obs-filters", "obs-nvenc", "obs-outputs",
			       "obs-qsv11",    "obs-x264",   "win-capture", "win-wasapi"};
// Hardware encoders first. Each one only registers on hardware that can run it. Columns: NVIDIA, AMD, Intel, CPU.
const char *const H264_ENCODERS[] = {"obs_nvenc_h264_tex", "h264_texture_amf", "obs_qsv11_v2", "obs_x264"};
const char *const HEVC_ENCODERS[] = {"obs_nvenc_hevc_tex", "h265_texture_amf", "obs_qsv11_hevc", nullptr};
const char *const AV1_ENCODERS[] = {"obs_nvenc_av1_tex", "av1_texture_amf", "obs_qsv11_av1", nullptr};
const size_t ENCODER_COLUMNS = 4;
const char *const SCENE_NAME = "Capture";
const int KEYFRAME_SECONDS = 1;
const int MAIN_AUDIO_KBPS = 320;
const int EXTRA_AUDIO_KBPS = 192;
const int MAX_AUDIO_TRACKS = 6; // What libobs supports, including the main mix.

std::mutex g_writeLock;
FILE *g_log = nullptr;

obs_scene_t *g_scene = nullptr;
obs_source_t *g_display = nullptr, *g_game = nullptr, *g_card = nullptr;
std::vector<obs_source_t *> g_audioSources;
std::vector<obs_source_t *> g_micSources; // The ones the mute key controls. g_audioSources owns them.
std::vector<obs_source_t *> g_sourceByIndex; // By position in the settings list, with null for any that failed.
std::vector<obs_source_t *> g_cleanFilters;
std::vector<obs_source_t *> g_cleanFilterOwners; // The source each filter above sits on.
const int GAME_AUDIO_INDEX = -1;
struct Meter {
	obs_volmeter_t *volmeter = nullptr;
	int index = 0;
	std::atomic<float> peak{-100.0f};
	std::atomic<uint64_t> updatedNs{0};
};
std::vector<std::unique_ptr<Meter>> g_meters;
bool g_metersOn = false;
os_cpu_usage_info_t *g_cpuInfo = nullptr;
std::string g_stagingDir;
obs_source_t *g_gameAudio = nullptr; // Also in g_audioSources, which owns it.
uint32_t g_gameAudioTrackBit = 0;
std::string g_gameAudioWindow;
obs_sceneitem_t *g_displayItem = nullptr, *g_gameItem = nullptr, *g_cardItem = nullptr;
obs_encoder_t *g_video = nullptr;
std::vector<obs_encoder_t *> g_audio; // Index 0 is the main mix. The rest are per-source tracks.
obs_output_t *g_replay = nullptr, *g_record = nullptr;
HANDLE g_recordStopped = nullptr;

void writeLine(obs_data_t *data)
{
	std::lock_guard<std::mutex> lock(g_writeLock);
	const char *json = obs_data_get_json(data);
	std::string line;
	// Jansson pretty-prints, and the protocol is one object per line.
	for (const char *c = json; *c; c++)
		if (*c != '\n' && *c != '\r')
			line += *c;
	line += '\n';
	fwrite(line.data(), 1, line.size(), stdout);
	fflush(stdout);
}

void logHandler(int level, const char *format, va_list args, void *)
{
	if (level > LOG_INFO || !g_log)
		return;
	vfprintf(g_log, format, args);
	fputc('\n', g_log);
	fflush(g_log);
}

void emitOutputEvent(const char *event, bool active, const char *extraKey = nullptr, const char *extra = nullptr,
		     int code = 0)
{
	obs_data_t *d = obs_data_create();
	obs_data_set_string(d, "event", event);
	obs_data_set_bool(d, "active", active);
	obs_data_set_int(d, "code", code);
	if (extraKey)
		obs_data_set_string(d, extraKey, extra ? extra : "");
	writeLine(d);
	obs_data_release(d);
}

void onReplayStart(void *, calldata_t *) { emitOutputEvent("replay", true); }
void onReplayStop(void *, calldata_t *cd) { emitOutputEvent("replay", false, nullptr, nullptr, (int)calldata_int(cd, "code")); }
void onRecordStart(void *, calldata_t *) { emitOutputEvent("record", true); }
void onRecordFileChanged(void *, calldata_t *cd) { emitOutputEvent("recordFile", true, "path", calldata_string(cd, "next_file")); }
void onRecordStop(void *, calldata_t *cd)
{
	emitOutputEvent("record", false, nullptr, nullptr, (int)calldata_int(cd, "code"));
	SetEvent(g_recordStopped);
}

void onReplaySaved(void *, calldata_t *)
{
	calldata_t cd;
	calldata_init(&cd);
	proc_handler_call(obs_output_get_proc_handler(g_replay), "get_last_replay", &cd);
	const char *path = calldata_string(&cd, "path");
	emitOutputEvent("saved", true, "path", path);
	calldata_free(&cd);
}

bool loadModules(const std::string &root)
{
	bool any = false;
	for (const char *name : MODULES) {
		const std::string dir = root + "/core/" + name;
		const std::string dll = dir + "/" + name + ".dll";
		const std::string data = dir + "/data";
		obs_module_t *module = nullptr;
		if (obs_open_module(&module, dll.c_str(), data.c_str()) != MODULE_SUCCESS || !obs_init_module(module)) {
			blog(LOG_WARNING, "Could not load module %s", name);
			continue;
		}
		any = true;
	}
	obs_post_load_modules();
	return any;
}

void fitToCanvas(obs_sceneitem_t *item, uint32_t width, uint32_t height)
{
	vec2 pos = {};
	vec2 bounds;
	vec2_set(&bounds, (float)width, (float)height);
	obs_sceneitem_set_alignment(item, OBS_ALIGN_LEFT | OBS_ALIGN_TOP);
	obs_sceneitem_set_pos(item, &pos);
	obs_sceneitem_set_bounds_type(item, OBS_BOUNDS_SCALE_INNER);
	obs_sceneitem_set_bounds_alignment(item, OBS_ALIGN_CENTER);
	obs_sceneitem_set_bounds(item, &bounds);
}

obs_source_t *createSource(const char *kind, const char *name, obs_data_t *settings = nullptr)
{
	return obs_source_create(kind, name, settings, nullptr);
}

/** Aims display capture at the monitor whose origin is x,y, or at the primary monitor when hasOrigin is false. */
bool aimDisplay(bool hasOrigin, int x, int y, std::string &error)
{
	obs_properties_t *props = obs_source_properties(g_display);
	obs_property_t *list = obs_properties_get(props, "monitor_id");
	const size_t count = list ? obs_property_list_item_count(list) : 0;
	const std::string origin = "@ " + std::to_string(x) + "," + std::to_string(y);
	std::string exact, primary, zero, first;
	for (size_t i = 0; i < count; i++) {
		const char *id = obs_property_list_item_string(list, i);
		const char *rawName = obs_property_list_item_name(list, i);
		// OBS lists a placeholder with the id DUMMY. Selecting it records a black picture.
		if (!id || !*id || strcmp(id, "DUMMY") == 0)
			continue;
		const std::string name = rawName ? rawName : "";
		if (first.empty())
			first = id;
		if (hasOrigin && exact.empty() && name.find(origin) != std::string::npos)
			exact = id;
		if (primary.empty() && name.find("Primary") != std::string::npos)
			primary = id;
		if (zero.empty() && name.find("@ 0,0") != std::string::npos)
			zero = id;
	}
	obs_properties_destroy(props);
	const std::string &chosen = !exact.empty() ? exact : !primary.empty() ? primary : !zero.empty() ? zero : first;
	if (chosen.empty()) {
		error = "OBS found no monitor to capture";
		return false;
	}
	obs_data_t *s = obs_data_create();
	obs_data_set_string(s, "monitor_id", chosen.c_str());
	obs_source_update(g_display, s);
	obs_data_release(s);
	return true;
}

/** Turns a p1 (fastest) to p7 (best) preset into each encoder's own terms. Only NVENC has all seven steps. */
obs_data_t *encoderSettings(const char *id, int bitrate, const std::string &preset, bool multipass)
{
	static const char *const X264_PRESETS[] = {"ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow"};
	const int step = preset.size() == 2 && preset[1] >= '1' && preset[1] <= '7' ? preset[1] - '1' : 2;
	const char *const coarse = step < 2 ? "speed" : step < 5 ? "balanced" : "quality";

	obs_data_t *s = obs_data_create();
	obs_data_set_string(s, "rate_control", "CBR");
	obs_data_set_int(s, "bitrate", bitrate);
	obs_data_set_int(s, "keyint_sec", KEYFRAME_SECONDS);
	if (strstr(id, "nvenc")) {
		obs_data_set_string(s, "preset", preset.c_str());
		obs_data_set_string(s, "tune", "hq");
		obs_data_set_string(s, "multipass", multipass ? "qres" : "disabled");
	} else if (strstr(id, "amf")) {
		obs_data_set_string(s, "preset", coarse);
	} else if (strstr(id, "qsv")) {
		obs_data_set_string(s, "target_usage", coarse);
	} else {
		obs_data_set_string(s, "preset", X264_PRESETS[step]);
	}
	return s;
}

bool encoderRegistered(const char *wanted)
{
	const char *id;
	for (size_t i = 0; obs_enum_encoder_types(i, &id); i++)
		if (strcmp(id, wanted) == 0)
			return true;
	return false;
}

/**
 * Tries the chosen encoder and codec first, then that codec on other hardware, then H.264 on anything.
 * A broken driver or an unsupported codec costs quality, not the buffer. Returns the one that started.
 */
const char *startReplayWithEncoder(const std::string &vendor, const std::string &codec, int bitrate, const std::string &preset, bool multipass)
{
	const char *const *wanted = codec == "av1" ? AV1_ENCODERS : codec == "hevc" ? HEVC_ENCODERS : H264_ENCODERS;
	const size_t column = vendor == "nvenc" ? 0 : vendor == "amd" ? 1 : vendor == "qsv" ? 2 : vendor == "x264" ? 3 : ENCODER_COLUMNS;

	std::vector<const char *> order;
	const auto add = [&order](const char *id) {
		if (id)
			order.push_back(id);
	};
	if (column < ENCODER_COLUMNS) {
		add(wanted[column]);
		add(H264_ENCODERS[column]);
	}
	for (size_t i = 0; i < ENCODER_COLUMNS; i++)
		add(wanted[i]);
	for (size_t i = 0; i < ENCODER_COLUMNS; i++)
		add(H264_ENCODERS[i]);

	for (const char *id : order) {
		if (!encoderRegistered(id))
			continue;
		obs_data_t *s = encoderSettings(id, bitrate, preset, multipass);
		obs_encoder_t *encoder = obs_video_encoder_create(id, "video", s, nullptr);
		obs_data_release(s);
		if (!encoder)
			continue;
		obs_encoder_set_video(encoder, obs_get_video());
		obs_output_set_video_encoder(g_replay, encoder);
		if (obs_output_start(g_replay)) {
			g_video = encoder;
			return id;
		}
		blog(LOG_WARNING, "Encoder %s could not start: %s", id, obs_output_get_last_error(g_replay));
		obs_output_set_video_encoder(g_replay, nullptr);
		obs_encoder_release(encoder);
	}
	return nullptr;
}

/**
 * Clean Mic: RNNoise takes out steady noise, then a gate closes between words to take out keyboard sound.
 * Both always exist on a mic, and switching them on and off doesn't interrupt recording.
 */
void addCleanMicFilters(obs_source_t *mic, bool enabled)
{
	obs_data_t *suppress = obs_data_create();
	obs_data_set_string(suppress, "method", "rnnoise");
	obs_data_t *gate = obs_data_create();
	// Lower than OBS's -26 dB default, so a quiet mic doesn't lose the start of words.
	obs_data_set_double(gate, "open_threshold", -32.0);
	obs_data_set_double(gate, "close_threshold", -40.0);
	obs_data_set_int(gate, "attack_time", 10);
	obs_data_set_int(gate, "hold_time", 200);
	obs_data_set_int(gate, "release_time", 120);
	for (auto [kind, name, settings] : {std::tuple{"noise_suppress_filter", "Clean Mic Noise", suppress},
					    std::tuple{"noise_gate_filter", "Clean Mic Gate", gate}}) {
		obs_source_t *filter = obs_source_create_private(kind, name, settings);
		obs_data_release(settings);
		if (!filter) {
			blog(LOG_WARNING, "Could not create filter %s", kind);
			continue;
		}
		obs_source_filter_add(mic, filter);
		obs_source_set_enabled(filter, enabled);
		g_cleanFilters.push_back(filter);
		g_cleanFilterOwners.push_back(mic);
	}
}

void onMeter(void *param, const float[MAX_AUDIO_CHANNELS], const float peak[MAX_AUDIO_CHANNELS], const float[MAX_AUDIO_CHANNELS])
{
	Meter *meter = static_cast<Meter *>(param);
	meter->peak.store(peak[0] > peak[1] ? peak[0] : peak[1]);
	meter->updatedNs.store(os_gettime_ns());
}

/** Meters only attach while someone is looking at them, so they cost nothing during play. */
void setMeters(bool on)
{
	if (on == g_metersOn)
		return;
	g_metersOn = on;
	if (!on) {
		for (auto &meter : g_meters) {
			obs_volmeter_remove_callback(meter->volmeter, onMeter, meter.get());
			obs_volmeter_destroy(meter->volmeter);
		}
		g_meters.clear();
		return;
	}
	auto add = [](obs_source_t *source, int index) {
		auto meter = std::make_unique<Meter>();
		meter->index = index;
		meter->volmeter = obs_volmeter_create(OBS_FADER_LOG);
		obs_volmeter_add_callback(meter->volmeter, onMeter, meter.get());
		obs_volmeter_attach_source(meter->volmeter, source);
		g_meters.push_back(std::move(meter));
	};
	for (size_t i = 0; i < g_sourceByIndex.size(); i++)
		if (g_sourceByIndex[i])
			add(g_sourceByIndex[i], (int)i);
	if (g_gameAudio)
		add(g_gameAudio, GAME_AUDIO_INDEX);
}

void addStats(obs_data_t *reply)
{
	video_t *video = obs_get_video();
	obs_data_set_double(reply, "fps", obs_get_active_fps());
	obs_data_set_int(reply, "renderTotal", obs_get_total_frames());
	obs_data_set_int(reply, "renderLagged", obs_get_lagged_frames());
	obs_data_set_int(reply, "encoderTotal", video ? video_output_get_total_frames(video) : 0);
	obs_data_set_int(reply, "encoderSkipped", video ? video_output_get_skipped_frames(video) : 0);
	obs_data_set_double(reply, "cpu", g_cpuInfo ? os_cpu_usage_info_query(g_cpuInfo) : 0.0);
	obs_data_set_int(reply, "memoryMb", (long long)(os_get_proc_resident_size() / (1024 * 1024)));
	obs_data_set_int(reply, "diskFreeMb", (long long)(os_get_free_disk_space(g_stagingDir.c_str()) / (1024 * 1024)));
}

/**
 * Creates each configured audio source and routes it. Track 1 is always the main mix, so ordinary players
 * hear everything, and a source with its own track also goes to the next free track. Returns the track count.
 */
int createAudioSources(obs_data_t *c)
{
	int tracks = 1;
	obs_data_array_t *list = obs_data_get_array(c, "audio");
	const size_t count = list ? obs_data_array_count(list) : 0;
	for (size_t i = 0; i < count; i++) {
		obs_data_t *item = obs_data_array_item(list, i);
		const std::string kind = obs_data_get_string(item, "kind");
		const std::string device = obs_data_get_string(item, "device");
		obs_data_t *s = obs_data_create();
		const char *id = "wasapi_output_capture";
		if (kind == "app") {
			id = "wasapi_process_output_capture";
			obs_data_set_string(s, "window", device.c_str());
			obs_data_set_int(s, "priority", 2); // Match by exe, so a changing window title keeps capturing.
		} else {
			if (kind == "input")
				id = "wasapi_input_capture";
			obs_data_set_string(s, "device_id", device.empty() ? "default" : device.c_str());
		}
		const std::string name = "Audio " + std::to_string(i + 1);
		obs_source_t *source = createSource(id, name.c_str(), s);
		obs_data_release(s);
		if (source) {
			uint32_t mixers = obs_data_get_bool(item, "mix") ? 1 : 0;
			if (obs_data_get_bool(item, "ownTrack") && tracks < MAX_AUDIO_TRACKS)
				mixers |= 1u << tracks++;
			obs_source_set_audio_mixers(source, mixers);
			obs_source_set_volume(source, (float)obs_data_get_double(item, "volume"));
			if (obs_data_get_bool(item, "muteKey")) {
				g_micSources.push_back(source);
				obs_source_set_muted(source, obs_data_get_bool(c, "micMuted"));
				addCleanMicFilters(source, obs_data_get_bool(c, "cleanMic"));
			}
			g_audioSources.push_back(source);
			obs_set_output_source((uint32_t)g_audioSources.size(), source);
		} else {
			blog(LOG_WARNING, "Could not create audio source %s", name.c_str());
		}
		g_sourceByIndex.push_back(source); // Keeps UI indexes lined up even if one failed.
		obs_data_release(item);
	}
	obs_data_array_release(list);

	// Silent until the game is known. It records one process, so it works wherever that process is routed.
	if (obs_data_get_bool(c, "gameAudio")) {
		obs_source_t *source = createSource("wasapi_process_output_capture", "Game Audio");
		if (source) {
			if (obs_data_get_bool(c, "gameAudioOwnTrack") && tracks < MAX_AUDIO_TRACKS)
				g_gameAudioTrackBit = 1u << tracks++;
			obs_source_set_audio_mixers(source, 0);
			obs_source_set_volume(source, (float)obs_data_get_double(c, "gameAudioVolume"));
			g_gameAudio = source;
			g_audioSources.push_back(source);
			obs_set_output_source((uint32_t)g_audioSources.size(), source);
		}
	}
	return tracks;
}

/** Adds one source type's choices to a reply, as an array of {id, name}. */
void listChoices(obs_data_t *reply, const char *key, const char *sourceId, const char *property)
{
	obs_data_array_t *array = obs_data_array_create();
	obs_properties_t *props = obs_get_source_properties(sourceId);
	obs_property_t *list = props ? obs_properties_get(props, property) : nullptr;
	const size_t count = list ? obs_property_list_item_count(list) : 0;
	for (size_t i = 0; i < count; i++) {
		const char *id = obs_property_list_item_string(list, i);
		const char *name = obs_property_list_item_name(list, i);
		if (!id || !*id)
			continue;
		obs_data_t *item = obs_data_create();
		obs_data_set_string(item, "id", id);
		obs_data_set_string(item, "name", name ? name : id);
		obs_data_array_push_back(array, item);
		obs_data_release(item);
	}
	obs_properties_destroy(props);
	obs_data_set_array(reply, key, array);
	obs_data_array_release(array);
}

bool cmdStart(obs_data_t *c, obs_data_t *reply, std::string &error)
{
	const uint32_t width = (uint32_t)obs_data_get_int(c, "width");
	const uint32_t height = (uint32_t)obs_data_get_int(c, "height");
	const int bitrate = (int)obs_data_get_int(c, "bitrateKbps");
	const bool testPattern = obs_data_get_bool(c, "testPattern");

	obs_video_info ovi = {};
	ovi.graphics_module = "libobs-d3d11";
	ovi.fps_num = (uint32_t)obs_data_get_int(c, "fps");
	ovi.fps_den = 1;
	ovi.base_width = ovi.output_width = width;
	ovi.base_height = ovi.output_height = height;
	ovi.output_format = VIDEO_FORMAT_NV12;
	ovi.colorspace = VIDEO_CS_709;
	ovi.range = VIDEO_RANGE_PARTIAL;
	ovi.scale_type = OBS_SCALE_BICUBIC;
	ovi.gpu_conversion = true;
	ovi.adapter = 0;
	const int video = obs_reset_video(&ovi);
	if (video != OBS_VIDEO_SUCCESS) {
		error = "Could not start video on this GPU (code " + std::to_string(video) + ")";
		return false;
	}
	obs_audio_info2 oai = {};
	oai.samples_per_sec = 48000;
	oai.speakers = SPEAKERS_STEREO;
	if (!obs_reset_audio2(&oai)) {
		error = "Could not start audio";
		return false;
	}
	if (!loadModules(obs_data_get_string(c, "root"))) {
		error = "Could not load capture modules";
		return false;
	}

	g_scene = obs_scene_create(SCENE_NAME);
	obs_set_output_source(0, obs_scene_get_source(g_scene));
	if (testPattern) {
		obs_data_t *s = obs_data_create();
		obs_data_set_int(s, "color", 0xff2a8c3f);
		obs_data_set_int(s, "width", width);
		obs_data_set_int(s, "height", height);
		g_card = createSource("color_source_v3", "Test Pattern", s);
		obs_data_release(s);
		g_cardItem = obs_scene_add(g_scene, g_card);
	} else {
		// Order matters: the game layer sits over the display, and is empty until it hooks.
		g_display = createSource("monitor_capture", "Display");
		g_displayItem = obs_scene_add(g_scene, g_display);
		fitToCanvas(g_displayItem, width, height);
		if (!aimDisplay(false, 0, 0, error))
			return false;

		obs_data_t *gs = obs_data_create();
		obs_data_set_string(gs, "capture_mode", "window");
		obs_data_set_bool(gs, "capture_cursor", true);
		obs_data_set_bool(gs, "anti_cheat_hook", true);
		g_game = createSource("game_capture", "Game", gs);
		obs_data_release(gs);
		g_gameItem = obs_scene_add(g_scene, g_game);
		fitToCanvas(g_gameItem, width, height);
		obs_sceneitem_set_visible(g_gameItem, false);
	}

	g_stagingDir = obs_data_get_string(c, "stagingDir");
	g_cpuInfo = os_cpu_usage_info_start();
	const int tracks = createAudioSources(c);
	for (int track = 0; track < tracks; track++) {
		obs_data_t *as = obs_data_create();
		obs_data_set_int(as, "bitrate", track == 0 ? MAIN_AUDIO_KBPS : EXTRA_AUDIO_KBPS);
		const std::string name = "audio" + std::to_string(track);
		obs_encoder_t *encoder = obs_audio_encoder_create("ffmpeg_aac", name.c_str(), as, track, nullptr);
		obs_data_release(as);
		if (!encoder) {
			error = "Could not create the audio encoder";
			return false;
		}
		obs_encoder_set_audio(encoder, obs_get_audio());
		g_audio.push_back(encoder);
	}
	obs_data_set_int(reply, "audioTracks", tracks);

	obs_data_t *rs = obs_data_create();
	obs_data_set_string(rs, "directory", obs_data_get_string(c, "stagingDir"));
	obs_data_set_string(rs, "format", "Replay %CCYY-%MM-%DD %hh-%mm-%ss");
	obs_data_set_string(rs, "extension", "mp4");
	obs_data_set_bool(rs, "allow_spaces", true);
	obs_data_set_int(rs, "max_time_sec", obs_data_get_int(c, "bufferSeconds"));
	obs_data_set_int(rs, "max_size_mb", obs_data_get_int(c, "bufferMaxMb"));
	g_replay = obs_output_create("replay_buffer", "Replay Buffer", rs, nullptr);
	obs_data_release(rs);
	if (!g_replay) {
		error = "Could not create the replay buffer";
		return false;
	}
	for (size_t i = 0; i < g_audio.size(); i++)
		obs_output_set_audio_encoder(g_replay, g_audio[i], i);
	signal_handler_t *sh = obs_output_get_signal_handler(g_replay);
	signal_handler_connect(sh, "start", onReplayStart, nullptr);
	signal_handler_connect(sh, "stop", onReplayStop, nullptr);
	signal_handler_connect(sh, "saved", onReplaySaved, nullptr);

	const char *encoder = startReplayWithEncoder(obs_data_get_string(c, "encoder"), obs_data_get_string(c, "codec"), bitrate, obs_data_get_string(c, "preset"), obs_data_get_bool(c, "multipass"));
	if (!encoder) {
		error = "No video encoder could start";
		return false;
	}
	obs_data_set_string(reply, "encoder", encoder);
	return true;
}

bool cmdRecordStart(obs_data_t *c, std::string &error)
{
	if (!g_record) {
		g_record = obs_output_create("mp4_output", "Recording", nullptr, nullptr);
		if (!g_record) {
			error = "Could not create the recording output";
			return false;
		}
		signal_handler_t *sh = obs_output_get_signal_handler(g_record);
		signal_handler_connect(sh, "start", onRecordStart, nullptr);
		signal_handler_connect(sh, "stop", onRecordStop, nullptr);
		signal_handler_connect(sh, "file_changed", onRecordFileChanged, nullptr);
	}
	obs_data_t *s = obs_data_create();
	obs_data_set_string(s, "path", obs_data_get_string(c, "path"));
	// The output names later parts itself from these, when splitting.
	const long long splitSeconds = obs_data_get_int(c, "splitSeconds");
	obs_data_set_bool(s, "split_file", splitSeconds > 0);
	obs_data_set_int(s, "max_time_sec", splitSeconds);
	obs_data_set_string(s, "directory", obs_data_get_string(c, "directory"));
	obs_data_set_string(s, "format", obs_data_get_string(c, "format"));
	obs_data_set_string(s, "extension", "mp4");
	obs_data_set_bool(s, "allow_spaces", true);
	obs_output_update(g_record, s);
	obs_data_release(s);
	// Sharing the buffer's encoders is what makes recording cost no extra encode.
	obs_output_set_video_encoder(g_record, g_video);
	for (size_t i = 0; i < g_audio.size(); i++)
		obs_output_set_audio_encoder(g_record, g_audio[i], i);
	ResetEvent(g_recordStopped);
	if (!obs_output_start(g_record)) {
		const char *why = obs_output_get_last_error(g_record);
		error = why ? why : "Recording could not start";
		return false;
	}
	return true;
}

void shutdownAll()
{
	setMeters(false);
	// Filters come off before anything is released, or shutdown destroys them twice.
	for (size_t i = 0; i < g_cleanFilters.size(); i++) {
		obs_source_filter_remove(g_cleanFilterOwners[i], g_cleanFilters[i]);
		obs_source_release(g_cleanFilters[i]);
	}
	if (g_cpuInfo)
		os_cpu_usage_info_destroy(g_cpuInfo);
	if (g_record && obs_output_active(g_record)) {
		obs_output_stop(g_record);
		WaitForSingleObject(g_recordStopped, 8000);
	}
	if (g_replay && obs_output_active(g_replay))
		obs_output_force_stop(g_replay);
	obs_set_output_source(0, nullptr);
	for (size_t i = 0; i < g_audioSources.size(); i++)
		obs_set_output_source((uint32_t)i + 1, nullptr);
	if (g_displayItem)
		obs_sceneitem_remove(g_displayItem);
	if (g_gameItem)
		obs_sceneitem_remove(g_gameItem);
	if (g_cardItem)
		obs_sceneitem_remove(g_cardItem);
	obs_output_release(g_record);
	obs_output_release(g_replay);
	obs_encoder_release(g_video);
	for (obs_encoder_t *encoder : g_audio)
		obs_encoder_release(encoder);
	obs_source_release(g_display);
	obs_source_release(g_game);
	for (obs_source_t *source : g_audioSources)
		obs_source_release(source);
	obs_source_release(g_card);
	obs_scene_release(g_scene);
	// Sources are destroyed on a background queue, and shutting down before it drains destroys them twice.
	obs_wait_for_destroy_queue();
	obs_shutdown();
}

/** One frame on its way out of the video thread. It only copies there, so the disk can't hold up capture. */
struct ScreenshotFrame {
	HANDLE ready = nullptr;
	bool taken = false;
	uint32_t width = 0, height = 0;
	std::vector<uint8_t> nv12;
};

void onScreenshotFrame(void *param, video_data *frame)
{
	ScreenshotFrame *shot = static_cast<ScreenshotFrame *>(param);
	if (shot->taken)
		return;
	const uint32_t width = shot->width, height = shot->height;
	shot->nv12.resize((size_t)width * height * 3 / 2);
	uint8_t *to = shot->nv12.data();
	for (uint32_t y = 0; y < height; y++, to += width)
		memcpy(to, frame->data[0] + (size_t)y * frame->linesize[0], width);
	for (uint32_t y = 0; y < height / 2; y++, to += width)
		memcpy(to, frame->data[1] + (size_t)y * frame->linesize[1], width);
	shot->taken = true;
	SetEvent(shot->ready);
}

/** Saves the next frame as raw NV12. Asking for frames only lasts for this one, so play pays nothing for it. */
bool cmdScreenshot(obs_data_t *c, obs_data_t *reply, std::string &error)
{
	const DWORD WAIT_MS = 3000;
	obs_video_info ovi = {};
	if (!obs_get_video_info(&ovi) || !g_replay) {
		error = "Capture is not running";
		return false;
	}
	ScreenshotFrame shot;
	shot.ready = CreateEventW(nullptr, TRUE, FALSE, nullptr);
	shot.width = ovi.output_width;
	shot.height = ovi.output_height;
	obs_add_raw_video_callback(nullptr, onScreenshotFrame, &shot);
	const bool arrived = WaitForSingleObject(shot.ready, WAIT_MS) == WAIT_OBJECT_0;
	obs_remove_raw_video_callback(onScreenshotFrame, &shot);
	CloseHandle(shot.ready);
	if (!arrived) {
		error = "Capture did not send a picture";
		return false;
	}
	// Opened here and not through libobs, because a file can't cross between two C runtimes.
	const char *path = obs_data_get_string(c, "path");
	std::wstring widePath(MultiByteToWideChar(CP_UTF8, 0, path, -1, nullptr, 0), L'\0');
	MultiByteToWideChar(CP_UTF8, 0, path, -1, widePath.data(), (int)widePath.size());
	FILE *file = _wfopen(widePath.c_str(), L"wb");
	const bool written = file && fwrite(shot.nv12.data(), 1, shot.nv12.size(), file) == shot.nv12.size();
	if (file)
		fclose(file);
	if (!written) {
		error = "Could not save the screenshot";
		return false;
	}
	obs_data_set_int(reply, "width", shot.width);
	obs_data_set_int(reply, "height", shot.height);
	return true;
}

/** Runs one command. Returns false when the engine should exit. */
bool handle(const std::string &line)
{
	obs_data_t *c = obs_data_create_from_json(line.c_str());
	if (!c)
		return true;
	const std::string cmd = obs_data_get_string(c, "cmd");
	obs_data_t *reply = obs_data_create();
	obs_data_set_int(reply, "id", obs_data_get_int(c, "id"));
	std::string error;
	bool ok = true;
	bool keepRunning = true;

	if (cmd == "start") {
		ok = cmdStart(c, reply, error);
	} else if (cmd == "shutdown") {
		keepRunning = false;
	} else if (cmd == "audioChoices") {
		listChoices(reply, "outputs", "wasapi_output_capture", "device_id");
		listChoices(reply, "inputs", "wasapi_input_capture", "device_id");
		listChoices(reply, "apps", "wasapi_process_output_capture", "window");
	} else if (cmd == "save") {
		calldata_t cd;
		calldata_init(&cd);
		ok = g_replay && obs_output_active(g_replay) &&
		     proc_handler_call(obs_output_get_proc_handler(g_replay), "save", &cd);
		calldata_free(&cd);
		if (!ok)
			error = "Replay buffer is not running";
	} else if (cmd == "screenshot") {
		ok = cmdScreenshot(c, reply, error);
	} else if (cmd == "recordStart") {
		ok = cmdRecordStart(c, error);
	} else if (cmd == "recordStop") {
		if (g_record)
			obs_output_stop(g_record);
	} else if (cmd == "display") {
		const bool hasOrigin = obs_data_get_bool(c, "hasOrigin");
		ok = !g_display || aimDisplay(hasOrigin, (int)obs_data_get_int(c, "x"), (int)obs_data_get_int(c, "y"), error);
	} else if (cmd == "displayVisible") {
		if (g_displayItem)
			obs_sceneitem_set_visible(g_displayItem, obs_data_get_bool(c, "visible"));
	} else if (cmd == "game") {
		if (g_game) {
			obs_data_t *s = obs_data_create();
			obs_data_set_string(s, "capture_mode", "window");
			obs_data_set_string(s, "window", obs_data_get_string(c, "window"));
			obs_data_set_int(s, "priority", 2); // Match by exe, so a changing window title stays hooked.
			obs_data_set_bool(s, "capture_cursor", true);
			obs_data_set_bool(s, "anti_cheat_hook", true);
			obs_source_update(g_game, s);
			obs_data_release(s);
			obs_sceneitem_set_visible(g_gameItem, true);
		}
	} else if (cmd == "cleanMic") {
		const bool on = obs_data_get_bool(c, "on");
		for (obs_source_t *filter : g_cleanFilters)
			obs_source_set_enabled(filter, on);
		blog(LOG_INFO, "Clean Mic %s on %d filter(s)", on ? "on" : "off", (int)g_cleanFilters.size());
		obs_data_set_int(reply, "filters", (long long)g_cleanFilters.size());
	} else if (cmd == "volume") {
		const int index = (int)obs_data_get_int(c, "index");
		obs_source_t *source = index == GAME_AUDIO_INDEX ? g_gameAudio
				       : index >= 0 && index < (int)g_sourceByIndex.size() ? g_sourceByIndex[index]
											   : nullptr;
		if (source)
			obs_source_set_volume(source, (float)obs_data_get_double(c, "volume"));
	} else if (cmd == "meters") {
		setMeters(obs_data_get_bool(c, "on"));
	} else if (cmd == "levels") {
		const uint64_t now = os_gettime_ns();
		obs_data_array_t *levels = obs_data_array_create();
		for (auto &meter : g_meters) {
			obs_data_t *item = obs_data_create();
			obs_data_set_int(item, "index", meter->index);
			// No update for a moment means no audio is arriving. Peak can also be -inf, which JSON can't carry.
			const bool fresh = now - meter->updatedNs.load() < 400000000ULL;
			const float peak = meter->peak.load();
			obs_data_set_double(item, "db", fresh && peak > -100.0f ? peak : -100.0);
			obs_data_array_push_back(levels, item);
			obs_data_release(item);
		}
		obs_data_set_array(reply, "levels", levels);
		obs_data_array_release(levels);
	} else if (cmd == "stats") {
		addStats(reply);
	} else if (cmd == "marker") {
		ok = g_record && obs_output_active(g_record);
		if (ok) {
			calldata_t cd;
			calldata_init(&cd);
			calldata_set_string(&cd, "chapter_name", obs_data_get_string(c, "name"));
			ok = proc_handler_call(obs_output_get_proc_handler(g_record), "add_chapter", &cd);
			calldata_free(&cd);
		}
		if (!ok)
			error = "Start a recording to add a marker";
	} else if (cmd == "micMute") {
		const bool muted = obs_data_get_bool(c, "muted");
		for (obs_source_t *source : g_micSources)
			obs_source_set_muted(source, muted);
		blog(LOG_INFO, "Mic mute %s on %d source(s)", muted ? "on" : "off", (int)g_micSources.size());
		obs_data_set_int(reply, "sources", (long long)g_micSources.size());
	} else if (cmd == "gameAudio") {
		if (g_gameAudio) {
			const std::string window = obs_data_get_string(c, "window");
			// Only a new window restarts the capture, so changing the mix alone can't drop any sound.
			if (window != g_gameAudioWindow) {
				g_gameAudioWindow = window;
				obs_data_t *s = obs_data_create();
				obs_data_set_string(s, "window", window.c_str());
				obs_data_set_int(s, "priority", 2); // Match by exe, so a changing window title keeps capturing.
				obs_source_update(g_gameAudio, s);
				obs_data_release(s);
			}
			const uint32_t mix = obs_data_get_bool(c, "mix") ? 1u : 0u;
			obs_source_set_audio_mixers(g_gameAudio, window.empty() ? 0u : (mix | g_gameAudioTrackBit));
		}
	} else if (cmd == "gameOff") {
		if (g_gameItem)
			obs_sceneitem_set_visible(g_gameItem, false);
	} else if (cmd == "hooked") {
		// An unhooked game capture source reports zero size.
		obs_data_set_bool(reply, "hooked", g_gameItem && obs_sceneitem_visible(g_gameItem) && obs_source_get_width(g_game) > 0);
	} else {
		ok = false;
		error = "Unknown command " + cmd;
	}

	obs_data_set_bool(reply, "ok", ok);
	if (!ok)
		obs_data_set_string(reply, "error", error.c_str());
	writeLine(reply);
	obs_data_release(reply);
	obs_data_release(c);
	return keepRunning;
}

std::mutex g_queueLock;
std::deque<std::string> g_queue;
HANDLE g_queued = nullptr;

void readCommands()
{
	std::string line;
	while (std::getline(std::cin, line)) {
		std::lock_guard<std::mutex> lock(g_queueLock);
		g_queue.push_back(line);
		SetEvent(g_queued);
	}
	std::lock_guard<std::mutex> lock(g_queueLock);
	g_queue.push_back("{\"cmd\":\"shutdown\"}");
	SetEvent(g_queued);
}

} // namespace

static std::string toUtf8(const wchar_t *s)
{
	const int size = WideCharToMultiByte(CP_UTF8, 0, s, -1, nullptr, 0, nullptr, nullptr);
	std::string out(size > 0 ? size - 1 : 0, '\0');
	if (size > 1)
		WideCharToMultiByte(CP_UTF8, 0, s, -1, out.data(), size, nullptr, nullptr);
	return out;
}

int main()
{
	// Windows refuses desktop duplication to processes that aren't DPI aware, which records black.
	SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

	// Read the wide command line, because a Windows username can contain any character.
	int argc = 0;
	wchar_t **argv = CommandLineToArgvW(GetCommandLineW(), &argc);
	if (!argv || argc < 3) {
		fprintf(stderr, "usage: afterclip-engine <install-root> <config-dir>\n");
		return 2;
	}
	const std::string root = toUtf8(argv[1]);
	const std::string config = toUtf8(argv[2]);

	// Shared for reading, so the log can be inspected while the engine runs.
	g_log = _wfsopen((std::wstring(argv[2]) + L"\\engine.log").c_str(), L"w", _SH_DENYWR);
	base_set_log_handler(logHandler, nullptr);

	CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
	g_recordStopped = CreateEventW(nullptr, TRUE, TRUE, nullptr);
	g_queued = CreateEventW(nullptr, FALSE, FALSE, nullptr);

	if (!obs_startup("en-US", config.c_str(), nullptr)) {
		fprintf(stderr, "obs_startup failed\n");
		return 1;
	}
	obs_add_data_path((root + "/data/").c_str());

	std::thread(readCommands).detach();

	// The main thread stays free to pump messages, because capture APIs can post to it.
	bool running = true;
	while (running) {
		MsgWaitForMultipleObjects(1, &g_queued, FALSE, INFINITE, QS_ALLINPUT);
		MSG msg;
		while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
			TranslateMessage(&msg);
			DispatchMessageW(&msg);
		}
		for (;;) {
			std::string next;
			{
				std::lock_guard<std::mutex> lock(g_queueLock);
				if (g_queue.empty())
					break;
				next = g_queue.front();
				g_queue.pop_front();
			}
			if (!handle(next)) {
				running = false;
				break;
			}
		}
	}
	shutdownAll();
	if (g_log)
		fclose(g_log);
	// Plugin threads can outlive main's statics, so leave without running them.
	ExitProcess(0);
}
