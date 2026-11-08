// Afterclip's media helper: trims clips and makes thumbnails using the FFmpeg libraries already bundled
// with libobs. It runs one job per process, so a bad file can never take the capture engine down.
//   afterclip-media duration <in>             prints seconds
//   afterclip-media trim <in> <out> <seconds> keeps the last N seconds, losslessly
//   afterclip-media thumb <in> <out.jpg>      writes a 320px wide JPEG
//   afterclip-media join <out> <in> <start> <end> [<in> <start> <end>...]
//                                            joins ranges of clips losslessly. An end below 0 means the end of the file.
//   afterclip-media tracks <in>               prints how many audio tracks it has
//   afterclip-media shrink <in> <out> <bytes> re-encodes a smaller copy to fit that size, as H.264 with the main mix
//   afterclip-media compress <in> <out> <bytes>
//                                            aims for that size at up to 1080p60, stopping at a good quality, and prints the final size
//   afterclip-media retrack <in> <out> <gain> [<gain>...]
//                                            sets each audio track's volume, 1 keeping it untouched and 0 removing it.
//                                            The video is copied losslessly.
//   afterclip-media retag <in> <out>          copies a clip with HEVC tagged hev1 to <out> tagged hvc1, so Apple devices play it.
//                                            Prints "retagged", or "unchanged" without writing <out> when it needs nothing.
// shrink, compress and vertical also take these, anywhere after the command:
//   --tracks=0,2                             mixes these audio tracks, counted from 0, instead of only the main mix
//   --even                                   evens out the volume, so a quiet clip comes out as loud as a loud one
#define _USE_MATH_DEFINES
extern "C" {
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/audio_fifo.h>
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
#include <libswscale/swscale.h>
}
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shellapi.h>

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

static const int THUMB_WIDTH = 320;
static const int THUMB_SEEK_SECONDS = 1;

/** The audio a shared copy gets, from --tracks and --even. */
struct ShareAudio {
	std::vector<int> tracks; // Audio tracks to mix, counted from 0. Empty means the main mix.
	bool even = false;
};
static ShareAudio g_share;

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

/**
 * The MP4 tag for a copied stream. FFmpeg picks hev1 for HEVC, which iPhones and Macs refuse to play,
 * so HEVC gets hvc1. Anything else lets FFmpeg choose.
 */
static uint32_t mp4Tag(const AVCodecParameters *par)
{
	return par->codec_id == AV_CODEC_ID_HEVC ? MKTAG('h', 'v', 'c', '1') : 0;
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
		to->codecpar->codec_tag = mp4Tag(to->codecpar);
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

/**
 * Copies a clip whose HEVC is tagged hev1 into one tagged hvc1, so Apple devices play it. Everything is copied
 * as it is. Prints "unchanged" and writes nothing when there's nothing to fix, or "retagged" when it wrote <out>.
 */
static int cmdRetag(const std::string &input, const std::string &output)
{
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	bool needed = false;
	for (unsigned i = 0; i < in->nb_streams; i++) {
		const AVCodecParameters *par = in->streams[i]->codecpar;
		needed |= par->codec_id == AV_CODEC_ID_HEVC && par->codec_tag != MKTAG('h', 'v', 'c', '1');
	}
	if (!needed) {
		avformat_close_input(&in);
		printf("unchanged\n");
		fflush(stdout);
		return 0;
	}

	AVFormatContext *out = nullptr;
	int err = avformat_alloc_output_context2(&out, nullptr, "mp4", output.c_str());
	if (err < 0)
		return fail("Could not create output", err);
	av_dict_copy(&out->metadata, in->metadata, 0);
	std::vector<int> outIndex(in->nb_streams, -1);
	for (unsigned i = 0; i < in->nb_streams; i++) {
		AVStream *from = in->streams[i];
		const AVMediaType type = from->codecpar->codec_type;
		if (type != AVMEDIA_TYPE_VIDEO && type != AVMEDIA_TYPE_AUDIO)
			continue;
		AVStream *to = avformat_new_stream(out, nullptr);
		if (!to || (err = avcodec_parameters_copy(to->codecpar, from->codecpar)) < 0)
			return fail("Could not copy stream", err);
		to->codecpar->codec_tag = mp4Tag(to->codecpar);
		to->time_base = from->time_base;
		to->disposition = from->disposition;
		av_dict_copy(&to->metadata, from->metadata, 0);
		outIndex[i] = to->index;
	}
	// Chapters are copied the way FFmpeg's own tool does it, and avformat_free_context frees them.
	if (in->nb_chapters) {
		out->chapters = (AVChapter **)av_calloc(in->nb_chapters, sizeof(AVChapter *));
		if (!out->chapters)
			return fail("Out of memory");
		for (unsigned i = 0; i < in->nb_chapters; i++) {
			const AVChapter *from = in->chapters[i];
			AVChapter *to = (AVChapter *)av_mallocz(sizeof(AVChapter));
			if (!to)
				return fail("Out of memory");
			to->id = from->id;
			to->time_base = from->time_base;
			to->start = from->start;
			to->end = from->end;
			av_dict_copy(&to->metadata, from->metadata, 0);
			out->chapters[out->nb_chapters++] = to;
		}
	}
	if ((err = avio_open(&out->pb, output.c_str(), AVIO_FLAG_WRITE)) < 0)
		return fail("Could not open output", err);
	AVDictionary *options = nullptr;
	av_dict_set(&options, "movflags", "+faststart", 0);
	err = avformat_write_header(out, &options);
	av_dict_free(&options);
	if (err < 0)
		return fail("Could not write header", err);

	AVPacket *pkt = av_packet_alloc();
	while (av_read_frame(in, pkt) >= 0) {
		const int index = outIndex[pkt->stream_index];
		if (index < 0) {
			av_packet_unref(pkt);
			continue;
		}
		av_packet_rescale_ts(pkt, in->streams[pkt->stream_index]->time_base, out->streams[index]->time_base);
		pkt->stream_index = index;
		pkt->pos = -1;
		if ((err = av_interleaved_write_frame(out, pkt)) < 0)
			return fail("Could not write packet", err);
	}
	av_packet_free(&pkt);
	if ((err = av_write_trailer(out)) < 0)
		return fail("Could not finish file", err);
	avio_closep(&out->pb);
	avformat_free_context(out);
	avformat_close_input(&in);
	printf("retagged\n");
	fflush(stdout);
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
				to->codecpar->codec_tag = mp4Tag(to->codecpar);
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

/** A full-range 4:2:0 picture of this size, ready to be drawn into and saved as a JPEG. */
static AVFrame *jpegFrame(int width, int height)
{
	AVFrame *picture = av_frame_alloc();
	picture->format = AV_PIX_FMT_YUVJ420P;
	picture->width = width;
	picture->height = height;
	picture->color_range = AVCOL_RANGE_JPEG;
	picture->pts = 0;
	if (av_frame_get_buffer(picture, 0) < 0)
		av_frame_free(&picture);
	return picture;
}

/** Saves a picture from jpegFrame as a JPEG. */
static int saveJpeg(AVFrame *picture, const std::string &output)
{
	const AVCodec *codec = avcodec_find_encoder(AV_CODEC_ID_MJPEG);
	if (!codec)
		return fail("No JPEG encoder");
	AVCodecContext *enc = avcodec_alloc_context3(codec);
	enc->width = picture->width;
	enc->height = picture->height;
	enc->pix_fmt = AV_PIX_FMT_YUVJ420P;
	enc->color_range = AVCOL_RANGE_JPEG;
	enc->time_base = {1, 25};
	enc->flags |= AV_CODEC_FLAG_QSCALE;
	enc->global_quality = 5 * FF_QP2LAMBDA;
	int err = avcodec_open2(enc, codec, nullptr);
	if (err < 0)
		return fail("Could not open JPEG encoder", err);
	picture->quality = enc->global_quality;

	AVPacket *pkt = av_packet_alloc();
	if ((err = avcodec_send_frame(enc, picture)) < 0 || (err = avcodec_send_frame(enc, nullptr)) < 0 ||
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
	avcodec_free_context(&enc);
	return 0;
}

static int writeJpeg(const AVFrame *frame, const std::string &output)
{
	const int height = FFMAX(2, (int)((int64_t)frame->height * THUMB_WIDTH / frame->width) & ~1);
	AVFrame *small = jpegFrame(THUMB_WIDTH, height);
	if (!small)
		return fail("Could not allocate frame");
	SwsContext *sws = sws_getContext(frame->width, frame->height, (AVPixelFormat)frame->format, small->width, small->height,
					 AV_PIX_FMT_YUVJ420P, SWS_BILINEAR, nullptr, nullptr, nullptr);
	if (!sws)
		return fail("Could not create scaler");
	sws_scale(sws, frame->data, frame->linesize, 0, frame->height, small->data, small->linesize);
	sws_freeContext(sws);
	const int result = saveJpeg(small, output);
	av_frame_free(&small);
	return result;
}

/**
 * A row of small frames spread evenly across a video, for scrubbing through it on hover. Each frame is the
 * keyframe nearest its moment, so only one picture is decoded for each, however long the video is.
 */
static int cmdStrip(const std::string &input, const std::string &output, int count)
{
	const int FRAME_WIDTH = 160;
	count = FFMAX(1, FFMIN(count, 40));
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	const AVCodec *codec = nullptr;
	const int video = av_find_best_stream(in, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
	if (video < 0 || !codec || in->duration <= 0)
		return fail("No decodable video stream");
	AVStream *stream = in->streams[video];
	AVCodecContext *dec = avcodec_alloc_context3(codec);
	avcodec_parameters_to_context(dec, stream->codecpar);
	dec->thread_count = 2;
	int err = avcodec_open2(dec, codec, nullptr);
	if (err < 0)
		return fail("Could not open decoder", err);

	const int height = FFMAX(2, (int)((int64_t)stream->codecpar->height * FRAME_WIDTH / stream->codecpar->width) & ~1);
	AVFrame *strip = jpegFrame(FRAME_WIDTH * count, height);
	if (!strip)
		return fail("Could not allocate frame");
	// Black, in case a moment can't be decoded.
	for (int plane = 0; plane < 3; plane++)
		memset(strip->data[plane], plane ? 128 : 0, (size_t)strip->linesize[plane] * (plane ? height / 2 : height));

	const int64_t start = stream->start_time == AV_NOPTS_VALUE ? 0 : stream->start_time;
	AVPacket *pkt = av_packet_alloc();
	AVFrame *frame = av_frame_alloc();
	SwsContext *sws = nullptr;
	for (int i = 0; i < count; i++) {
		const int64_t moment = (int64_t)((double)in->duration * (i + 0.5) / count);
		av_seek_frame(in, video, start + av_rescale_q(moment, {1, AV_TIME_BASE}, stream->time_base), AVSEEK_FLAG_BACKWARD);
		avcodec_flush_buffers(dec);
		bool got = false, draining = false;
		while (!got) {
			if (!draining) {
				if (av_read_frame(in, pkt) < 0) {
					draining = true;
					avcodec_send_packet(dec, nullptr);
				} else {
					if (pkt->stream_index == video)
						avcodec_send_packet(dec, pkt);
					av_packet_unref(pkt);
				}
			}
			err = avcodec_receive_frame(dec, frame);
			if (err == 0)
				got = true;
			else if (err != AVERROR(EAGAIN))
				break;
		}
		if (!got)
			continue;
		sws = sws_getCachedContext(sws, frame->width, frame->height, (AVPixelFormat)frame->format, FRAME_WIDTH, height,
					   AV_PIX_FMT_YUVJ420P, SWS_BILINEAR, nullptr, nullptr, nullptr);
		if (!sws)
			return fail("Could not create scaler");
		uint8_t *to[4] = {strip->data[0] + i * FRAME_WIDTH, strip->data[1] + i * FRAME_WIDTH / 2, strip->data[2] + i * FRAME_WIDTH / 2, nullptr};
		sws_scale(sws, frame->data, frame->linesize, 0, frame->height, to, strip->linesize);
		av_frame_unref(frame);
	}
	const int result = saveJpeg(strip, output);
	sws_freeContext(sws);
	av_frame_free(&frame);
	av_frame_free(&strip);
	av_packet_free(&pkt);
	avcodec_free_context(&dec);
	avformat_close_input(&in);
	return result;
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

static int cmdTracks(const std::string &path)
{
	AVFormatContext *in = openInput(path);
	if (!in)
		return fail("Could not open input");
	int count = 0;
	for (unsigned i = 0; i < in->nb_streams; i++)
		count += in->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO;
	printf("%d\n", count);
	fflush(stdout);
	avformat_close_input(&in);
	return 0;
}

/** Sends an encoder's finished packets to the file. */
static int drainEncoder(AVCodecContext *enc, AVFormatContext *out, AVStream *stream, AVPacket *pkt)
{
	for (;;) {
		const int err = avcodec_receive_packet(enc, pkt);
		if (err == AVERROR(EAGAIN) || err == AVERROR_EOF)
			return 0;
		if (err < 0)
			return err;
		av_packet_rescale_ts(pkt, enc->time_base, stream->time_base);
		pkt->stream_index = stream->index;
		const int written = av_interleaved_write_frame(out, pkt);
		if (written < 0)
			return written;
	}
}

/** Decodes one AAC track and encodes it again, at a chosen volume. AAC needs exact frame sizes, hence the FIFO. */
struct AudioTrack {
	AVStream *from = nullptr;
	AVStream *to = nullptr;
	AVCodecContext *dec = nullptr;
	AVCodecContext *enc = nullptr;
	AVAudioFifo *fifo = nullptr;
	AVFrame *decoded = nullptr;
	double gain = 1.0;
	int64_t originMicro = 0; // Where the output's time zero is in the input.
	int64_t nextPts = AV_NOPTS_VALUE;
};

static int openAudioTrack(AudioTrack &track, AVStream *from, AVFormatContext *out, int bitrate, double gain, int64_t originMicro)
{
	const AVCodec *decoder = avcodec_find_decoder(from->codecpar->codec_id);
	const AVCodec *encoder = avcodec_find_encoder(AV_CODEC_ID_AAC);
	if (!decoder || !encoder)
		return AVERROR_ENCODER_NOT_FOUND;
	track.from = from;
	track.gain = gain;
	track.originMicro = originMicro;
	track.dec = avcodec_alloc_context3(decoder);
	int err = avcodec_parameters_to_context(track.dec, from->codecpar);
	if (err < 0 || (err = avcodec_open2(track.dec, decoder, nullptr)) < 0)
		return err;
	track.enc = avcodec_alloc_context3(encoder);
	track.enc->sample_fmt = AV_SAMPLE_FMT_FLTP;
	track.enc->sample_rate = from->codecpar->sample_rate;
	track.enc->bit_rate = bitrate;
	track.enc->time_base = {1, from->codecpar->sample_rate};
	if ((err = av_channel_layout_copy(&track.enc->ch_layout, &from->codecpar->ch_layout)) < 0)
		return err;
	if (out->oformat->flags & AVFMT_GLOBALHEADER)
		track.enc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
	if ((err = avcodec_open2(track.enc, encoder, nullptr)) < 0)
		return err;
	track.to = avformat_new_stream(out, nullptr);
	if (!track.to)
		return AVERROR(ENOMEM);
	if ((err = avcodec_parameters_from_context(track.to->codecpar, track.enc)) < 0)
		return err;
	track.to->time_base = track.enc->time_base;
	track.fifo = av_audio_fifo_alloc(AV_SAMPLE_FMT_FLTP, track.enc->ch_layout.nb_channels, track.enc->frame_size * 4);
	track.decoded = av_frame_alloc();
	return track.fifo && track.decoded ? 0 : AVERROR(ENOMEM);
}

static int encodeFromFifo(AudioTrack &track, AVFormatContext *out, AVPacket *pkt, int samples)
{
	AVFrame *frame = av_frame_alloc();
	frame->nb_samples = samples;
	frame->format = track.enc->sample_fmt;
	frame->sample_rate = track.enc->sample_rate;
	int err = av_channel_layout_copy(&frame->ch_layout, &track.enc->ch_layout);
	if (err >= 0)
		err = av_frame_get_buffer(frame, 0);
	if (err < 0) {
		av_frame_free(&frame);
		return err;
	}
	av_audio_fifo_read(track.fifo, (void **)frame->data, samples);
	frame->pts = track.nextPts;
	track.nextPts += samples;
	err = avcodec_send_frame(track.enc, frame);
	av_frame_free(&frame);
	return err < 0 ? err : drainEncoder(track.enc, out, track.to, pkt);
}

/** Takes a packet from the input, or nullptr at the end of the file. */
static int feedAudio(AudioTrack &track, const AVPacket *input, AVFormatContext *out, AVPacket *pkt)
{
	int err = avcodec_send_packet(track.dec, input);
	if (err < 0 && err != AVERROR_EOF && err != AVERROR_INVALIDDATA)
		return err;
	while ((err = avcodec_receive_frame(track.dec, track.decoded)) >= 0) {
		AVFrame *in = track.decoded;
		if (in->format != AV_SAMPLE_FMT_FLTP) {
			av_frame_unref(in);
			return AVERROR(EINVAL);
		}
		const int64_t micro = in->pts == AV_NOPTS_VALUE ? 0 : av_rescale_q(in->pts, track.from->time_base, {1, AV_TIME_BASE});
		if (micro < track.originMicro && in->pts != AV_NOPTS_VALUE) {
			av_frame_unref(in);
			continue;
		}
		if (track.nextPts == AV_NOPTS_VALUE)
			track.nextPts = av_rescale_q(micro - track.originMicro, {1, AV_TIME_BASE}, track.enc->time_base);
		for (int channel = 0; channel < in->ch_layout.nb_channels && track.gain != 1.0; channel++) {
			float *samples = (float *)in->extended_data[channel];
			for (int i = 0; i < in->nb_samples; i++)
				samples[i] = (float)FFMIN(1.0, FFMAX(-1.0, samples[i] * track.gain));
		}
		av_audio_fifo_write(track.fifo, (void **)in->extended_data, in->nb_samples);
		av_frame_unref(in);
		while (av_audio_fifo_size(track.fifo) >= track.enc->frame_size) {
			const int encoded = encodeFromFifo(track, out, pkt, track.enc->frame_size);
			if (encoded < 0)
				return encoded;
		}
	}
	return err == AVERROR(EAGAIN) || err == AVERROR_EOF ? 0 : err;
}

static int finishAudio(AudioTrack &track, AVFormatContext *out, AVPacket *pkt)
{
	int err = feedAudio(track, nullptr, out, pkt);
	if (err < 0)
		return err;
	const int left = av_audio_fifo_size(track.fifo);
	if (left > 0 && (err = encodeFromFifo(track, out, pkt, left)) < 0)
		return err;
	if ((err = avcodec_send_frame(track.enc, nullptr)) < 0)
		return err;
	return drainEncoder(track.enc, out, track.to, pkt);
}

static void freeAudio(AudioTrack &track)
{
	avcodec_free_context(&track.dec);
	avcodec_free_context(&track.enc);
	if (track.fifo)
		av_audio_fifo_free(track.fifo);
	av_frame_free(&track.decoded);
}

/**
 * How loud a mix sounds, as ITU-R BS.1770 measures it: K-weighted, in 400 ms blocks every 100 ms, ignoring
 * silence and the quiet stretches between loud ones. The answer is in LUFS.
 */
struct LoudnessMeter {
	struct Biquad {
		double b0, b1, b2, a1, a2;
	};
	Biquad shelf{}, highpass{};
	std::vector<double> state; // Four values a channel: two for each filter.
	int channels = 0;
	int hop = 0;
	int filled = 0;
	double hopSum = 0;
	std::vector<double> hops; // The weighted power of each 100 ms.

	void open(int rate, int channelCount)
	{
		// The filters from the standard, worked out for this sample rate as libebur128 does.
		double K = tan(M_PI * 1681.974450955533 / rate);
		const double Q = 0.7071752369554196;
		const double Vh = pow(10.0, 3.999843853973347 / 20.0);
		const double Vb = pow(Vh, 0.4996667741545416);
		double a0 = 1.0 + K / Q + K * K;
		shelf = {(Vh + Vb * K / Q + K * K) / a0, 2.0 * (K * K - Vh) / a0, (Vh - Vb * K / Q + K * K) / a0, 2.0 * (K * K - 1.0) / a0,
			 (1.0 - K / Q + K * K) / a0};
		K = tan(M_PI * 38.13547087602444 / rate);
		const double Qh = 0.5003270373238773;
		a0 = 1.0 + K / Qh + K * K;
		highpass = {1.0, -2.0, 1.0, 2.0 * (K * K - 1.0) / a0, (1.0 - K / Qh + K * K) / a0};
		channels = channelCount;
		hop = FFMAX(1, rate / 10);
		state.assign((size_t)channels * 4, 0.0);
	}

	void add(float *const *planes, int count)
	{
		for (int i = 0; i < count; i++) {
			for (int c = 0; c < channels; c++) {
				double *s = &state[(size_t)c * 4];
				const double x = planes[c][i];
				const double y = shelf.b0 * x + s[0];
				s[0] = shelf.b1 * x - shelf.a1 * y + s[1];
				s[1] = shelf.b2 * x - shelf.a2 * y;
				const double z = highpass.b0 * y + s[2];
				s[2] = highpass.b1 * y - highpass.a1 * z + s[3];
				s[3] = highpass.b2 * y - highpass.a2 * z;
				hopSum += z * z;
			}
			if (++filled == hop) {
				hops.push_back(hopSum / hop);
				hopSum = 0;
				filled = 0;
			}
		}
	}

	/** The integrated loudness, or -infinity for silence. */
	double integrated() const
	{
		std::vector<double> blocks;
		for (size_t i = 3; i < hops.size(); i++)
			blocks.push_back((hops[i - 3] + hops[i - 2] + hops[i - 1] + hops[i]) / 4.0);
		// Under 400 ms there's no whole block, so what there is counts as one.
		if (blocks.empty() && !hops.empty()) {
			double sum = 0;
			for (double h : hops)
				sum += h;
			blocks.push_back(sum / hops.size());
		}
		const auto lufs = [](double power) { return -0.691 + 10.0 * log10(power); };
		const auto gatedMean = [&](double gate) {
			double sum = 0;
			int count = 0;
			for (double power : blocks) {
				if (power > 0 && lufs(power) > gate) {
					sum += power;
					count++;
				}
			}
			return count ? sum / count : 0.0;
		};
		const double absolute = gatedMean(-70.0);
		if (absolute <= 0)
			return -INFINITY;
		const double relative = gatedMean(lufs(absolute) - 10.0);
		return relative > 0 ? lufs(relative) : -INFINITY;
	}
};

/** One audio track going into a mix. */
struct MixInput {
	AVStream *from = nullptr;
	AVCodecContext *dec = nullptr;
	AVAudioFifo *fifo = nullptr;
	bool started = false;
	bool silent = false; // Gave no sound long after the others started, so it counts as silence from then on.
};

/**
 * Mixes chosen audio tracks into one, optionally evened out, and hands it to an AAC encoder or a loudness meter.
 * Each track is lined up by its own start time, because OBS starts every track together but not always exactly.
 */
struct Mixer {
	std::vector<MixInput> inputs;
	std::vector<int> inputOf; // For each stream in the file, its input, or -1.
	int rate = 0;
	int channels = 0;
	AVChannelLayout layout{};
	int64_t originMicro = 0;
	double level = 1.0;     // From --even. 1 leaves the volume alone.
	bool limit = false;     // Keeps peaks under the ceiling, needed when tracks add up or the level goes up.
	double reduction = 1.0; // The limiter's gain right now.
	double release = 0.0;
	int chunk = 1024;
	AVFrame *decoded = nullptr;
	std::vector<std::vector<float>> mixed, scratch;
	AVCodecContext *enc = nullptr;
	AVStream *to = nullptr;
	int64_t nextPts = 0;
	LoudnessMeter *meter = nullptr;
};

// What a copy is evened out to. Loud enough for Discord and phones, with room left for gunshots and shouts.
static const double EVEN_TARGET_LUFS = -16.0;
static const double EVEN_MOST_BOOST_DB = 18.0; // A near-silent clip would be all hiss turned this far up.
static const double EVEN_MOST_CUT_DB = -12.0;
static const double LIMIT_CEILING = 0.891; // -1 dBFS
static const double LIMIT_RELEASE_SECONDS = 0.15;
// A track still silent this long after another started has no sound to give.
static const int SILENT_AFTER_SECONDS = 5;

/** The audio streams of a file, in order, so track 0 is the main mix. */
static std::vector<int> audioStreams(const AVFormatContext *in)
{
	std::vector<int> found;
	for (unsigned i = 0; i < in->nb_streams; i++) {
		if (in->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO)
			found.push_back((int)i);
	}
	return found;
}

/** Sets up decoders for the chosen tracks. Tracks that don't match the first one's format are left out. Returns how many are mixed. */
static int openMixer(Mixer &mix, AVFormatContext *in, const std::vector<int> &picks, int64_t originMicro)
{
	const std::vector<int> streams = audioStreams(in);
	std::vector<int> wanted;
	for (int pick : picks) {
		if (pick >= 0 && pick < (int)streams.size() && std::find(wanted.begin(), wanted.end(), streams[pick]) == wanted.end())
			wanted.push_back(streams[pick]);
	}
	if (wanted.empty() && !streams.empty())
		wanted.push_back(streams[0]);
	mix.inputOf.assign(in->nb_streams, -1);
	mix.originMicro = originMicro;
	for (int index : wanted) {
		AVStream *from = in->streams[index];
		const AVCodec *decoder = avcodec_find_decoder(from->codecpar->codec_id);
		if (!decoder)
			continue;
		if (mix.inputs.empty()) {
			mix.rate = from->codecpar->sample_rate;
			mix.channels = from->codecpar->ch_layout.nb_channels;
			if (av_channel_layout_copy(&mix.layout, &from->codecpar->ch_layout) < 0)
				return 0;
		} else if (from->codecpar->sample_rate != mix.rate || from->codecpar->ch_layout.nb_channels != mix.channels) {
			fprintf(stderr, "Left out audio track %d, which is in a different format\n", index);
			continue;
		}
		MixInput input;
		input.from = from;
		input.dec = avcodec_alloc_context3(decoder);
		if (avcodec_parameters_to_context(input.dec, from->codecpar) < 0 || avcodec_open2(input.dec, decoder, nullptr) < 0) {
			avcodec_free_context(&input.dec);
			continue;
		}
		input.fifo = av_audio_fifo_alloc(AV_SAMPLE_FMT_FLTP, mix.channels, mix.rate);
		if (!input.fifo) {
			avcodec_free_context(&input.dec);
			continue;
		}
		mix.inputOf[index] = (int)mix.inputs.size();
		mix.inputs.push_back(input);
	}
	mix.decoded = av_frame_alloc();
	mix.release = exp(-1.0 / (LIMIT_RELEASE_SECONDS * FFMAX(1, mix.rate)));
	return (int)mix.inputs.size();
}

static int openMixEncoder(Mixer &mix, AVFormatContext *out, int bitrate)
{
	const AVCodec *encoder = avcodec_find_encoder(AV_CODEC_ID_AAC);
	if (!encoder)
		return AVERROR_ENCODER_NOT_FOUND;
	mix.enc = avcodec_alloc_context3(encoder);
	mix.enc->sample_fmt = AV_SAMPLE_FMT_FLTP;
	mix.enc->sample_rate = mix.rate;
	mix.enc->bit_rate = bitrate;
	mix.enc->time_base = {1, mix.rate};
	int err = av_channel_layout_copy(&mix.enc->ch_layout, &mix.layout);
	if (err < 0)
		return err;
	if (out->oformat->flags & AVFMT_GLOBALHEADER)
		mix.enc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
	if ((err = avcodec_open2(mix.enc, encoder, nullptr)) < 0)
		return err;
	mix.to = avformat_new_stream(out, nullptr);
	if (!mix.to)
		return AVERROR(ENOMEM);
	if ((err = avcodec_parameters_from_context(mix.to->codecpar, mix.enc)) < 0)
		return err;
	mix.to->time_base = mix.enc->time_base;
	mix.chunk = mix.enc->frame_size > 0 ? mix.enc->frame_size : 1024;
	return 0;
}

static void writeSilence(AVAudioFifo *fifo, int channels, int64_t samples)
{
	static const std::vector<float> zeros(4096, 0.0f);
	std::vector<void *> planes((size_t)channels, (void *)zeros.data());
	while (samples > 0) {
		const int now = (int)FFMIN(samples, (int64_t)zeros.size());
		av_audio_fifo_write(fifo, planes.data(), now);
		samples -= now;
	}
}

/** Sends a mixed chunk on: to the encoder, or to the meter. */
static int emitMixed(Mixer &mix, int samples, AVFormatContext *out, AVPacket *pkt)
{
	std::vector<float *> planes((size_t)mix.channels);
	for (int c = 0; c < mix.channels; c++)
		planes[c] = mix.mixed[c].data();
	if (mix.meter)
		mix.meter->add(planes.data(), samples);
	if (!mix.enc)
		return 0;
	AVFrame *frame = av_frame_alloc();
	frame->nb_samples = samples;
	frame->format = mix.enc->sample_fmt;
	frame->sample_rate = mix.enc->sample_rate;
	int err = av_channel_layout_copy(&frame->ch_layout, &mix.enc->ch_layout);
	if (err >= 0)
		err = av_frame_get_buffer(frame, 0);
	if (err < 0) {
		av_frame_free(&frame);
		return err;
	}
	for (int c = 0; c < mix.channels; c++)
		memcpy(frame->extended_data[c], planes[c], sizeof(float) * samples);
	frame->pts = mix.nextPts;
	mix.nextPts += samples;
	err = avcodec_send_frame(mix.enc, frame);
	av_frame_free(&frame);
	return err < 0 ? err : drainEncoder(mix.enc, out, mix.to, pkt);
}

/** Mixes whatever every track has ready. At the end, shorter tracks are padded with silence. */
static int drainMixer(Mixer &mix, bool finishing, AVFormatContext *out, AVPacket *pkt)
{
	if (mix.inputs.empty())
		return 0;
	if (mix.mixed.size() != (size_t)mix.channels) {
		mix.mixed.assign(mix.channels, std::vector<float>(mix.chunk));
		mix.scratch.assign(mix.channels, std::vector<float>(mix.chunk));
	}
	for (;;) {
		int most = 0;
		for (const MixInput &input : mix.inputs)
			most = FFMAX(most, av_audio_fifo_size(input.fifo));
		for (MixInput &input : mix.inputs) {
			if (!input.started && most > (int64_t)mix.rate * SILENT_AFTER_SECONDS) {
				input.started = true;
				input.silent = true;
			}
			if (input.silent || finishing)
				writeSilence(input.fifo, mix.channels, most - av_audio_fifo_size(input.fifo));
		}
		int ready = most;
		for (const MixInput &input : mix.inputs)
			ready = FFMIN(ready, av_audio_fifo_size(input.fifo));
		if (ready <= 0 || (ready < mix.chunk && !finishing))
			return 0;
		const int samples = FFMIN(ready, mix.chunk);
		for (int c = 0; c < mix.channels; c++)
			std::fill(mix.mixed[c].begin(), mix.mixed[c].begin() + samples, 0.0f);
		std::vector<void *> into((size_t)mix.channels);
		for (int c = 0; c < mix.channels; c++)
			into[c] = mix.scratch[c].data();
		for (MixInput &input : mix.inputs) {
			av_audio_fifo_read(input.fifo, into.data(), samples);
			for (int c = 0; c < mix.channels; c++) {
				for (int i = 0; i < samples; i++)
					mix.mixed[c][i] += mix.scratch[c][i];
			}
		}
		for (int i = 0; i < samples; i++) {
			double peak = 0;
			for (int c = 0; c < mix.channels; c++) {
				mix.mixed[c][i] = (float)(mix.mixed[c][i] * mix.level);
				peak = FFMAX(peak, fabs(mix.mixed[c][i]));
			}
			if (!mix.limit)
				continue;
			// Turns down at once for a peak, then comes back up smoothly, so it doesn't pump or crackle.
			const double wanted = peak > LIMIT_CEILING ? LIMIT_CEILING / peak : 1.0;
			mix.reduction = wanted < mix.reduction ? wanted : wanted - (wanted - mix.reduction) * mix.release;
			for (int c = 0; c < mix.channels; c++)
				mix.mixed[c][i] = (float)FFMIN(1.0, FFMAX(-1.0, mix.mixed[c][i] * mix.reduction));
		}
		const int err = emitMixed(mix, samples, out, pkt);
		if (err < 0)
			return err;
	}
}

/** Takes a packet from one of the mixed tracks, or nullptr for the end of that track. */
static int feedMixer(Mixer &mix, int streamIndex, const AVPacket *packet, AVFormatContext *out, AVPacket *pkt)
{
	MixInput &input = mix.inputs[mix.inputOf[streamIndex]];
	if (input.silent)
		return 0;
	int err = avcodec_send_packet(input.dec, packet);
	if (err < 0 && err != AVERROR_EOF && err != AVERROR_INVALIDDATA)
		return err;
	while ((err = avcodec_receive_frame(input.dec, mix.decoded)) >= 0) {
		AVFrame *frame = mix.decoded;
		if (frame->format != AV_SAMPLE_FMT_FLTP || frame->ch_layout.nb_channels != mix.channels) {
			av_frame_unref(frame);
			return AVERROR(EINVAL);
		}
		const int64_t micro = frame->pts == AV_NOPTS_VALUE ? mix.originMicro : av_rescale_q(frame->pts, input.from->time_base, {1, AV_TIME_BASE});
		if (micro < mix.originMicro) {
			av_frame_unref(frame);
			continue;
		}
		if (!input.started) {
			input.started = true;
			writeSilence(input.fifo, mix.channels, av_rescale(micro - mix.originMicro, mix.rate, AV_TIME_BASE));
		}
		av_audio_fifo_write(input.fifo, (void **)frame->extended_data, frame->nb_samples);
		av_frame_unref(frame);
	}
	if (err != AVERROR(EAGAIN) && err != AVERROR_EOF)
		return err;
	return drainMixer(mix, false, out, pkt);
}

static int finishMixer(Mixer &mix, AVFormatContext *out, AVPacket *pkt)
{
	for (MixInput &input : mix.inputs) {
		int err = avcodec_send_packet(input.dec, nullptr);
		while (err >= 0 && (err = avcodec_receive_frame(input.dec, mix.decoded)) >= 0) {
			if (!input.silent && mix.decoded->format == AV_SAMPLE_FMT_FLTP && mix.decoded->ch_layout.nb_channels == mix.channels)
				av_audio_fifo_write(input.fifo, (void **)mix.decoded->extended_data, mix.decoded->nb_samples);
			av_frame_unref(mix.decoded);
		}
	}
	int err = drainMixer(mix, true, out, pkt);
	if (err < 0 || !mix.enc)
		return err;
	if ((err = avcodec_send_frame(mix.enc, nullptr)) < 0)
		return err;
	return drainEncoder(mix.enc, out, mix.to, pkt);
}

static void freeMixer(Mixer &mix)
{
	for (MixInput &input : mix.inputs) {
		avcodec_free_context(&input.dec);
		if (input.fifo)
			av_audio_fifo_free(input.fifo);
	}
	mix.inputs.clear();
	avcodec_free_context(&mix.enc);
	av_frame_free(&mix.decoded);
	av_channel_layout_uninit(&mix.layout);
}

/** How loud the chosen tracks are together, in LUFS, from a quick pass over just the audio. */
static double measureLoudness(const std::string &input, int64_t originMicro)
{
	AVFormatContext *in = openInput(input);
	if (!in)
		return -INFINITY;
	Mixer mix;
	LoudnessMeter meter;
	double loudness = -INFINITY;
	if (openMixer(mix, in, g_share.tracks, originMicro) > 0) {
		meter.open(mix.rate, mix.channels);
		mix.meter = &meter;
		mix.chunk = 4096;
		AVPacket *read = av_packet_alloc();
		bool ok = true;
		while (ok && av_read_frame(in, read) >= 0) {
			if (mix.inputOf[read->stream_index] >= 0)
				ok = feedMixer(mix, read->stream_index, read, nullptr, nullptr) >= 0;
			av_packet_unref(read);
		}
		if (ok && finishMixer(mix, nullptr, nullptr) >= 0)
			loudness = meter.integrated();
		av_packet_free(&read);
	}
	freeMixer(mix);
	avformat_close_input(&in);
	return loudness;
}

/** The gain that brings a mix this loud to the target, kept within limits, so silence isn't turned into hiss. */
static double evenLevel(double loudness)
{
	if (!std::isfinite(loudness) || loudness < -60.0)
		return 1.0;
	const double db = FFMIN(EVEN_MOST_BOOST_DB, FFMAX(EVEN_MOST_CUT_DB, EVEN_TARGET_LUFS - loudness));
	return pow(10.0, db / 20.0);
}

static int openMp4(AVFormatContext *&out, const std::string &output)
{
	return avformat_alloc_output_context2(&out, nullptr, "mp4", output.c_str());
}

static int beginMp4(AVFormatContext *out, const std::string &output)
{
	int err = avio_open(&out->pb, output.c_str(), AVIO_FLAG_WRITE);
	if (err < 0)
		return err;
	AVDictionary *options = nullptr;
	av_dict_set(&options, "movflags", "+faststart", 0);
	err = avformat_write_header(out, &options);
	av_dict_free(&options);
	return err;
}

static int cmdRetrack(const std::string &input, const std::string &output, const std::vector<double> &gains)
{
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	AVFormatContext *out = nullptr;
	int err = openMp4(out, output);
	if (err < 0)
		return fail("Could not create output", err);

	std::vector<int> copyTo(in->nb_streams, -1);
	std::vector<int> encodeTo(in->nb_streams, -1);
	std::vector<AudioTrack> tracks;
	tracks.reserve(in->nb_streams);
	size_t audioIndex = 0;
	for (unsigned i = 0; i < in->nb_streams; i++) {
		AVStream *from = in->streams[i];
		const AVMediaType type = from->codecpar->codec_type;
		if (type != AVMEDIA_TYPE_VIDEO && type != AVMEDIA_TYPE_AUDIO)
			continue;
		const double gain = type == AVMEDIA_TYPE_AUDIO && audioIndex < gains.size() ? gains[audioIndex] : 1.0;
		if (type == AVMEDIA_TYPE_AUDIO)
			audioIndex++;
		if (gain <= 0.0)
			continue;
		if (gain == 1.0) {
			AVStream *to = avformat_new_stream(out, nullptr);
			if (!to || (err = avcodec_parameters_copy(to->codecpar, from->codecpar)) < 0)
				return fail("Could not copy stream", err);
			to->codecpar->codec_tag = mp4Tag(to->codecpar);
			to->time_base = from->time_base;
			copyTo[i] = to->index;
			continue;
		}
		const int bitrate = from->codecpar->bit_rate > 0 ? (int)from->codecpar->bit_rate : 160000;
		tracks.emplace_back();
		if ((err = openAudioTrack(tracks.back(), from, out, bitrate, FFMIN(gain, 2.0), 0)) < 0)
			return fail("Could not set up an audio track", err);
		encodeTo[i] = (int)tracks.size() - 1;
	}
	if ((err = beginMp4(out, output)) < 0)
		return fail("Could not write header", err);

	AVPacket *read = av_packet_alloc();
	AVPacket *written = av_packet_alloc();
	while (av_read_frame(in, read) >= 0) {
		const int index = read->stream_index;
		if (encodeTo[index] >= 0) {
			if ((err = feedAudio(tracks[encodeTo[index]], read, out, written)) < 0)
				return fail("Could not change a track's volume", err);
		} else if (copyTo[index] >= 0 && read->dts != AV_NOPTS_VALUE) {
			AVStream *to = out->streams[copyTo[index]];
			av_packet_rescale_ts(read, in->streams[index]->time_base, to->time_base);
			read->stream_index = to->index;
			read->pos = -1;
			if ((err = av_interleaved_write_frame(out, read)) < 0)
				return fail("Could not write packet", err);
		}
		av_packet_unref(read);
	}
	for (AudioTrack &track : tracks) {
		if ((err = finishAudio(track, out, written)) < 0)
			return fail("Could not finish a track", err);
		freeAudio(track);
	}
	if ((err = av_write_trailer(out)) < 0)
		return fail("Could not finish file", err);
	av_packet_free(&read);
	av_packet_free(&written);
	avio_closep(&out->pb);
	avformat_free_context(out);
	avformat_close_input(&in);
	return 0;
}

/** Tries the GPU first, because it's fast and leaves the game alone, then the CPU. */
static AVCodecContext *openH264Encoder(int width, int height, int fps, int kbps, bool globalHeader, const char *&chosen)
{
	for (const char *name : {"h264_nvenc", "h264_amf", "h264_qsv", "libx264"}) {
		const AVCodec *codec = avcodec_find_encoder_by_name(name);
		if (!codec)
			continue;
		AVCodecContext *enc = avcodec_alloc_context3(codec);
		enc->width = width;
		enc->height = height;
		enc->pix_fmt = AV_PIX_FMT_YUV420P;
		enc->time_base = {1, fps};
		enc->framerate = {fps, 1};
		enc->gop_size = fps * 2;
		enc->max_b_frames = 0;
		enc->bit_rate = (int64_t)kbps * 1000;
		enc->rc_max_rate = enc->bit_rate * 13 / 10;
		enc->rc_buffer_size = (int)(enc->bit_rate * 2);
		if (globalHeader)
			enc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
		if (strcmp(name, "h264_nvenc") == 0) {
			av_opt_set(enc->priv_data, "preset", "p5", 0);
			av_opt_set(enc->priv_data, "rc", "vbr", 0);
		} else if (strcmp(name, "libx264") == 0) {
			av_opt_set(enc->priv_data, "preset", "veryfast", 0);
		}
		if (avcodec_open2(enc, codec, nullptr) >= 0) {
			chosen = name;
			return enc;
		}
		avcodec_free_context(&enc);
	}
	return nullptr;
}

/** A vertical copy keeps the middle of the picture at 9:16. */
static int verticalWidth(int height)
{
	return FFMAX(2, height * 9 / 16 / 2 * 2);
}

/** Cuts a part out of a frame without copying it. Every edge is kept even, which 4:2:0 needs. */
static AVFrame *cropped(const AVFrame *frame, int x, int y, int width, int height)
{
	AVFrame *part = av_frame_clone(frame);
	if (!part)
		return nullptr;
	x = FFMAX(0, FFMIN(x & ~1, frame->width - 2));
	y = FFMAX(0, FFMIN(y & ~1, frame->height - 2));
	width = FFMAX(2, FFMIN(width & ~1, frame->width - x));
	height = FFMAX(2, FFMIN(height & ~1, frame->height - y));
	part->crop_left = x;
	part->crop_top = y;
	part->crop_right = frame->width - x - width;
	part->crop_bottom = frame->height - y - height;
	if (av_frame_apply_cropping(part, AV_FRAME_CROP_UNALIGNED) < 0)
		av_frame_free(&part);
	return part;
}

/**
 * Draws a vertical face-cam picture: the webcam across the top, as wide as the picture, and the middle of the
 * game filling the rest. camera is the webcam's place in the frame, as fractions.
 */
static int drawFaceCam(const AVFrame *frame, const double *camera, AVFrame *out, SwsContext *&top, SwsContext *&bottom)
{
	const int camX = (int)(camera[0] * frame->width), camY = (int)(camera[1] * frame->height);
	const int camW = FFMAX(2, (int)(camera[2] * frame->width)), camH = FFMAX(2, (int)(camera[3] * frame->height));
	const int topHeight = FFMIN((int)((int64_t)out->width * camH / camW), out->height * 45 / 100) & ~1;
	const int bottomHeight = out->height - topHeight;
	// The middle of the game, in the shape of the space left for it.
	const int keptWidth = FFMIN(frame->width, (int)((int64_t)frame->height * out->width / bottomHeight));
	const int keptHeight = FFMIN(frame->height, (int)((int64_t)keptWidth * bottomHeight / out->width));
	AVFrame *cam = cropped(frame, camX, camY, camW, camH);
	AVFrame *game = cropped(frame, (frame->width - keptWidth) / 2, (frame->height - keptHeight) / 2, keptWidth, keptHeight);
	int result = cam && game ? 0 : AVERROR(ENOMEM);
	if (result == 0) {
		top = sws_getCachedContext(top, cam->width, cam->height, (AVPixelFormat)cam->format, out->width, topHeight, (AVPixelFormat)out->format,
					   SWS_BICUBIC, nullptr, nullptr, nullptr);
		bottom = sws_getCachedContext(bottom, game->width, game->height, (AVPixelFormat)game->format, out->width, bottomHeight,
					      (AVPixelFormat)out->format, SWS_BICUBIC, nullptr, nullptr, nullptr);
		if (!top || !bottom) {
			result = AVERROR(EINVAL);
		} else {
			uint8_t *upper[4] = {out->data[0], out->data[1], out->data[2], nullptr};
			uint8_t *lower[4] = {out->data[0] + (size_t)topHeight * out->linesize[0], out->data[1] + (size_t)topHeight / 2 * out->linesize[1],
					     out->data[2] + (size_t)topHeight / 2 * out->linesize[2], nullptr};
			sws_scale(top, cam->data, cam->linesize, 0, cam->height, upper, out->linesize);
			sws_scale(bottom, game->data, game->linesize, 0, game->height, lower, out->linesize);
		}
	}
	av_frame_free(&cam);
	av_frame_free(&game);
	return result;
}

static int encodeShrunk(const std::string &input, const std::string &output, int videoKbps, int audioKbps, int height, int fps, bool vertical = false,
			const double *camera = nullptr)
{
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	const AVCodec *decoder = nullptr;
	const int video = av_find_best_stream(in, AVMEDIA_TYPE_VIDEO, -1, -1, &decoder, 0);
	if (video < 0 || !decoder)
		return fail("No decodable video stream");
	AVStream *source = in->streams[video];
	AVCodecContext *dec = avcodec_alloc_context3(decoder);
	avcodec_parameters_to_context(dec, source->codecpar);
	int err = avcodec_open2(dec, decoder, nullptr);
	if (err < 0)
		return fail("Could not open decoder", err);

	const int width = vertical ? verticalWidth(height) : FFMAX(2, (int)((int64_t)source->codecpar->width * height / source->codecpar->height / 2) * 2);
	AVFormatContext *out = nullptr;
	if ((err = openMp4(out, output)) < 0)
		return fail("Could not create output", err);
	const char *encoderName = "";
	AVCodecContext *enc = openH264Encoder(width, height, fps, videoKbps, out->oformat->flags & AVFMT_GLOBALHEADER, encoderName);
	if (!enc)
		return fail("No H.264 encoder could start");
	AVStream *videoOut = avformat_new_stream(out, nullptr);
	if (!videoOut || (err = avcodec_parameters_from_context(videoOut->codecpar, enc)) < 0)
		return fail("Could not create the video stream", err);
	videoOut->time_base = enc->time_base;

	const int64_t startMicro = source->start_time == AV_NOPTS_VALUE ? 0 : av_rescale_q(source->start_time, source->time_base, {1, AV_TIME_BASE});
	Mixer mix;
	const bool audio = audioKbps > 0 && openMixer(mix, in, g_share.tracks, startMicro) > 0;
	if (audio) {
		if ((err = openMixEncoder(mix, out, audioKbps * 1000)) < 0)
			return fail("Could not set up audio", err);
		if (g_share.even)
			mix.level = evenLevel(measureLoudness(input, startMicro));
		// Tracks added together, or turned up, can go over the top.
		mix.limit = mix.inputs.size() > 1 || mix.level > 1.0;
	}
	if ((err = beginMp4(out, output)) < 0)
		return fail("Could not write header", err);

	AVFrame *scaled = av_frame_alloc();
	scaled->format = enc->pix_fmt;
	scaled->width = width;
	scaled->height = height;
	if ((err = av_frame_get_buffer(scaled, 0)) < 0)
		return fail("Could not allocate frame", err);
	AVFrame *frame = av_frame_alloc();
	AVPacket *read = av_packet_alloc();
	AVPacket *written = av_packet_alloc();
	SwsContext *sws = nullptr, *camSws = nullptr;
	int64_t lastIndex = -1;

	const auto drainDecoder = [&]() -> int {
		int result;
		while ((result = avcodec_receive_frame(dec, frame)) >= 0) {
			const int64_t micro = av_rescale_q(frame->pts, source->time_base, {1, AV_TIME_BASE}) - startMicro;
			const int64_t index = (micro * fps + AV_TIME_BASE / 2) / AV_TIME_BASE;
			// Lowering the frame rate means leaving frames out.
			if (micro >= 0 && index > lastIndex && vertical && camera) {
				if ((result = av_frame_make_writable(scaled)) < 0 || (result = drawFaceCam(frame, camera, scaled, camSws, sws)) < 0)
					return result;
				scaled->pts = index;
				lastIndex = index;
				if ((result = avcodec_send_frame(enc, scaled)) < 0 ||
				    (result = drainEncoder(enc, out, videoOut, written)) < 0)
					return result;
			} else if (micro >= 0 && index > lastIndex) {
				if (vertical) {
					const int kept = FFMIN(frame->width, verticalWidth(frame->height));
					frame->crop_left = (frame->width - kept) / 2 / 2 * 2;
					frame->crop_right = frame->width - kept - frame->crop_left;
					if ((result = av_frame_apply_cropping(frame, AV_FRAME_CROP_UNALIGNED)) < 0)
						return result;
				}
				sws = sws_getCachedContext(sws, frame->width, frame->height, (AVPixelFormat)frame->format, width, height,
							   enc->pix_fmt, SWS_BICUBIC, nullptr, nullptr, nullptr);
				if (!sws)
					return AVERROR(EINVAL);
				if ((result = av_frame_make_writable(scaled)) < 0)
					return result;
				sws_scale(sws, frame->data, frame->linesize, 0, frame->height, scaled->data, scaled->linesize);
				scaled->pts = index;
				lastIndex = index;
				if ((result = avcodec_send_frame(enc, scaled)) < 0 ||
				    (result = drainEncoder(enc, out, videoOut, written)) < 0)
					return result;
			}
			av_frame_unref(frame);
		}
		return result == AVERROR(EAGAIN) || result == AVERROR_EOF ? 0 : result;
	};

	while (av_read_frame(in, read) >= 0) {
		if (read->stream_index == video) {
			if (avcodec_send_packet(dec, read) >= 0 && (err = drainDecoder()) < 0)
				return fail("Could not encode the video", err);
		} else if (audio && mix.inputOf[read->stream_index] >= 0) {
			if ((err = feedMixer(mix, read->stream_index, read, out, written)) < 0)
				return fail("Could not encode the audio", err);
		}
		av_packet_unref(read);
	}
	avcodec_send_packet(dec, nullptr);
	if ((err = drainDecoder()) < 0 || (err = avcodec_send_frame(enc, nullptr)) < 0 || (err = drainEncoder(enc, out, videoOut, written)) < 0)
		return fail("Could not finish the video", err);
	if (audio && (err = finishMixer(mix, out, written)) < 0)
		return fail("Could not finish the audio", err);
	if ((err = av_write_trailer(out)) < 0)
		return fail("Could not finish file", err);

	sws_freeContext(sws);
	sws_freeContext(camSws);
	av_frame_free(&scaled);
	av_frame_free(&frame);
	av_packet_free(&read);
	av_packet_free(&written);
	freeMixer(mix);
	avcodec_free_context(&enc);
	avcodec_free_context(&dec);
	avio_closep(&out->pb);
	avformat_free_context(out);
	avformat_close_input(&in);
	return 0;
}

static int64_t fileSize(const std::string &path)
{
	WIN32_FILE_ATTRIBUTE_DATA data;
	if (!GetFileAttributesExW(toWide(path).c_str(), GetFileExInfoStandard, &data))
		return -1;
	return ((int64_t)data.nFileSizeHigh << 32) | data.nFileSizeLow;
}

/** Fewer pixels look far better than too few bits per pixel. */
static int heightForBitrate(int sourceHeight, int videoKbps)
{
	const int wanted = videoKbps >= 3500 ? 1080 : videoKbps >= 1500 ? 720 : videoKbps >= 600 ? 480 : 360;
	return FFMIN(sourceHeight, wanted) & ~1;
}

static int fpsForBitrate(int sourceFps, int videoKbps)
{
	return FFMAX(1, FFMIN(FFMIN(sourceFps, 60), videoKbps < 1500 ? 30 : 60));
}

/** Aims for a size at up to 1080p60, but a long clip stops at a good quality instead, so it can end up bigger. Prints the final size. */
static int cmdCompress(const std::string &input, const std::string &output, int64_t targetBytes)
{
	const int MOST_HEIGHT = 1080;
	const int MOST_FPS = 60;
	const double GOOD_KBPS_AT_MOST = 4000;
	const int LOWEST_GOOD_KBPS = 500;
	const int MOST_VIDEO_KBPS = 12000;
	const int AUDIO_KBPS = 128;
	const int MAX_ATTEMPTS = 3;
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	const int video = av_find_best_stream(in, AVMEDIA_TYPE_VIDEO, -1, -1, nullptr, 0);
	if (video < 0 || in->duration <= 0)
		return fail("No video stream");
	const double seconds = (double)in->duration / AV_TIME_BASE;
	const int sourceWidth = in->streams[video]->codecpar->width;
	const int sourceHeight = in->streams[video]->codecpar->height;
	const AVRational rate = av_guess_frame_rate(in, in->streams[video], nullptr);
	const int sourceFps = rate.den > 0 && rate.num > 0 ? (int)(av_q2d(rate) + 0.5) : MOST_FPS;
	const bool hasAudio = av_find_best_stream(in, AVMEDIA_TYPE_AUDIO, -1, -1, nullptr, 0) >= 0;
	avformat_close_input(&in);

	const int height = FFMIN(sourceHeight, MOST_HEIGHT) & ~1;
	const int fps = FFMAX(1, FFMIN(sourceFps, MOST_FPS));
	// A smaller or slower picture needs fewer bits to look just as good.
	const double share = (double)sourceWidth * height / sourceHeight * height * fps / (1920.0 * 1080.0 * MOST_FPS);
	const int goodKbps = FFMAX(LOWEST_GOOD_KBPS, (int)(GOOD_KBPS_AT_MOST * share));
	const int64_t sourceBytes = fileSize(input);
	const double sourceKbps = sourceBytes * 8.0 / 1000.0 / seconds;
	const char *CANT_SHRINK = "This clip can't get much smaller without looking worse.\n";
	if (sourceKbps < goodKbps * 1.25) {
		fprintf(stderr, "%s", CANT_SHRINK);
		return 4;
	}
	const int audioKbps = hasAudio ? AUDIO_KBPS : 0;
	const double totalKbps = targetBytes * 8.0 / 1000.0 * 0.96 / seconds;
	// A short clip has room to spare, but the copy should still come out well under the original.
	int videoKbps = FFMAX(goodKbps, (int)FFMIN(FFMIN(totalKbps - audioKbps, (double)MOST_VIDEO_KBPS), sourceKbps * 0.6));

	for (int attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
		const int result = encodeShrunk(input, output, videoKbps, audioKbps, height, fps);
		if (result != 0)
			return result;
		const int64_t size = fileSize(output);
		if (size <= targetBytes || videoKbps <= goodKbps || attempt == MAX_ATTEMPTS - 1) {
			if (size < 0 || size >= sourceBytes) {
				fprintf(stderr, "%s", CANT_SHRINK);
				return 4;
			}
			printf("%lld\n", (long long)size);
			fflush(stdout);
			return 0;
		}
		videoKbps = FFMAX(goodKbps, (int)(videoKbps * ((double)targetBytes / (double)size) * 0.92));
	}
	return fail("Could not compress this clip");
}

/** Fits a size by working out a bitrate, then trying again lower if the encoder overshot. */
static int cmdShrink(const std::string &input, const std::string &output, int64_t targetBytes)
{
	const int MIN_VIDEO_KBPS = 150;
	const int MAX_ATTEMPTS = 3;
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	const int video = av_find_best_stream(in, AVMEDIA_TYPE_VIDEO, -1, -1, nullptr, 0);
	if (video < 0 || in->duration <= 0)
		return fail("No video stream");
	const double seconds = (double)in->duration / AV_TIME_BASE;
	const int sourceHeight = in->streams[video]->codecpar->height;
	const AVRational rate = av_guess_frame_rate(in, in->streams[video], nullptr);
	const int sourceFps = rate.den > 0 && rate.num > 0 ? (int)(av_q2d(rate) + 0.5) : 60;
	const bool hasAudio = av_find_best_stream(in, AVMEDIA_TYPE_AUDIO, -1, -1, nullptr, 0) >= 0;
	avformat_close_input(&in);

	// A little is held back for the file's own structure.
	const double totalKbps = targetBytes * 8.0 / 1000.0 * 0.96 / seconds;
	const int audioKbps = hasAudio ? (totalKbps < 600 ? 64 : 128) : 0;
	int videoKbps = (int)(totalKbps - audioKbps);
	if (videoKbps < MIN_VIDEO_KBPS) {
		fprintf(stderr, "This clip is too long to fit that size and still look watchable. Trim it first, or pick a larger size.\n");
		return 4;
	}
	const int height = heightForBitrate(sourceHeight, videoKbps);
	const int fps = fpsForBitrate(sourceFps, videoKbps);

	for (int attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
		const int result = encodeShrunk(input, output, videoKbps, audioKbps, height, fps);
		if (result != 0)
			return result;
		const int64_t size = fileSize(output);
		if (size >= 0 && size <= targetBytes) {
			printf("%lld\n", (long long)size);
			fflush(stdout);
			return 0;
		}
		videoKbps = (int)(videoKbps * ((double)targetBytes / (double)size) * 0.92);
		if (videoKbps < MIN_VIDEO_KBPS)
			break;
	}
	fprintf(stderr, "Couldn't fit this clip into that size. Trim it first, or pick a larger size.\n");
	return 4;
}

/**
 * Makes a 9:16 copy of a wide clip for phones, keeping as many of the original pixels as it can. With a webcam's
 * place given, the webcam goes across the top and the game below it.
 */
static int cmdVertical(const std::string &input, const std::string &output, const double *camera)
{
	const int MAX_HEIGHT = 1920;
	const double BITS_PER_PIXEL = 0.12;
	const int MIN_KBPS = 2500;
	const int MAX_KBPS = 20000;
	const int AUDIO_KBPS = 160;
	AVFormatContext *in = openInput(input);
	if (!in)
		return fail("Could not open input");
	const int video = av_find_best_stream(in, AVMEDIA_TYPE_VIDEO, -1, -1, nullptr, 0);
	if (video < 0)
		return fail("No video stream");
	const int sourceWidth = in->streams[video]->codecpar->width;
	const int sourceHeight = in->streams[video]->codecpar->height;
	const AVRational rate = av_guess_frame_rate(in, in->streams[video], nullptr);
	const int fps = rate.den > 0 && rate.num > 0 ? FFMAX(1, FFMIN((int)(av_q2d(rate) + 0.5), 60)) : 60;
	avformat_close_input(&in);
	if ((int64_t)sourceWidth * 16 <= (int64_t)sourceHeight * 9) {
		fprintf(stderr, "This clip is already vertical.\n");
		return 4;
	}
	const int height = FFMIN(sourceHeight, MAX_HEIGHT) & ~1;
	const int kbps = (int)((double)verticalWidth(height) * height * fps * BITS_PER_PIXEL / 1000.0);
	return encodeShrunk(input, output, FFMAX(MIN_KBPS, FFMIN(kbps, MAX_KBPS)), AUDIO_KBPS, height, fps, true, camera);
}

/** Turns the frame the engine saved, as raw NV12 in video levels, into a PNG. */
static int cmdPng(const std::string &input, const std::string &output, int width, int height)
{
	const int64_t expected = (int64_t)width * height * 3 / 2;
	std::vector<uint8_t> raw((size_t)FFMAX(expected, 0));
	FILE *in = _wfopen(toWide(input).c_str(), L"rb");
	if (!in)
		return fail("Could not open the captured frame");
	const size_t got = fread(raw.data(), 1, raw.size(), in);
	fclose(in);
	if (width <= 0 || height <= 0 || (int64_t)got != expected) {
		fprintf(stderr, "The captured frame is the wrong size (%lld bytes for %dx%d)\n", (long long)got, width, height);
		return 1;
	}

	const AVCodec *codec = avcodec_find_encoder(AV_CODEC_ID_PNG);
	if (!codec)
		return fail("No PNG encoder");
	AVCodecContext *enc = avcodec_alloc_context3(codec);
	enc->width = width;
	enc->height = height;
	enc->pix_fmt = AV_PIX_FMT_RGB24;
	enc->time_base = {1, 25};
	int err = avcodec_open2(enc, codec, nullptr);
	if (err < 0)
		return fail("Could not open PNG encoder", err);

	AVFrame *picture = av_frame_alloc();
	picture->format = enc->pix_fmt;
	picture->width = width;
	picture->height = height;
	picture->pts = 0;
	if ((err = av_frame_get_buffer(picture, 0)) < 0)
		return fail("Could not allocate frame", err);
	SwsContext *sws = sws_getContext(width, height, AV_PIX_FMT_NV12, width, height, AV_PIX_FMT_RGB24, SWS_BICUBIC | SWS_ACCURATE_RND | SWS_FULL_CHR_H_INT, nullptr, nullptr, nullptr);
	if (!sws)
		return fail("Could not create scaler");
	// The engine captures in BT.709 with video levels, and a picture needs full levels.
	sws_setColorspaceDetails(sws, sws_getCoefficients(SWS_CS_ITU709), 0, sws_getCoefficients(SWS_CS_DEFAULT), 1, 0, 1 << 16, 1 << 16);
	const uint8_t *planes[4] = {raw.data(), raw.data() + (size_t)width * height, nullptr, nullptr};
	const int strides[4] = {width, width, 0, 0};
	sws_scale(sws, planes, strides, 0, height, picture->data, picture->linesize);
	sws_freeContext(sws);

	AVPacket *pkt = av_packet_alloc();
	if ((err = avcodec_send_frame(enc, picture)) < 0 || (err = avcodec_receive_packet(enc, pkt)) < 0)
		return fail("Could not encode the PNG", err);
	FILE *out = _wfopen(toWide(output).c_str(), L"wb");
	if (!out)
		return fail("Could not create the PNG");
	const bool written = fwrite(pkt->data, 1, pkt->size, out) == (size_t)pkt->size;
	fclose(out);
	av_packet_free(&pkt);
	av_frame_free(&picture);
	avcodec_free_context(&enc);
	return written ? 0 : fail("Could not write the PNG");
}

/** Takes the --options out, wherever they are, and leaves the rest in order. */
static std::vector<wchar_t *> readOptions(int count, wchar_t **all)
{
	std::vector<wchar_t *> rest;
	for (int i = 0; i < count; i++) {
		const std::wstring arg = all[i];
		if (arg == L"--even") {
			g_share.even = true;
		} else if (arg.rfind(L"--tracks=", 0) == 0) {
			const std::string list = toUtf8(arg.c_str() + 9);
			size_t at = 0;
			while (at <= list.size()) {
				const size_t comma = list.find(',', at);
				const std::string part = list.substr(at, comma == std::string::npos ? std::string::npos : comma - at);
				if (!part.empty())
					g_share.tracks.push_back(atoi(part.c_str()));
				if (comma == std::string::npos)
					break;
				at = comma + 1;
			}
		} else {
			rest.push_back(all[i]);
		}
	}
	return rest;
}

int main()
{
	int count = 0;
	wchar_t **all = CommandLineToArgvW(GetCommandLineW(), &count);
	std::vector<wchar_t *> kept = all ? readOptions(count, all) : std::vector<wchar_t *>();
	const int argc = (int)kept.size();
	wchar_t **argv = kept.data();
	av_log_set_level(AV_LOG_ERROR);
	if (argv && argc >= 3) {
		const std::wstring cmd = argv[1];
		const std::string in = toUtf8(argv[2]);
		if (cmd == L"duration")
			return cmdDuration(in);
		if (cmd == L"trim" && argc >= 5)
			return cmdTrim(in, toUtf8(argv[3]), _wtof(argv[4]));
		if (cmd == L"retag" && argc >= 4)
			return cmdRetag(in, toUtf8(argv[3]));
		if (cmd == L"thumb" && argc >= 4)
			return cmdThumb(in, toUtf8(argv[3]));
		if (cmd == L"tracks")
			return cmdTracks(in);
		if (cmd == L"shrink" && argc >= 5)
			return cmdShrink(in, toUtf8(argv[3]), _wtoi64(argv[4]));
		if (cmd == L"compress" && argc >= 5)
			return cmdCompress(in, toUtf8(argv[3]), _wtoi64(argv[4]));
		if (cmd == L"vertical" && argc >= 8) {
			const double camera[4] = {_wtof(argv[4]), _wtof(argv[5]), _wtof(argv[6]), _wtof(argv[7])};
			const bool usable = camera[2] > 0.0 && camera[3] > 0.0 && camera[0] >= 0.0 && camera[1] >= 0.0 && camera[0] + camera[2] <= 1.001 && camera[1] + camera[3] <= 1.001;
			return cmdVertical(in, toUtf8(argv[3]), usable ? camera : nullptr);
		}
		if (cmd == L"vertical" && argc >= 4)
			return cmdVertical(in, toUtf8(argv[3]), nullptr);
		if (cmd == L"strip" && argc >= 5)
			return cmdStrip(in, toUtf8(argv[3]), _wtoi(argv[4]));

		if (cmd == L"png" && argc >= 6)
			return cmdPng(in, toUtf8(argv[3]), _wtoi(argv[4]), _wtoi(argv[5]));
		if (cmd == L"retrack" && argc >= 5) {
			std::vector<double> gains;
			for (int i = 4; i < argc; i++)
				gains.push_back(_wtof(argv[i]));
			return cmdRetrack(in, toUtf8(argv[3]), gains);
		}
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
