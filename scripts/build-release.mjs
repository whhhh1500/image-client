import { readFileSync, existsSync, mkdirSync } from "node:fs";
import { spawn, spawnSync } from "node:child_process";
import { availableParallelism } from "node:os";
import { resolve } from "node:path";
import process from "node:process";

const root = resolve(import.meta.dirname, "..");

// Keep child-process scratch on the repository's drive. Go and the linker
// honour TEMP/TMP/GOTMPDIR; the Go build cache follows unless the developer
// already chose a location.
const scratchRoot = resolve(root, ".test-tmp");
mkdirSync(scratchRoot, { recursive: true });
process.env.TEMP = scratchRoot;
process.env.TMP = scratchRoot;
process.env.TMPDIR = scratchRoot;
process.env.GOTMPDIR = scratchRoot;
process.env.GOCACHE ??= resolve(scratchRoot, "go-build");

const args = new Set(process.argv.slice(2));
const tagIndex = process.argv.indexOf("--tag");
const releaseTag = tagIndex >= 0 ? process.argv[tagIndex + 1] : undefined;

function readJson(path) {
  return JSON.parse(readFileSync(resolve(root, path), "utf8"));
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

/** The command line that runs pnpm with `commandArgs`. */
function pnpm(commandArgs) {
  if (process.platform === "win32" && process.env.npm_execpath) return [process.execPath, [process.env.npm_execpath, ...commandArgs]];
  return ["pnpm", commandArgs];
}

/**
 * Runs independent checks side by side. Their output is buffered so it does
 * not interleave; each check prints one line, and a failing one its full log.
 */
async function runChecks(checks) {
  const results = await Promise.all(checks.map(({ label, command: [command, commandArgs], env }) => new Promise((done) => {
    const started = Date.now();
    let output = "";
    const options = { cwd: root, env: { ...process.env, ...env } };
    // Outside `pnpm run` on Windows, pnpm is a .cmd shim that needs a shell;
    // the fixed script names are passed as one command line.
    const child = process.platform === "win32" && command === "pnpm"
      ? spawn(`pnpm ${commandArgs.join(" ")}`, { ...options, shell: true })
      : spawn(command, commandArgs, options);
    child.stdout.on("data", (chunk) => { output += chunk; });
    child.stderr.on("data", (chunk) => { output += chunk; });
    child.on("error", (error) => done({ label, ok: false, output: `${error}\n`, seconds: 0 }));
    child.on("close", (code) => done({ label, ok: code === 0, output, seconds: (Date.now() - started) / 1000 }));
  })));
  for (const result of results) {
    console.log(`${result.ok ? "✓" : "✗"} ${result.label}（${result.seconds.toFixed(1)}s）`);
    if (!result.ok) process.stdout.write(result.output);
  }
  if (results.some((result) => !result.ok)) process.exit(1);
}

// package.json is the frontend version, wails.json the one embedded in the
// executable; a release must not disagree with itself.
const packageJson = readJson("package.json");
const wailsJson = readJson("wails.json");
const versions = [packageJson.version, wailsJson.info?.productVersion];
if (versions.some((version) => version !== versions[0])) {
  throw new Error(`版本号不一致：package=${versions[0]} wails=${versions[1]}`);
}
if (releaseTag && releaseTag !== `v${versions[0]}`) {
  throw new Error(`标签 ${releaseTag} 与应用版本 v${versions[0]} 不一致`);
}

checkIcon("build/appicon.png");
if (!existsSync(resolve(root, "build/windows/icon.ico"))) throw new Error("缺少打包图标：build/windows/icon.ico");

console.log(`发布检查通过：Image-Client v${versions[0]}`);
if (args.has("--check-only")) process.exit(0);

// A release run is a single deliberate run, so the frontend tests may use
// half the cores instead of the bounded default in vite.config.ts.
await runChecks([
  { label: "前端测试", command: pnpm(["test"]), env: { VITEST_MAX_WORKERS: String(Math.max(2, Math.floor(availableParallelism() / 2))) } },
  { label: "类型检查", command: pnpm(["typecheck"]) },
  { label: "Go 测试", command: ["go", ["test", "./internal/..."]] },
]);

const localPlatform = process.platform === "win32" && process.arch === "x64"
  ? "windows-x64"
  : process.platform === "darwin" && process.arch === "arm64"
    ? "macos-arm64"
    : undefined;
if (!localPlatform) {
  throw new Error("release:build 仅支持 Windows x64 或 macOS Apple Silicon；其他平台当前不作为发布目标");
}
const wailsPlatform = localPlatform === "windows-x64" ? "windows/amd64" : "darwin/arm64";
// The frontend is built by wails via `frontend:build`; bindings are not used
// (the frontend calls one generic bridge method).
run("wails", ["build", "-clean", "-skipbindings", "-platform", wailsPlatform]);
run(process.execPath, ["scripts/package-release.mjs", "--platform", localPlatform]);
