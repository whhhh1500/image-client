import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { basename, join, relative, resolve } from "node:path";
import process from "node:process";

const root = resolve(import.meta.dirname, "..");
const args = process.argv.slice(2);

function option(name) {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : undefined;
}

function requiredOption(name) {
  const value = option(name);
  if (!value || value.startsWith("--")) throw new Error(`缺少 ${name}`);
  return value;
}

function appVersion() {
  const packageJson = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  return packageJson.version;
}

function safeOutputDirectory(raw, version) {
  const output = resolve(root, raw ?? join("release-exe", version));
  const pathFromRoot = relative(root, output);
  if (!pathFromRoot || pathFromRoot.startsWith("..") || pathFromRoot.includes(":\\")) {
    throw new Error("发布输出目录必须位于仓库内");
  }
  mkdirSync(output, { recursive: true });
  return output;
}

function releaseName(platform, version) {
  const names = {
    "windows-x64": `Image-Client_${version}_x64_portable.exe`,
    "macos-arm64": `Image-Client_${version}_aarch64.app.tar.gz`,
    "linux-x64": `Image-Client_${version}_amd64.AppImage`,
  };
  const name = names[platform];
  if (!name) throw new Error(`不支持的发布平台：${platform}`);
  return name;
}

function releaseNames(version) {
  return ["windows-x64", "macos-arm64", "linux-x64"].map((platform) => releaseName(platform, version));
}

function targetReleaseDirectory(target) {
  return target
    ? join(root, "src-tauri", "target", target, "release")
    : join(root, "src-tauri", "target", "release");
}

function exactlyOne(directory, predicate, description) {
  if (!existsSync(directory)) throw new Error(`缺少 ${description} 目录：${directory}`);
  const found = readdirSync(directory)
    .map((name) => join(directory, name))
    .filter((path) => predicate(path));
  if (found.length !== 1) throw new Error(`${description} 必须恰好有一个，当前为 ${found.length}`);
  return found[0];
}

function windowsProductVersion(file) {
  const result = spawnSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "$ErrorActionPreference = 'Stop'; (Get-Item -LiteralPath $env:IMAGE_CLIENT_RELEASE_EXE).VersionInfo.ProductVersion",
    ],
    {
      cwd: root,
      encoding: "utf8",
      env: { ...process.env, IMAGE_CLIENT_RELEASE_EXE: file },
      shell: false,
    },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`无法读取 Windows portable 版本：${result.stderr.trim()}`);
  return result.stdout.trim();
}

function packagePlatform(platform, version, outputDirectory, target) {
  const targetDirectory = targetReleaseDirectory(target);
  const destination = join(outputDirectory, releaseName(platform, version));
  if (existsSync(destination) && !statSync(destination).isFile()) {
    throw new Error(`已知发布文件路径不是普通文件：${destination}`);
  }
  if (platform === "windows-x64") {
    const source = join(targetDirectory, "image-client.exe");
    if (!existsSync(source) || !statSync(source).isFile()) {
      throw new Error(`缺少 Windows portable 可执行文件：${source}`);
    }
    const productVersion = windowsProductVersion(source);
    if (productVersion !== version) {
      throw new Error(`Windows portable 版本不匹配：期望 ${version}，实际 ${productVersion || "缺失"}`);
    }
    copyFileSync(source, destination);
  } else if (platform === "linux-x64") {
    const source = exactlyOne(
      join(targetDirectory, "bundle", "appimage"),
      (path) => statSync(path).isFile() && path.endsWith(".AppImage"),
      "Linux AppImage",
    );
    copyFileSync(source, destination);
  } else {
    const source = exactlyOne(
      join(targetDirectory, "bundle", "macos"),
      (path) => statSync(path).isDirectory() && path.endsWith(".app"),
      "macOS app",
    );
    const result = spawnSync(
      "tar",
      ["-czf", destination, "-C", resolve(source, ".."), basename(source)],
      { cwd: root, stdio: "inherit", shell: false },
    );
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error("打包 macOS app.tar.gz 失败");
  }
  if (!existsSync(destination) || !statSync(destination).isFile() || statSync(destination).size === 0) {
    throw new Error(`发布文件未生成：${destination}`);
  }
  verifyDirectory(outputDirectory, [releaseName(platform, version)]);
  console.log(`发布文件已准备：${destination}`);
}

function filesRecursively(directory) {
  return readdirSync(directory, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => entry.name);
}

function verifyDirectory(directory, expectedNames) {
  if (!existsSync(directory)) throw new Error(`发布输出目录不存在：${directory}`);
  const actualNames = filesRecursively(directory).sort();
  const expected = [...expectedNames].sort();
  if (actualNames.length !== expected.length || actualNames.some((name, index) => name !== expected[index])) {
    throw new Error(`发布文件不符合 allowlist：期望 ${expected.join(", ")}；实际 ${actualNames.join(", ") || "无"}`);
  }
}

function safeRepositoryFile(raw, description) {
  const file = resolve(root, requiredOption(raw));
  const pathFromRoot = relative(root, file);
  if (!pathFromRoot || pathFromRoot.startsWith("..") || pathFromRoot.includes(":\\")) {
    throw new Error(`${description}必须位于仓库内`);
  }
  return file;
}

function changelogSection(version) {
  const lines = readFileSync(join(root, "CHANGELOG.md"), "utf8").split(/\r?\n/);
  const start = lines.findIndex((line) => line.startsWith(`## ${version} - `));
  if (start < 0) throw new Error(`CHANGELOG.md 中缺少 ${version} 的发布说明`);
  const nextHeading = lines.slice(start + 1).findIndex((line) => /^## /.test(line));
  const end = nextHeading < 0 ? lines.length : start + 1 + nextHeading;
  return lines.slice(start, end).join("\n").trim();
}

function writeReleaseNotes(version, destination) {
  const notes = [
    `# Image-Client v${version}`,
    "",
    "## 下载与使用",
    "",
    `- Windows x64：\`Image-Client_${version}_x64_portable.exe\`。下载后直接运行；系统需要 Microsoft Edge WebView2 Runtime。`,
    `- macOS Apple Silicon：\`Image-Client_${version}_aarch64.app.tar.gz\`。解压后运行应用。`,
    `- Linux x64：\`Image-Client_${version}_amd64.AppImage\`。赋予执行权限后运行。`,
    "",
    "此版本未进行商业代码签名，系统首次打开时可能显示安全提示。",
    "",
    "## 本次更新",
    "",
    changelogSection(version),
    "",
  ].join("\n");
  writeFileSync(destination, notes, "utf8");
  console.log(`发布说明已生成：${destination}`);
}

const version = option("--version") ?? appVersion();
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error(`版本号无效：${version}`);
}
if (args.includes("--release-notes")) {
  writeReleaseNotes(version, safeRepositoryFile("--notes-file", "发布说明文件"));
} else {
  const outputDirectory = safeOutputDirectory(option("--output-dir"), version);
  if (args.includes("--verify-all")) {
  verifyDirectory(outputDirectory, releaseNames(version));
  console.log(`发布 allowlist 验证通过：${releaseNames(version).join(", ")}`);
  } else {
    packagePlatform(requiredOption("--platform"), version, outputDirectory, option("--target"));
  }
}
