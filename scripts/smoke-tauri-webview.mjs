import { readFile } from "node:fs/promises";

const port = Number(process.argv[2] || 9339);
const deadline = Date.now() + 30_000;
const novelWorkbenchSource = await readFile(new URL("../src/components/novel/NovelWorkbench.tsx", import.meta.url), "utf8");
const novelStaticContracts = {
  hasChapterIngestEntry: novelWorkbenchSource.includes("录入正文"),
  hasSaveAndProduction: novelWorkbenchSource.includes("保存并全自动生产"),
  hasOneClickArtifactProduction: novelWorkbenchSource.includes("生成本章全部文字产物"),
  hasUnifiedProductionStatus: novelWorkbenchSource.includes("NovelProductionStatus"),
};

async function waitForTarget() {
  let lastError;
  while (Date.now() < deadline) {
    try {
      const targets = await fetch(`http://127.0.0.1:${port}/json/list`).then((response) => {
        if (!response.ok) throw new Error(`DevTools HTTP ${response.status}`);
        return response.json();
      });
      const target = targets.find((item) => item.type === "page" && item.webSocketDebuggerUrl);
      if (target) return target;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw lastError ?? new Error("WebView DevTools target was not created within 30 seconds");
}

function inspectTarget(target) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    const messages = [];
    let nextId = 1;
    const pending = new Map();
    const timer = setTimeout(() => reject(new Error("DevTools inspection timed out")), 15_000);

    const send = (method, params = {}) => new Promise((resolveCommand, rejectCommand) => {
      const id = nextId++;
      pending.set(id, { resolve: resolveCommand, reject: rejectCommand });
      socket.send(JSON.stringify({ id, method, params }));
    });

    socket.addEventListener("message", (event) => {
      const message = JSON.parse(String(event.data));
      if (message.id && pending.has(message.id)) {
        const entry = pending.get(message.id);
        pending.delete(message.id);
        if (message.error) entry.reject(new Error(message.error.message));
        else entry.resolve(message.result);
        return;
      }
      if (["Runtime.exceptionThrown", "Runtime.consoleAPICalled", "Log.entryAdded"].includes(message.method)) {
        messages.push(message);
      }
    });

    socket.addEventListener("error", () => reject(new Error("DevTools WebSocket failed")));
    socket.addEventListener("open", async () => {
      try {
        await send("Runtime.enable");
        await send("Log.enable");
        await new Promise((resolveWait) => setTimeout(resolveWait, 1_000));
        const inspectDom = () => send("Runtime.evaluate", {
          expression: `(() => ({
            url: location.href,
            title: document.title,
            readyState: document.readyState,
            bodyChildCount: document.body?.children.length ?? -1,
            bodyTextLength: document.body?.innerText?.trim().length ?? 0,
            rootChildCount: document.querySelector('#root')?.children.length ?? -1,
            rootHtmlLength: document.querySelector('#root')?.innerHTML.length ?? 0,
            visibleText: (document.body?.innerText || '').trim().slice(0, 240)
          }))()`,
          returnByValue: true,
        });
        const evaluation = await inspectDom();
        let novelWorkbench = null;
        let comicLayoutWorkbench = null;
        let comicEditorWorkbench = null;
        if (process.argv[3] === "novel" || process.argv[3] === "comic-layout" || process.argv[3] === "comic-editor") {
          await send("Runtime.evaluate", {
            expression: `([...document.querySelectorAll('button')].find((node) => node.textContent?.trim() === '小说漫画')?.click(), true)`,
          });
          await new Promise((resolveWait) => setTimeout(resolveWait, 800));
        }
        if (process.argv[3] === "novel") {
          await send("Runtime.evaluate", {
            expression: `([...document.querySelectorAll('button')].find((node) => node.textContent?.trim() === '管理书架')?.click(), true)`,
          });
          await new Promise((resolveWait) => setTimeout(resolveWait, 800));
          const drawerContract = (await send("Runtime.evaluate", {
            expression: `(() => {
              const text = document.body?.innerText || '';
              return {
                drawerVisible: Boolean(document.querySelector('[role="dialog"][aria-label="小说书架管理"]')),
                hasLibraryHeading: text.includes('小说书架与增量知识库'),
                hasChapterIngestEntry: text.includes('录入正文')
              };
            })()`,
            returnByValue: true,
          })).result.value;
          const ingestButton = (await send("Runtime.evaluate", {
            expression: `(() => {
              const button = [...document.querySelectorAll('button')].find((node) => node.textContent?.trim() === '录入正文');
              return button ? { found: true, disabled: button.disabled } : { found: false, disabled: true };
            })()`,
            returnByValue: true,
          })).result.value;
          let chapterIngest;
          if (ingestButton.disabled) {
            chapterIngest = {
              skippedReason: ingestButton.found
                ? "录入正文按钮已禁用：隔离数据没有可写入的小说 snapshot，或所选小说处于只读状态。"
                : "抽屉内未找到录入正文按钮。",
            };
          } else {
            await send("Runtime.evaluate", {
              expression: `([...document.querySelectorAll('button')].find((node) => node.textContent?.trim() === '录入正文')?.click(), true)`,
            });
            await new Promise((resolveWait) => setTimeout(resolveWait, 650));
            chapterIngest = (await send("Runtime.evaluate", {
              expression: `(() => {
                const drawer = document.querySelector('[role="dialog"][aria-label="小说书架管理"]');
                const section = document.getElementById('novel-chapter-ingest');
                const rect = section?.getBoundingClientRect();
                const drawerRect = drawer?.getBoundingClientRect();
                const intersectsDrawer = Boolean(rect && drawerRect && rect.width > 0 && rect.height > 0
                  && rect.right > drawerRect.left && rect.left < drawerRect.right
                  && rect.bottom > drawerRect.top && rect.top < drawerRect.bottom);
                const intersectsViewport = Boolean(rect && rect.width > 0 && rect.height > 0
                  && rect.right > 0 && rect.left < window.innerWidth
                  && rect.bottom > 0 && rect.top < window.innerHeight);
                return {
                  sectionTagName: section?.tagName ?? null,
                  hasTextarea: Boolean(section?.querySelector('textarea')),
                  intersectsDrawer,
                  intersectsViewport,
                };
              })()`,
              returnByValue: true,
            })).result.value;
          }
          novelWorkbench = { ...drawerContract, chapterIngest };
        }
        if (process.argv[3] === "comic-layout") {
          await send("Runtime.evaluate", {
            expression: `([...document.querySelectorAll('button')].find((node) => node.textContent?.includes('排版与导出'))?.click(), true)`,
          });
          await new Promise((resolveWait) => setTimeout(resolveWait, 800));
          comicLayoutWorkbench = (await inspectDom()).result.value;
        }
        if (process.argv[3] === "comic-editor") {
          await send("Runtime.evaluate", {
            expression: `([...document.querySelectorAll('button')].find((node) => node.textContent?.includes('生成与质检'))?.click(), true)`,
          });
          await new Promise((resolveWait) => setTimeout(resolveWait, 800));
          comicEditorWorkbench = (await send("Runtime.evaluate", {
            expression: `(() => ({
              hasLayoutEditorEntry: (document.body?.innerText || '').includes('可视化拖拽不规则分格'),
              hasDirectRenderWarning: (document.body?.innerText || '').includes('重新生成会创建新的 attempt/资产')
            }))()`,
            returnByValue: true,
          })).result.value;
        }
        clearTimeout(timer);
        socket.close();
        resolve({ target: { title: target.title, url: target.url }, dom: evaluation.result.value, novelWorkbench, novelStaticContracts, comicLayoutWorkbench, comicEditorWorkbench, events: messages });
      } catch (error) {
        clearTimeout(timer);
        socket.close();
        reject(error);
      }
    });
  });
}

const target = await waitForTarget();
const result = await inspectTarget(target);
const fatalEvents = result.events.filter((event) => event.method === "Runtime.exceptionThrown");
const consoleEventCount = result.events.filter((event) => event.method === "Runtime.consoleAPICalled").length;
const logEventCount = result.events.filter((event) => event.method === "Log.entryAdded").length;
console.log(JSON.stringify({
  target: result.target,
  dom: result.dom,
  novelWorkbench: result.novelWorkbench,
  novelStaticContracts: result.novelStaticContracts,
  comicLayoutWorkbench: result.comicLayoutWorkbench,
  comicEditorWorkbench: result.comicEditorWorkbench,
  exceptionCount: fatalEvents.length,
  consoleEventCount,
  logEventCount,
}, null, 2));
const chapterIngestFailed = !result.novelWorkbench?.chapterIngest?.skippedReason
  && (result.novelWorkbench?.chapterIngest?.sectionTagName !== "SECTION"
    || !result.novelWorkbench?.chapterIngest?.hasTextarea
    || !result.novelWorkbench?.chapterIngest?.intersectsDrawer
    || !result.novelWorkbench?.chapterIngest?.intersectsViewport);
const novelFailed = process.argv[3] === "novel" && (!result.novelWorkbench?.drawerVisible || !result.novelWorkbench?.hasLibraryHeading || !result.novelWorkbench?.hasChapterIngestEntry || !result.novelStaticContracts?.hasChapterIngestEntry || !result.novelStaticContracts?.hasSaveAndProduction || !result.novelStaticContracts?.hasOneClickArtifactProduction || !result.novelStaticContracts?.hasUnifiedProductionStatus || chapterIngestFailed);
const comicLayoutFailed = process.argv[3] === "comic-layout" && (!result.comicLayoutWorkbench || !result.comicLayoutWorkbench.visibleText.includes("排版、质检与导出"));
const comicEditorFailed = process.argv[3] === "comic-editor" && (!result.comicEditorWorkbench?.hasLayoutEditorEntry || !result.comicEditorWorkbench?.hasDirectRenderWarning);
if (result.dom.readyState !== "complete" || result.dom.rootChildCount < 1 || result.dom.rootHtmlLength < 100 || fatalEvents.length > 0 || novelFailed || comicLayoutFailed || comicEditorFailed) {
  process.exitCode = 1;
}
