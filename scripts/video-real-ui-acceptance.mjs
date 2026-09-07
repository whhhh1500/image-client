import { writeFile } from "node:fs/promises";

const wsUrl = process.env.CDP_WS;
if (!wsUrl) throw new Error("CDP_WS is required");
const socket = new WebSocket(wsUrl);
let sequence = 0;
const pending = new Map();
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  const resolver = pending.get(message.id);
  if (resolver) {
    pending.delete(message.id);
    resolver(message);
  }
});
const send = (method, params = {}) => new Promise((resolve) => {
  const id = ++sequence;
  pending.set(id, resolve);
  socket.send(JSON.stringify({ id, method, params }));
});
const evaluate = async (expression) => {
  const response = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (response.result?.exceptionDetails) throw new Error(response.result.exceptionDetails.text);
  return response.result?.result?.value;
};
await new Promise((resolve, reject) => {
  socket.addEventListener("open", resolve, { once: true });
  socket.addEventListener("error", reject, { once: true });
});
if (process.env.INSPECT_ONLY === "1") {
  await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent.trim() === '视频'))?.click()`);
  await new Promise((resolve) => setTimeout(resolve, 800));
  await evaluate(`([...document.querySelectorAll('button[title="选择用于拼接"]')].slice(0, 2)).forEach(button => button.click())`);
  await new Promise((resolve) => setTimeout(resolve, 300));
  const result = await evaluate(`(async () => {
    const rows = await window.__TAURI_INTERNALS__.invoke('db_select', {
      query: "SELECT id, kind, path, duration_s, metadata FROM assets WHERE kind = ? ORDER BY created_at",
      bindValues: ['video'],
    });
    const joinButton = [...document.querySelectorAll('button')].find(button => button.textContent.includes('拼接所选镜头'));
    return {
      rows,
      selectedTwo: document.body.innerText.includes('已选 2 镜'),
      joinButtonEnabled: joinButton ? !joinButton.disabled : false,
      videoCount: document.querySelectorAll('video').length,
    };
  })()`);
  console.log(JSON.stringify(result, null, 2));
  socket.close();
  process.exit(0);
}
await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent.trim() === '视频')).click()`);
await new Promise((resolve) => setTimeout(resolve, 1200));
await evaluate(`(() => {
  const setValue = (element, value) => {
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), 'value').set;
    setter.call(element, value);
    element.dispatchEvent(new Event('input', { bubbles: true }));
    element.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const model = [...document.querySelectorAll('select')].find(select => [...select.options].some(option => option.value === 'grok-imagine-video'));
  if (!model) throw new Error('missing video model select');
  setValue(model, 'grok-imagine-video');
  const resolution = [...document.querySelectorAll('select')].find(select => [...select.options].some(option => option.value === '480p'));
  if (resolution) setValue(resolution, '480p');
  const first = [...document.querySelectorAll('textarea')].find(area => area.placeholder.includes('完整视频 Prompt'));
  if (!first) throw new Error('missing first shot textarea');
  setValue(first, '电影感写实画面，清晨海边，一只白色小狗沿着潮湿沙滩向镜头跑来，低机位缓慢后退跟拍，动作连贯，无字幕无文字。');
  const firstDuration = [...document.querySelectorAll('select')].find(select => select.getAttribute('aria-label') === '第 1 镜时长');
  if (!firstDuration) throw new Error('missing first shot duration');
  setValue(firstDuration, '3');
  const add = [...document.querySelectorAll('button')].find(button => button.textContent.includes('新增镜头'));
  if (!add) throw new Error('missing add shot button');
  add.click();
})()`);
await new Promise((resolve) => setTimeout(resolve, 300));
await evaluate(`(() => {
  const areas = [...document.querySelectorAll('textarea')].filter(area => area.placeholder.includes('完整视频 Prompt'));
  if (areas.length !== 2) throw new Error('expected two shot textareas');
  const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(areas[1]), 'value').set;
  setter.call(areas[1], '电影感写实画面，白色小狗停在浅水边抬头看向远方，海浪轻轻掠过脚边，镜头从侧面缓慢环绕，无字幕无文字。');
  areas[1].dispatchEvent(new Event('input', { bubbles: true }));
  areas[1].dispatchEvent(new Event('change', { bubbles: true }));
  const duration = [...document.querySelectorAll('select')].find(select => select.getAttribute('aria-label') === '第 2 镜时长');
  if (!duration) throw new Error('missing second shot duration');
  const durationSetter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(duration), 'value').set;
  durationSetter.call(duration, '3');
  duration.dispatchEvent(new Event('change', { bubbles: true }));
})()`);
await new Promise((resolve) => setTimeout(resolve, 300));
const before = await evaluate("document.body.innerText");
if (!before.includes("2 镜 · 共 6 秒")) throw new Error("UI did not render two three-second test shots");
await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent.includes('生成 2 个视频镜头'))).click()`);
const deadline = Date.now() + 20 * 60 * 1000;
let body = "";
while (Date.now() < deadline) {
  await new Promise((resolve) => setTimeout(resolve, 5000));
  body = await evaluate("document.body.innerText");
  if (body.includes("全部镜头已分别保存") || body.includes("任务失败，请修正后重试")) break;
}
const screenshot = await send("Page.captureScreenshot", { format: "png" });
await writeFile(process.env.UI_SCREENSHOT || ".test-tmp/video-real-ui.png", Buffer.from(screenshot.result.data, "base64"));
console.log(body.includes("全部镜头已分别保存") ? "VIDEO_UI_GENERATION_OK" : body.slice(-3000));
socket.close();
if (!body.includes("全部镜头已分别保存")) process.exitCode = 1;
