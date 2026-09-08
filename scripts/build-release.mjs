import { readFileSync, existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import process from "node:process";

const root = resolve(import.meta.dirname, "..");
const args = new Set(process.argv.slice(2));
const tagIndex = process.argv.indexOf("--tag");
const releaseTag = tagIndex >= 0 ? process.argv[tagIndex + 1] : undefined;

function readJson(path) {
  return JSON.parse(readFileSync(resolve(root, path), "utf8"));
}

function cargoVersion() {
  const cargo = readFileSync(resolve(root, "src-tauri/Cargo.toml"), "utf8");
  const packageBlock = cargo.match(/\[package\]([\s\S]*?)(?:\n\[|$)/)?.[1] ?? "";
  return packageBlock.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
}

function checkIcon(path) {
  const fullPath = resolve(root, path);
  if (!existsSync(fullPath)) throw new Error(`缺少应用图标：${path}`);
  const png = readFileSync(fullPath);
  if (png.subarray(0, 8).toString("hex") !== "89504e470d0a1a0a") throw new Error(`不是有效 PNG：${path}`);
  const width = png.readUInt32BE(16);
  const height = png.readUInt32BE(20);
  const colorType = png[25];
  if (width !== 1024 || height !== 1024) throw new Error(`主图标必须为 1024×1024：${path}`);
  if (![4, 6].includes(colorType)) throw new Error(`主图标必须带透明通道：${path}`);
}

function run(command, commandArgs) {
  const result = spawnSync(command, commandArgs, { cwd: root, stdio: "inherit", shell: false });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function runPnpm(commandArgs) {
  if (process.platform === "win32" && process.env.npm_execpath) {
    run(process.execPath, [process.env.npm_execpath, ...commandArgs]);
    return;
  }
  run("pnpm", commandArgs);
}

const packageJson = readJson("package.json");
const tauriConfig = readJson("src-tauri/tauri.conf.json");
const versions = [packageJson.version, cargoVersion(), tauriConfig.version];
if (versions.some((version) => version !== versions[0])) {
  throw new Error(`版本号不一致：package=${versions[0]} cargo=${versions[1]} tauri=${versions[2]}`);
}
if (releaseTag && releaseTag !== `v${versions[0]}`) {
  throw new Error(`标签 ${releaseTag} 与应用版本 v${versions[0]} 不一致`);
}

checkIcon("src-tauri/icons/app-icon-source.png");
for (const icon of tauriConfig.bundle.icon) {
  if (!existsSync(resolve(root, "src-tauri", icon))) throw new Error(`缺少打包图标：src-tauri/${icon}`);
}

console.log(`发布检查通过：Image-Client v${versions[0]}`);
if (args.has("--check-only")) process.exit(0);

runPnpm(["test"]);
run("cargo", ["test", "--locked", "--manifest-path", "src-tauri/Cargo.toml"]);

const localPlatform = process.platform === "win32" && process.arch === "x64"
  ? "windows-x64"
  : process.platform === "darwin" && process.arch === "arm64"
    ? "macos-arm64"
    : process.platform === "linux" && process.arch === "x64"
      ? "linux-x64"
      : undefined;
if (!localPlatform) {
  throw new Error("release:build 仅支持 Windows x64、macOS Apple Silicon 或 Linux x64；其他架构当前不作为发布目标");
}
const bundleArgs = localPlatform === "windows-x64"
  ? ["build", "--ci", "--no-bundle"]
  : localPlatform === "macos-arm64"
    ? ["build", "--ci", "--bundles", "app"]
    : ["build", "--ci", "--bundles", "appimage"];
runPnpm(["tauri", ...bundleArgs]);
run(process.execPath, ["scripts/package-release.mjs", "--platform", localPlatform]);
