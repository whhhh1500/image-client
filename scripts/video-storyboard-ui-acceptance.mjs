import { writeFile } from "node:fs/promises";

const wsUrl = process.env.CDP_WS;
if (!wsUrl) throw new Error("CDP_WS is required");
const screenshotPath = process.env.UI_SCREENSHOT || ".test-tmp/video-storyboard-handoff.png";
const socket = new WebSocket(wsUrl);
let sequence = 0;
const pending = new Map();
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  const resolver = pending.get(message.id);
  if (resolver) { pending.delete(message.id); resolver(message); }
});
const send = (method, params = {}) => new Promise((resolve) => {
  const id = ++sequence; pending.set(id, resolve); socket.send(JSON.stringify({ id, method, params }));
});
const evaluate = async (expression) => {
  const response = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (response.result?.exceptionDetails) throw new Error(response.result.exceptionDetails.text);
  return response.result?.result?.value;
};
const waitFor = async (test, label, timeout = 30_000) => {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await evaluate(test)) return;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Timed out waiting for ${label}`);
};
await new Promise((resolve, reject) => {
  socket.addEventListener("open", resolve, { once: true });
  socket.addEventListener("error", reject, { once: true });
});
await waitFor("Boolean(window.__TAURI_INTERNALS__) && document.body.innerText.includes('数据库 就绪')", "application startup");

const storyboard = `# 视频分镜

## 第1镜

### 场次
外景 海边 清晨

### 景别
中景

### 构图
主体居中，海平线位于上三分线

### 光线
清晨侧逆光，柔和阴影

### 运镜
低机位缓慢后退跟拍

### 画面动作
白色小狗沿湿润沙滩跑近

### 情绪
轻快

### 时长
3秒

### 起始状态
小狗位于远处沙滩中央

### 动作过程
小狗持续向镜头跑近，脚边溅起少量水花

### 结束状态
小狗到达镜头前方

### 承接镜头
无

### 画风锚
style:film:v1

### 场景锚
scene:beach:v1

### 角色锚
character:dog:v1

### 道具锚
无

### 参考方式
text

### 参考资产
无

### 来源对白
无

### 视频 Prompt
电影写实风格，清晨海边，保持白色小狗外观、海滩环境和清晨光线一致，小狗沿湿润沙滩向镜头跑近，低机位缓慢后退跟拍，动作自然连贯，无字幕无文字。

## 第2镜

### 场次
外景 海边 清晨

### 景别
近景

### 构图
主体偏左，右侧保留海面空间

### 光线
清晨侧逆光，柔和阴影

### 运镜
侧面缓慢环绕

### 画面动作
白色小狗停在浅水边抬头看海

### 情绪
好奇

### 时长
5秒

### 起始状态
小狗已到达镜头前方

### 动作过程
小狗减速停下，缓慢转头看向海面

### 结束状态
小狗面向海面静止

### 承接镜头
第1镜

### 画风锚
style:film:v1

### 场景锚
scene:beach:v1

### 角色锚
character:dog:v1

### 道具锚
无

### 参考方式
text

### 参考资产
无

### 来源对白
无

### 视频 Prompt
承接上一镜，保持白色小狗外观、海滩环境和清晨光线一致，小狗在浅水边停下并抬头看向海面，镜头从侧面缓慢环绕，无字幕无文字。`;

await evaluate(`(async () => {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const projectRows = await invoke('db_select', { query: 'SELECT value FROM settings WHERE key = ?', bindValues: ['projects'] });
  const projectId = JSON.parse(projectRows[0].value)[0].id;
  const text = ${JSON.stringify(storyboard)};
  const asset = await invoke('save_text', { label: 'UI验收视频分镜', text, model: 'fixture' });
  const params = { text, title: 'UI验收视频分镜', documentType: 'storyboard', documentId: 'document-ui-storyboard', version: 1, changeType: 'generated', agentId: 'storyboard', updatedAt: Date.now() };
  await invoke('db_execute', {
    query: 'INSERT OR REPLACE INTO assets (id, kind, path, width, height, duration_s, format, created_at, metadata) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)',
    bindValues: [asset.id, asset.kind, asset.path, null, null, null, asset.format ?? 'md', Date.now(), JSON.stringify({ source: 'UI验收视频分镜', model: 'fixture', projectId, params })],
  });
})()`);
await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent.includes('刷新历史'))).click()`);
await waitFor("document.body.innerText.includes('历史已刷新')", "history refresh");
await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent.includes('短剧 Agent'))).click()`);
await waitFor("document.body.innerText.includes('UI验收视频分镜')", "storyboard history item");
await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent.includes('UI验收视频分镜'))).click()`);
await waitFor("document.body.innerText.includes('视频 Prompt') && ![...document.querySelectorAll('button')].find(button => button.textContent.trim() === '用于下一步')?.disabled", "loaded storyboard detail modal");
await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent.trim() === '用于下一步')).click()`);
await waitFor("document.body.innerText.includes('视频分镜 · 2 镜')", "parsed storyboard shots");
await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent.includes('发送到视频工作区'))).click()`);
await waitFor("document.body.innerText.includes('2 镜 · 共 8 秒') && document.body.innerText.includes('生成 2 个视频镜头')", "video workspace handoff");
const result = await evaluate(`({
  prompts: [...document.querySelectorAll('textarea')].filter(area => area.placeholder.includes('完整视频 Prompt')).map(area => area.value),
  durations: [...document.querySelectorAll('select')].filter(select => select.getAttribute('aria-label')?.includes('镜时长')).map(select => Number(select.value)),
})`);
if (result.prompts.length !== 2 || result.durations.join(',') !== '3,5') throw new Error(`shot handoff mismatch: ${JSON.stringify(result)}`);
const screenshot = await send("Page.captureScreenshot", { format: "png" });
await writeFile(screenshotPath, Buffer.from(screenshot.result.data, "base64"));
console.log(JSON.stringify({ status: "VIDEO_STORYBOARD_HANDOFF_OK", shotCount: 2, durations: result.durations, screenshotPath }));
socket.close();
