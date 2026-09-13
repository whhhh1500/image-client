// Downloads a lean static ffmpeg for the CURRENT platform and
// places it in src-tauri/resources/ as a development resource. The current
// default installer does not bundle it until Rust media-edit commands exist.
//
// Tauri builds a desktop app for the OS you build on (no cross-OS compile), so
// running this script on each build machine fetches that platform's binaries —
// Windows/macOS/Linux, x64/arm64.
//
// ffmpeg-static (gyan essentials) 含 libx264/webp/aac，覆盖 concat、混音、
// 媒体编辑所需能力，体积约为 BtbN 全量构建的一半。ffprobe 未被代码使用，
// 不再打包。
import os from "node:os";
import path from "node:path";
import {
  copyFileSync,
  createReadStream,
  createWriteStream,
  existsSync,
  mkdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createGunzip } from "node:zlib";
import { pipeline } from "node:stream/promises";

const platform = os.platform(); // win32 | darwin | linux
const arch = os.arch(); // x64 | arm64

function assetFor(p, a) {
  const tag = "b6.0";
  const base = "https://github.com/eugeneware/ffmpeg-static/releases/download";
  if (p === "win32") {
    return { file: "ffmpeg-win32-x64.gz", url: `${base}/${tag}/ffmpeg-win32-x64.gz`, exe: "ffmpeg.exe" };
  }
  if (p === "darwin") {
    const name = a === "arm64" ? "ffmpeg-darwin-arm64.gz" : "ffmpeg-darwin-x64.gz";
    return { file: name, url: `${base}/${tag}/${name}`, exe: "ffmpeg" };
  }
  if (p === "linux") {
    const name = a === "arm64" ? "ffmpeg-linux-arm64.gz" : "ffmpeg-linux-x64.gz";
    return { file: name, url: `${base}/${tag}/${name}`, exe: "ffmpeg" };
  }
  throw new Error(`Unsupported platform: ${p}`);
}

const asset = assetFor(platform, arch);
const url = asset.url;

const resDir = path.join("src-tauri", "resources");
// Scratch stays next to the repository so downloads never fill the system drive.
const scratchRoot = path.join(process.cwd(), ".test-tmp");
mkdirSync(scratchRoot, { recursive: true });
const tmp = path.join(scratchRoot, `ffmpeg-dl-${Date.now()}`);
mkdirSync(tmp, { recursive: true });
const zip = path.join(tmp, asset.file);

console.log(`[ffmpeg] downloading ${url}`);
const resp = await fetch(url);
if (!resp.ok) {
  throw new Error(`download failed: HTTP ${resp.status}`);
}
const buf = Buffer.from(await resp.arrayBuffer());
writeFileSync(zip, buf);
console.log(`[ffmpeg] downloaded ${(buf.length / 1024 / 1024).toFixed(1)} MB`);

console.log("[ffmpeg] decompressing …");
const inStream = createReadStream(zip);
const gunzip = createGunzip();
const outPath = path.join(tmp, asset.exe);
await pipeline(inStream, gunzip, createWriteStream(outPath));

mkdirSync(resDir, { recursive: true });
copyFileSync(outPath, path.join(resDir, asset.exe));
rmSync(tmp, { recursive: true, force: true });

console.log(
  `[ffmpeg] done -> ${resDir}/${asset.exe} (${asset.file})`,
);
if (!existsSync(path.join(resDir, asset.exe))) {
  throw new Error("ffmpeg binary missing after extraction");
}
