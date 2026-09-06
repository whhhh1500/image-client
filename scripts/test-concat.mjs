// Node 版拼接验证：与 src/lib/media.ts 的 concatMp4 同一套逻辑。
// 用法: node scripts/test-concat.mjs <a.mp4> <b.mp4> ... <out.mp4>
import { readFileSync, writeFileSync } from "node:fs";
import {
  ALL_FORMATS,
  BlobSource,
  BufferTarget,
  EncodedAudioPacketSource,
  EncodedPacketSink,
  EncodedVideoPacketSource,
  Input,
  Mp4OutputFormat,
  Output,
} from "mediabunny";

const args = process.argv.slice(2);
if (args.length < 3) {
  console.error("usage: node test-concat.mjs <a.mp4> [b.mp4 ...] <out.mp4>");
  process.exit(1);
}
const inputs = args.slice(0, -1);
const outPath = args.at(-1);

const blobs = inputs.map((p) => new Blob([readFileSync(p)], { type: "video/mp4" }));
const readers = await Promise.all(
  blobs.map((b) => new Input({ source: new BlobSource(b), formats: ALL_FORMATS })),
);

const firstVideo = await readers[0].getPrimaryVideoTrack();
const firstAudio = await readers[0].getPrimaryAudioTrack();
const output = new Output({
  format: new Mp4OutputFormat({ fastStart: "in-memory" }),
  target: new BufferTarget(),
});

let videoSource = null;
if (firstVideo) {
  const codec = await firstVideo.getCodec();
  videoSource = new EncodedVideoPacketSource(codec);
  const stats = await firstVideo.computePacketStats(64);
  await output.addVideoTrack(videoSource, { frameRate: stats.averagePacketRate });
}
let audioSource = null;
if (firstAudio) {
  const codec = await firstAudio.getCodec();
  if (codec) {
    audioSource = new EncodedAudioPacketSource(codec);
    await output.addAudioTrack(audioSource);
  }
}

await output.start();
const videoMeta = firstVideo ? { decoderConfig: (await firstVideo.getDecoderConfig()) ?? undefined } : undefined;
const audioMeta = firstAudio ? { decoderConfig: (await firstAudio.getDecoderConfig()) ?? undefined } : undefined;

let offset = 0;
for (const input of readers) {
  const base = await input.getFirstTimestamp();
  if (videoSource) {
    const track = await input.getPrimaryVideoTrack();
    if (track) {
      const sink = new EncodedPacketSink(track);
      let meta = videoMeta;
      for await (const packet of sink.packets()) {
        await videoSource.add(packet.clone({ timestamp: packet.timestamp - base + offset }), meta);
        meta = undefined;
      }
    }
  }
  if (audioSource) {
    const track = await input.getPrimaryAudioTrack();
    if (track) {
      const sink = new EncodedPacketSink(track);
      let meta = audioMeta;
      for await (const packet of sink.packets()) {
        await audioSource.add(packet.clone({ timestamp: packet.timestamp - base + offset }), meta);
        meta = undefined;
      }
    }
  }
  offset += await input.computeDuration();
}

await output.finalize();
const buffer = output.target.buffer;
writeFileSync(outPath, new Uint8Array(buffer));

const check = new Input({ source: new BlobSource(new Blob([readFileSync(outPath)])), formats: ALL_FORMATS });
const duration = await check.computeDuration();
const v = await check.getPrimaryVideoTrack();
console.log(`OK -> ${outPath}`);
console.log(`duration=${duration.toFixed(2)}s codec=${v ? await v.getCodec() : "none"}`);
