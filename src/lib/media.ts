import {
  ALL_FORMATS,
  AudioBufferSource,
  BlobSource,
  BufferTarget,
  EncodedAudioPacketSource,
  EncodedPacketSink,
  EncodedVideoPacketSource,
  Input,
  Mp4OutputFormat,
  Output,
  canEncodeAudio,
} from "mediabunny";
import { logEvent } from "./logger";

function openInput(blob: Blob) {
  return new Input({ source: new BlobSource(blob), formats: ALL_FORMATS });
}

function mp4Output() {
  return new Output({ format: new Mp4OutputFormat({ fastStart: "in-memory" }), target: new BufferTarget() });
}

/**
 * 无损拼接同编码 MP4 分段（包级复制，不解码不重编码）。
 * 分段须来自同一视频模型（时间基/编码参数一致），这正是我们的场景。
 */
export async function concatMp4(blobs: Blob[]): Promise<Blob> {
  const started = performance.now();
  logEvent("debug", "media.concat.start", { inputCount: blobs.length, inputBytes: blobs.reduce((sum, blob) => sum + blob.size, 0) });
  if (blobs.length <= 1) return blobs[0] ?? new Blob([], { type: "video/mp4" });

  const inputs = await Promise.all(blobs.map((b) => openInput(b)));
  const first = inputs[0];
  const firstVideo = await first.getPrimaryVideoTrack();
  const firstAudio = await first.getPrimaryAudioTrack();

  const output = mp4Output();

  let videoSource: EncodedVideoPacketSource | null = null;
  if (firstVideo) {
    const codec = await firstVideo.getCodec();
    if (!codec) throw new Error("分段缺少视频编码信息");
    videoSource = new EncodedVideoPacketSource(codec);
    const stats = await firstVideo.computePacketStats(64);
    await output.addVideoTrack(videoSource, { frameRate: stats.averagePacketRate });
  }
  let audioSource: EncodedAudioPacketSource | null = null;
  if (firstAudio) {
    const codec = await firstAudio.getCodec();
    if (codec) {
      audioSource = new EncodedAudioPacketSource(codec);
      await output.addAudioTrack(audioSource);
    }
  }
  if (!videoSource && !audioSource) throw new Error("分段中没有可用的媒体轨");

  await output.start();

  const videoMeta = firstVideo
    ? { decoderConfig: (await firstVideo.getDecoderConfig()) ?? undefined }
    : undefined;
  const audioMeta = firstAudio
    ? { decoderConfig: (await firstAudio.getDecoderConfig()) ?? undefined }
    : undefined;

  let offset = 0;
  for (const input of inputs) {
    // 分段首包时间戳可能为负（AAC priming），先归零再叠加累计偏移。
    const base = await input.getFirstTimestamp();
    if (videoSource) {
      const track = await input.getPrimaryVideoTrack();
      if (track) {
        const reader = new EncodedPacketSink(track);
        let meta: typeof videoMeta = videoMeta;
        for await (const packet of reader.packets()) {
          await videoSource.add(packet.clone({ timestamp: packet.timestamp - base + offset }), meta);
          meta = undefined;
        }
      }
    }
    if (audioSource) {
      const track = await input.getPrimaryAudioTrack();
      if (track) {
        const reader = new EncodedPacketSink(track);
        let meta: typeof audioMeta = audioMeta;
        for await (const packet of reader.packets()) {
          await audioSource.add(packet.clone({ timestamp: packet.timestamp - base + offset }), meta);
          meta = undefined;
        }
      }
    }
    offset += await input.computeDuration();
  }

  await output.finalize();
  const buffer = output.target.buffer;
  if (!buffer) throw new Error("拼接输出为空");
  const result = new Blob([buffer], { type: "video/mp4" });
  logEvent("info", "media.concat.end", { status: "success", durationMs: performance.now() - started, outputBytes: result.size });
  return result;
}

/**
 * 混入背景音乐：视频轨包级复制，音频轨替换为音乐（与原 ffmpeg -shortest
 * 行为一致，音乐长度截到视频长度）。AAC 不可用时退回 Opus。
 */
export async function muxMusic(video: Blob, music: Blob): Promise<Blob> {
  const started = performance.now();
  logEvent("debug", "media.mux_music.start", { videoBytes: video.size, musicBytes: music.size });
  const input = await openInput(video);
  const videoTrack = await input.getPrimaryVideoTrack();
  if (!videoTrack) throw new Error("视频缺少画面轨");
  const codec = await videoTrack.getCodec();
  if (!codec) throw new Error("视频缺少编码信息");

  const audioCodec = (await canEncodeAudio("aac")) ? "aac" : (await canEncodeAudio("opus")) ? "opus" : null;
  if (!audioCodec) throw new Error("当前环境不支持音频编码（AAC/Opus 均不可用）");

  const audioCtx = new AudioContext();
  let musicBuf: AudioBuffer;
  let trimmed: AudioBuffer;
  try {
    musicBuf = await audioCtx.decodeAudioData(await music.arrayBuffer());
    const videoDuration = await input.computeDuration();
    const frames = videoDuration > 0
      ? Math.min(musicBuf.length, Math.floor(musicBuf.sampleRate * videoDuration))
      : musicBuf.length;
    trimmed = audioCtx.createBuffer(musicBuf.numberOfChannels, Math.max(1, frames), musicBuf.sampleRate);
    for (let c = 0; c < musicBuf.numberOfChannels; c++) {
      trimmed.copyToChannel(musicBuf.getChannelData(c).subarray(0, trimmed.length), c);
    }
  } finally {
    await audioCtx.close().catch(() => undefined);
  }

  const output = mp4Output();
  const videoSource = new EncodedVideoPacketSource(codec);
  const stats = await videoTrack.computePacketStats(64);
  await output.addVideoTrack(videoSource, { frameRate: stats.averagePacketRate });
  const audioSource = new AudioBufferSource({
    codec: audioCodec,
    bitrate: audioCodec === "aac" ? 128_000 : 96_000,
  });
  await output.addAudioTrack(audioSource);

  await output.start();

  const videoMeta: { decoderConfig?: VideoDecoderConfig } | undefined = {
    decoderConfig: (await videoTrack.getDecoderConfig()) ?? undefined,
  };
  const reader = new EncodedPacketSink(videoTrack);
  let meta: { decoderConfig?: VideoDecoderConfig } | undefined = videoMeta;
  for await (const packet of reader.packets()) {
    await videoSource.add(packet, meta);
    meta = undefined;
  }
  await audioSource.add(trimmed);
  audioSource.close();

  await output.finalize();
  const buffer = output.target.buffer;
  if (!buffer) throw new Error("混音输出为空");
  const result = new Blob([buffer], { type: "video/mp4" });
  logEvent("info", "media.mux_music.end", { status: "success", durationMs: performance.now() - started, outputBytes: result.size, audioCodec });
  return result;
}
