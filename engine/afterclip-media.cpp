// Afterclip's media helper: trims clips and makes thumbnails using the FFmpeg libraries already bundled
// with libobs. It runs one job per process, so a bad file can never take the capture engine down.
//   afterclip-media duration <in>             prints seconds
//   afterclip-media trim <in> <out> <seconds> keeps the last N seconds, losslessly
//   afterclip-media thumb <in> <out.jpg>      writes a 320px wide JPEG
//   afterclip-media join <out> <in> <start> <end> [<in> <start> <end>...]
//                                            joins ranges of clips losslessly. An end below 0 means the end of the file.
extern "C" {
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/imgutils.h>
#include <libswscale/swscale.h>
}
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shellapi.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

static const int THUMB_WIDTH = 320;
static const int THUMB_SEEK_SECONDS = 1;

static std::string toUtf8(const wchar_t *s)
{
	const int size = WideCharToMultiByte(CP_UTF8, 0, s, -1, nullptr, 0, nullptr, nullptr);
	std::string out(size > 0 ? size - 1 : 0, '\0');
	if (size > 1)
		WideCharToMultiByte(CP_UTF8, 0, s, -1, out.data(), size, nullptr, nullptr);
	return out;
}

static std::wstring toWide(const std::string &s)
{
	const int size = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
	std::wstring out(size > 0 ? size - 1 : 0, L'\0');
	if (size > 1)
		MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, out.data(), size);
	return out;
}

static int fail(const char *what, int code = 0)
{
	char detail[128] = "";
	if (code)
		av_strerror(code, detail, sizeof(detail));
	fprintf(stderr, "%s %s\n", what, detail);
	return 1;
}

static AVFormatContext *openInput(const std::string &path)
{
	AVFormatContext *in = nullptr;
	if (avformat_open_input(&in, path.c_str(), nullptr, nullptr) < 0)
		return nullptr;
	if (avformat_find_stream_info(in, nullptr) < 0) {
		avformat_close_input(&in);
		return nullptr;
	}
	return in;
}

static int cmdDuration(const std::string &path)
{
	AVFormatContext *in = openInput(path);
	if (!in)
		return fail("Could not open input");
	printf("%.3f\n", in->duration > 0 ? (double)in->duration / AV_TIME_BASE : 0.0);
	fflush(stdout);
	avformat_close_input(&in);
	return 0;
}

static int cmdTrim(const std::string &input, const std::string &output, double seconds)
{
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	const int video = av_find_best_stream(in, AVMEDIA_TYPE_VIDEO, -1, -1, nullptr, 0);
	if (video < 0)
		return fail("No video stream");

	AVFormatContext *out = nullptr;
	int err = avformat_alloc_output_context2(&out, nullptr, "mp4", output.c_str());
	if (err < 0)
		return fail("Could not create output", err);
	std::vector<int> outIndex(in->nb_streams, -1);
	int next = 0;
	for (unsigned i = 0; i < in->nb_streams; i++) {
		AVStream *from = in->streams[i];
		const AVMediaType type = from->codecpar->codec_type;
		if (type != AVMEDIA_TYPE_VIDEO && type != AVMEDIA_TYPE_AUDIO)
			continue;
		AVStream *to = avformat_new_stream(out, nullptr);
		if (!to || (err = avcodec_parameters_copy(to->codecpar, from->codecpar)) < 0)
			return fail("Could not copy stream", err);
		to->codecpar->codec_tag = 0;
		to->time_base = from->time_base;
		outIndex[i] = next++;
	}
	if ((err = avio_open(&out->pb, output.c_str(), AVIO_FLAG_WRITE)) < 0)
		return fail("Could not open output", err);
	AVDictionary *options = nullptr;
	av_dict_set(&options, "movflags", "+faststart", 0);
	err = avformat_write_header(out, &options);
	av_dict_free(&options);
	if (err < 0)
		return fail("Could not write header", err);

	// Seeking backward lands on a keyframe at or before the target, so a clip can run long but never starts late.
	const int64_t end = (in->start_time == AV_NOPTS_VALUE ? 0 : in->start_time) + in->duration;
	const int64_t target = end - (int64_t)(seconds * AV_TIME_BASE);
	const AVRational micro = {1, AV_TIME_BASE};
	const int64_t seekTs = av_rescale_q(target, micro, in->streams[video]->time_base);
	if ((err = av_seek_frame(in, video, seekTs, AVSEEK_FLAG_BACKWARD)) < 0)
		return fail("Could not seek", err);

	AVPacket *pkt = av_packet_alloc();
	int64_t originDts = AV_NOPTS_VALUE; // First video dts, in microseconds. Everything shifts to start here.
	int64_t firstPts = AV_NOPTS_VALUE;
	while (av_read_frame(in, pkt) >= 0) {
		const int index = pkt->stream_index;
		if (outIndex[index] < 0 || pkt->dts == AV_NOPTS_VALUE) {
			av_packet_unref(pkt);
			continue;
		}
		const AVRational base = in->streams[index]->time_base;
		if (originDts == AV_NOPTS_VALUE) {
			// Nothing is written until the first video keyframe, or the clip would open on undecodable frames.
			if (index != video || !(pkt->flags & AV_PKT_FLAG_KEY)) {
				av_packet_unref(pkt);
				continue;
			}
			originDts = av_rescale_q(pkt->dts, base, micro);
			firstPts = av_rescale_q(pkt->pts, base, micro);
		}
		if (index != video && av_rescale_q(pkt->pts, base, micro) < firstPts) {
			av_packet_unref(pkt);
			continue;
		}
		const int64_t shift = av_rescale_q(originDts, micro, base);
		pkt->pts -= shift;
		pkt->dts -= shift;
		AVStream *to = out->streams[outIndex[index]];
		av_packet_rescale_ts(pkt, base, to->time_base);
		pkt->stream_index = to->index;
		pkt->pos = -1;
		if ((err = av_interleaved_write_frame(out, pkt)) < 0)
			return fail("Could not write packet", err);
	}
	av_packet_free(&pkt);
	if (originDts == AV_NOPTS_VALUE)
		return fail("No keyframe found");
	if ((err = av_write_trailer(out)) < 0)
		return fail("Could not finish file", err);
	avio_closep(&out->pb);
	avformat_free_context(out);
	avformat_close_input(&in);
	return 0;
}

struct Segment {
	std::string path;
	double start;
	double end; // Negative means the end of the file.
};

/** Lossless joining needs every input to match, or players would break where the clips meet. */
static bool sameFormat(const AVFormatContext *a, const AVFormatContext *b, std::string &why)
{
	if (a->nb_streams != b->nb_streams) {
		why = "they have different numbers of audio tracks";
		return false;
	}
	for (unsigned i = 0; i < a->nb_streams; i++) {
		const AVCodecParameters *x = a->streams[i]->codecpar, *y = b->streams[i]->codecpar;
		const bool extraSame = x->extradata_size == y->extradata_size &&
				       (x->extradata_size == 0 || memcmp(x->extradata, y->extradata, x->extradata_size) == 0);
		if (x->codec_type != y->codec_type || x->codec_id != y->codec_id) {
			why = "they use different codecs";
			return false;
		}
		if (x->codec_type == AVMEDIA_TYPE_VIDEO && (x->width != y->width || x->height != y->height)) {
			why = "they have different resolutions";
			return false;
		}
		if (x->codec_type == AVMEDIA_TYPE_AUDIO && x->sample_rate != y->sample_rate) {
			why = "they have different audio sample rates";
			return false;
		}
		if (!extraSame) {
			why = "they were recorded with different video settings";
			return false;
		}
	}
	return true;
}

/**
 * Finds the decode timestamp of the last video packet needed to show everything before `endPts`.
 * With B-frames, a frame shown after the cut can be needed by one shown before it, so it has to stay.
 */
static int64_t lastNeededDts(AVFormatContext *in, int video, int64_t seekTs, int64_t endPts)
{
	av_seek_frame(in, video, seekTs, AVSEEK_FLAG_BACKWARD);
	AVPacket *pkt = av_packet_alloc();
	int64_t last = INT64_MIN;
	int pastEnd = 0;
	const int REORDER_WINDOW = 8;
	while (pastEnd < REORDER_WINDOW && av_read_frame(in, pkt) >= 0) {
		if (pkt->stream_index == video) {
			if (pkt->pts < endPts)
				last = FFMAX(last, pkt->dts);
			else
				pastEnd++;
		}
		av_packet_unref(pkt);
	}
	av_packet_free(&pkt);
	return last;
}

static int cmdJoin(const std::string &output, const std::vector<Segment> &segments)
{
	const AVRational micro = {1, AV_TIME_BASE};
	AVFormatContext *out = nullptr;
	AVFormatContext *first = nullptr;
	std::vector<int64_t> lastDts;
	int64_t timelineEnd = 0; // Where the next segment starts, in microseconds.

	for (const Segment &segment : segments) {
		AVFormatContext *in = openInput(segment.path);
		if (!in)
			return fail("Could not open a clip");
		const int video = av_find_best_stream(in, AVMEDIA_TYPE_VIDEO, -1, -1, nullptr, 0);
		if (video < 0)
			return fail("A clip has no video");

		int err;
		if (!out) {
			first = in;
			if ((err = avformat_alloc_output_context2(&out, nullptr, "mp4", output.c_str())) < 0)
				return fail("Could not create output", err);
			for (unsigned i = 0; i < in->nb_streams; i++) {
				AVStream *to = avformat_new_stream(out, nullptr);
				if (!to || (err = avcodec_parameters_copy(to->codecpar, in->streams[i]->codecpar)) < 0)
					return fail("Could not copy stream", err);
				to->codecpar->codec_tag = 0;
				to->time_base = in->streams[i]->time_base;
			}
			lastDts.assign(in->nb_streams, INT64_MIN);
			if ((err = avio_open(&out->pb, output.c_str(), AVIO_FLAG_WRITE)) < 0)
				return fail("Could not open output", err);
			AVDictionary *options = nullptr;
			av_dict_set(&options, "movflags", "+faststart", 0);
			err = avformat_write_header(out, &options);
			av_dict_free(&options);
			if (err < 0)
				return fail("Could not write header", err);
		} else {
			std::string why;
			if (!sameFormat(first, in, why)) {
				fprintf(stderr, "These clips can't be merged because %s.\n", why.c_str());
				return 3;
			}
		}

		const AVRational videoBase = in->streams[video]->time_base;
		const int64_t base = in->start_time == AV_NOPTS_VALUE ? 0 : in->start_time;
		const double duration = (double)in->duration / AV_TIME_BASE;
		const double end = segment.end < 0 || segment.end > duration ? duration : segment.end;
		const int64_t startMicro = base + (int64_t)(segment.start * AV_TIME_BASE);
		const int64_t endMicro = base + (int64_t)(end * AV_TIME_BASE);
		const int64_t seekTs = av_rescale_q(startMicro, micro, videoBase);
		const int64_t lastVideoDts = lastNeededDts(in, video, seekTs, av_rescale_q(endMicro, micro, videoBase));
		if ((err = av_seek_frame(in, video, seekTs, AVSEEK_FLAG_BACKWARD)) < 0)
			return fail("Could not seek", err);

		AVPacket *pkt = av_packet_alloc();
		int64_t originDts = AV_NOPTS_VALUE, firstPts = AV_NOPTS_VALUE, segmentEnd = 0;
		while (av_read_frame(in, pkt) >= 0) {
			const int index = pkt->stream_index;
			const AVMediaType type = in->streams[index]->codecpar->codec_type;
			const bool usable = (type == AVMEDIA_TYPE_VIDEO || type == AVMEDIA_TYPE_AUDIO) && pkt->dts != AV_NOPTS_VALUE;
			const AVRational streamBase = in->streams[index]->time_base;
			if (!usable) {
				av_packet_unref(pkt);
				continue;
			}
			if (originDts == AV_NOPTS_VALUE) {
				// Nothing is written until the first video keyframe, or the clip would open on undecodable frames.
				if (index != video || !(pkt->flags & AV_PKT_FLAG_KEY)) {
					av_packet_unref(pkt);
					continue;
				}
				originDts = av_rescale_q(pkt->dts, streamBase, micro);
				firstPts = av_rescale_q(pkt->pts, streamBase, micro);
			}
			const int64_t ptsMicro = av_rescale_q(pkt->pts, streamBase, micro);
			if (index == video ? pkt->dts > lastVideoDts : (ptsMicro < firstPts || ptsMicro >= endMicro)) {
				const bool done = index == video;
				av_packet_unref(pkt);
				if (done)
					break;
				continue;
			}
			const int64_t shift = av_rescale_q(originDts - timelineEnd, micro, streamBase);
			pkt->pts -= shift;
			pkt->dts -= shift;
			AVStream *to = out->streams[index];
			av_packet_rescale_ts(pkt, streamBase, to->time_base);
			// The join point can leave timestamps touching, and the muxer needs each stream strictly rising.
			if (lastDts[index] != INT64_MIN && pkt->dts <= lastDts[index]) {
				const int64_t bump = lastDts[index] + 1 - pkt->dts;
				pkt->dts += bump;
				pkt->pts += bump;
			}
			lastDts[index] = pkt->dts;
			segmentEnd = FFMAX(segmentEnd, av_rescale_q(pkt->pts + pkt->duration, to->time_base, micro));
			pkt->stream_index = to->index;
			pkt->pos = -1;
			if ((err = av_interleaved_write_frame(out, pkt)) < 0)
				return fail("Could not write packet", err);
		}
		av_packet_free(&pkt);
		if (originDts == AV_NOPTS_VALUE)
			return fail("A clip had no keyframe in the chosen range");
		timelineEnd = segmentEnd;
		if (in != first)
			avformat_close_input(&in);
	}
	int err = av_write_trailer(out);
	if (err < 0)
		return fail("Could not finish file", err);
	avio_closep(&out->pb);
	avformat_free_context(out);
	avformat_close_input(&first);
	return 0;
}

static int writeJpeg(const AVFrame *frame, const std::string &output)
{
	const int height = FFMAX(2, (int)((int64_t)frame->height * THUMB_WIDTH / frame->width) & ~1);
	const AVCodec *codec = avcodec_find_encoder(AV_CODEC_ID_MJPEG);
	if (!codec)
		return fail("No JPEG encoder");
	AVCodecContext *enc = avcodec_alloc_context3(codec);
	enc->width = THUMB_WIDTH;
	enc->height = height;
	enc->pix_fmt = AV_PIX_FMT_YUVJ420P;
	enc->color_range = AVCOL_RANGE_JPEG;
	enc->time_base = {1, 25};
	enc->flags |= AV_CODEC_FLAG_QSCALE;
	enc->global_quality = 5 * FF_QP2LAMBDA;
	int err = avcodec_open2(enc, codec, nullptr);
	if (err < 0)
		return fail("Could not open JPEG encoder", err);

	AVFrame *small = av_frame_alloc();
	small->format = enc->pix_fmt;
	small->width = enc->width;
	small->height = enc->height;
	small->color_range = AVCOL_RANGE_JPEG;
	small->quality = enc->global_quality;
	small->pts = 0;
	if ((err = av_frame_get_buffer(small, 0)) < 0)
		return fail("Could not allocate frame", err);
	SwsContext *sws = sws_getContext(frame->width, frame->height, (AVPixelFormat)frame->format, enc->width, enc->height,
					 enc->pix_fmt, SWS_BILINEAR, nullptr, nullptr, nullptr);
	if (!sws)
		return fail("Could not create scaler");
	sws_scale(sws, frame->data, frame->linesize, 0, frame->height, small->data, small->linesize);
	sws_freeContext(sws);

	AVPacket *pkt = av_packet_alloc();
	if ((err = avcodec_send_frame(enc, small)) < 0 || (err = avcodec_send_frame(enc, nullptr)) < 0 ||
	    (err = avcodec_receive_packet(enc, pkt)) < 0)
		return fail("Could not encode JPEG", err);
	// Written to a temp name and renamed, so a crash can't leave a half-written file in the thumbnail cache.
	const std::wstring partial = toWide(output + ".part");
	FILE *f = nullptr;
	if (_wfopen_s(&f, partial.c_str(), L"wb") != 0 || !f)
		return fail("Could not write thumbnail");
	fwrite(pkt->data, 1, pkt->size, f);
	fclose(f);
	if (!MoveFileExW(partial.c_str(), toWide(output).c_str(), MOVEFILE_REPLACE_EXISTING))
		return fail("Could not finish thumbnail");
	av_packet_free(&pkt);
	av_frame_free(&small);
	avcodec_free_context(&enc);
	return 0;
}

static int cmdThumb(const std::string &input, const std::string &output)
{
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	const AVCodec *codec = nullptr;
	const int video = av_find_best_stream(in, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
	if (video < 0 || !codec)
		return fail("No decodable video stream");
	AVCodecContext *dec = avcodec_alloc_context3(codec);
	avcodec_parameters_to_context(dec, in->streams[video]->codecpar);
	dec->thread_count = 1; // Stays out of the way of a running game.
	int err = avcodec_open2(dec, codec, nullptr);
	if (err < 0)
		return fail("Could not open decoder", err);

	const AVRational base = in->streams[video]->time_base;
	int64_t wanted = 0;
	if (in->duration > THUMB_SEEK_SECONDS * (int64_t)AV_TIME_BASE) {
		wanted = av_rescale_q(THUMB_SEEK_SECONDS * (int64_t)AV_TIME_BASE, {1, AV_TIME_BASE}, base);
		av_seek_frame(in, video, wanted, AVSEEK_FLAG_BACKWARD);
	}

	AVPacket *pkt = av_packet_alloc();
	AVFrame *frame = av_frame_alloc();
	bool draining = false;
	for (;;) {
		if (!draining) {
			const int read = av_read_frame(in, pkt);
			if (read < 0) {
				draining = true;
				avcodec_send_packet(dec, nullptr);
			} else if (pkt->stream_index != video) {
				av_packet_unref(pkt);
				continue;
			} else {
				avcodec_send_packet(dec, pkt);
				av_packet_unref(pkt);
			}
		}
		err = avcodec_receive_frame(dec, frame);
		if (err == AVERROR(EAGAIN))
			continue;
		if (err < 0)
			return fail("No frame could be decoded", err);
		// Use the first frame at or past the seek point, for a picture that isn't the fade-in.
		if (frame->pts == AV_NOPTS_VALUE || frame->pts >= wanted || draining) {
			const int result = writeJpeg(frame, output);
			av_frame_free(&frame);
			av_packet_free(&pkt);
			avcodec_free_context(&dec);
			avformat_close_input(&in);
			return result;
		}
		av_frame_unref(frame);
	}
}

int main()
{
	int argc = 0;
	wchar_t **argv = CommandLineToArgvW(GetCommandLineW(), &argc);
	av_log_set_level(AV_LOG_ERROR);
	if (argv && argc >= 3) {
		const std::wstring cmd = argv[1];
		const std::string in = toUtf8(argv[2]);
		if (cmd == L"duration")
			return cmdDuration(in);
		if (cmd == L"trim" && argc >= 5)
			return cmdTrim(in, toUtf8(argv[3]), _wtof(argv[4]));
		if (cmd == L"thumb" && argc >= 4)
			return cmdThumb(in, toUtf8(argv[3]));
		if (cmd == L"join" && argc >= 6 && (argc - 3) % 3 == 0) {
			std::vector<Segment> segments;
			for (int i = 3; i + 2 < argc; i += 3)
				segments.push_back({toUtf8(argv[i]), _wtof(argv[i + 1]), _wtof(argv[i + 2])});
			return cmdJoin(in, segments);
		}
	}
	fprintf(stderr, "usage: afterclip-media duration|trim|thumb ...\n");
	return 2;
}
