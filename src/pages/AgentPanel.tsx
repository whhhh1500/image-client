import { useEffect, useState } from "react";
import { Bot, ChevronRight, Loader2, Pencil, Send, Wand2 } from "lucide-react";
import { llmChat, agentRun, type AgentToolDef } from "../lib/ipc";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useGenerationStore } from "../store/useGenerationStore";
import { generateImage } from "../lib/generate";
import { agentLabel, agentSystem, useAgentStore, pipelineAgents } from "../store/useAgentStore";
import AssetPicker from "../components/AssetPicker";
import { LLM_MODELS } from "../lib/models";
import { parseMarkedSections, parseStoryboardShots, stripThinking, type StoryboardShot } from "../lib/aiOutput";
import { projectContext } from "../lib/projectProfile";
import { logEvent } from "../lib/logger";
import AssetDetailModal, { StoryboardGrid } from "../components/AssetDetailModal";
import { getDocumentDisplayVersion, getDocumentMeta, saveDocumentVersion, type DocumentType } from "../lib/documents";
import WorkflowGuide from "../components/WorkflowGuide";
import { snapshotAsset, type HistoryProvenance, type SourceMaterialSnapshot } from "../lib/provenance";

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";

function MarkedOutput({ text }: { text: string }) {
  const secs = parseMarkedSections(text);
  const badge = (x: string) =>
    x.includes("恒定锚")
      ? { t: "恒定锚 · 版本不变", c: "text-cyan-200", bg: "bg-cyan-300/8" }
      : x.includes("版本锚")
        ? { t: "版本锚 · 随版本", c: "text-violet-200", bg: "bg-violet-300/8" }
        : { t: "剧情内容", c: "text-slate-300", bg: "bg-white/5" };
  return (
    <div className="grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(280px,1fr))]">
      {secs.map((s, i) => (
        <div key={i} className={`${!s.marker || s.body.length > 650 ? "col-span-full" : ""} min-w-0`}>
          {s.marker && (
            <div className={`mb-1 inline-flex items-center gap-1 rounded px-2 py-0.5 text-[10px] font-semibold ${badge(s.marker).bg} ${badge(s.marker).c}`}>
              {badge(s.marker).t}{"  "}{s.marker}
            </div>
          )}
          <pre className="whitespace-pre-wrap rounded-xl border border-slate-700 bg-slate-950/60 p-4 font-mono text-xs leading-relaxed text-slate-200">{s.body}</pre>
        </div>
      ))}
    </div>
  );
}

export default function AgentPanel({ imageModel, onEditPrompt }: { imageModel?: string; onEditPrompt: (id: string) => void }) {
  const [agentId, setAgentId] = useState("director");
  const [model, setModel] = useState("gemini-3.7-flash");
  const [input, setInput] = useState("");
  const [output, setOutput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [viewing, setViewing] = useState<LibAsset | null>(null);
  const [currentDocument, setCurrentDocument] = useState<LibAsset | null>(null);
  const [storyboardDocument, setStoryboardDocument] = useState<LibAsset | null>(null);
  const [shots, setShots] = useState<StoryboardShot[]>([]);
  const [pickerFor, setPickerFor] = useState<number | null>(null);
  const [inputSource, setInputSource] = useState<LibAsset | null>(null);

  useEffect(() => {
    void useAgentStore.getState().load();
  }, []);

  const allAssets = useLibraryStore((s) => s.assets);
  const projects = useProjectStore((s) => s.projects);
  const projectId = useProjectStore((s) => s.activeId);
  const textAssets = allAssets.filter((a) => a.asset.kind === "text" && (a.projectId === projectId || !a.projectId));

  const pipeline = pipelineAgents();
  const agent = pipeline.find((a) => a.id === agentId) ?? pipeline[0];

  // 注入：项目信息 + 已生成的锚（恒定锚/版本锚）作为记忆上下文
  const buildContext = () => {
    const proj = projects.find((p) => p.id === projectId);
    // 串联模式下每步跑完立刻入库，这里必须读 store 最新值，
    // 否则同一次串联里前序步骤生成的锚对后续步骤不可见。
    const anchors = useLibraryStore
      .getState()
      .assets.filter(
        (a) =>
          a.asset.kind === "text" &&
          (a.projectId ?? null) === projectId &&
          a.source.includes("锚"),
      )
      .sort((a, b) => b.createdAt - a.createdAt)
      .slice(0, 5)
      .map((a) => `## ${a.source}\n${String((a.params as { text?: string })?.text ?? "")}`)
      .join("\n\n");
    const ctx: string[] = [];
    if (proj) ctx.push(projectContext(proj));
    if (anchors) ctx.push(`【已生成的锚（可复用，勿偏离）】\n${anchors}`);
    return ctx.length ? `【项目与锚上下文】\n${ctx.join("\n\n")}\n\n---\n` : "";
  };

  const extractBlock = (t: string, marker: string): string | null => {
    const open = `【${marker}】`;
    const idx = t.indexOf(open);
    if (idx === -1) return null;
    const next = t.indexOf("【", idx + open.length);
    return (next === -1 ? t.slice(idx) : t.slice(idx, next)).trim();
  };

  const documentTypeFor = (id: string): DocumentType => ({
    director: "director",
    writer: "script",
    storyboard: "storyboard",
    consistency: "consistency",
    qc: "qc",
  } as Record<string, DocumentType>)[id] ?? "document";

  const persist = async (
    id: string,
    label: string,
    text: string,
    m: string,
    withAnchors = true,
    provenance?: Partial<Omit<HistoryProvenance, "schemaVersion" | "recordedAt">>,
  ) => {
    const document = await saveDocumentVersion({
      title: label,
      text,
      model: m,
      projectId: projectId ?? undefined,
      documentType: documentTypeFor(id),
      changeType: "generated",
      agentId: id,
      shots: id === "storyboard" ? parseStoryboardShots(text) : undefined,
      provenance,
    });
    // 恒定锚 与 版本锚 分别存为可复用锚资产
    if (withAnchors) {
      for (const [mk, tag] of [["恒定锚", "恒定锚"], ["版本锚", "版本锚"]] as const) {
        const blk = extractBlock(text, mk);
        if (blk) {
          await saveDocumentVersion({
            title: `${label}·${tag}`,
            text: blk,
            model: m,
            projectId: projectId ?? undefined,
            documentType: "anchor",
            changeType: "generated",
            agentId: id,
            provenance: {
              ...provenance,
              sourceMaterials: [
                ...(provenance?.sourceMaterials ?? []),
                snapshotAsset(document, text, `${label}生成结果`),
              ],
              parentAssetIds: [...(provenance?.parentAssetIds ?? []), document.asset.id],
            },
          });
        }
      }
    }
    return document;
  };

  const run = async (useAgent = agent) => {
    if (!input.trim() || busy) return;
    const startedProjectId = projectId;
    setBusy(true);
    setError(null);
    setOutput("");
    try {
      const rawInput = input;
      const context = buildContext();
      const systemInstruction = agentSystem(useAgent.id);
      const sourceMaterials: SourceMaterialSnapshot[] = inputSource
        ? [snapshotAsset(inputSource, getDocumentMeta(inputSource)?.text, "导入的原始资料")]
        : [];
      const text = stripThinking(await llmChat(systemInstruction, context + rawInput, model));
      if (useProjectStore.getState().activeId === startedProjectId) {
        setOutput(text);
        setShots(parseStoryboardShots(text));
      }
      const document = await persist(useAgent.id, agentLabel(useAgent.id), text, model, useAgent.id !== "qc", {
        originalInput: rawInput,
        generationInput: rawInput,
        systemInstruction,
        contextSnapshot: context,
        sourceMaterials,
        parentAssetIds: inputSource ? [inputSource.asset.id] : [],
      });
      if (useProjectStore.getState().activeId === startedProjectId) {
        setCurrentDocument(document);
        if (useAgent.id === "storyboard") setStoryboardDocument(document);
      }
    } catch (e) {
      if (useProjectStore.getState().activeId === startedProjectId) setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runPipeline = async () => {
    if (!input.trim() || busy) return;
    const startedProjectId = projectId;
    setBusy(true);
    setError(null);
    setOutput("");
    setShots([]);
    const rootInput = input;
    const rootMaterials: SourceMaterialSnapshot[] = inputSource
      ? [snapshotAsset(inputSource, getDocumentMeta(inputSource)?.text, "导入的原始资料")]
      : [];
    const upstreamDocuments: LibAsset[] = [];
    let cur = rootInput;
    let script = "";
    let storyboard = "";
    const parts: string[] = [];
    try {
      for (const a of pipelineAgents()) {
        // 一致性与质检需要上游原文：只传上一步输出会导致
        // 一致性拿不到角色描写、质检看不到剧本和分镜。
        let stepInput = cur;
        if (a.id === "consistency" || a.id === "qc") {
          const upstream = a.id === "consistency"
            ? [script, storyboard]
            : [script, storyboard, cur];
          const joined = upstream.filter(Boolean).join("\n\n---\n\n");
          if (joined) stepInput = joined;
        }
        const context = buildContext();
        const systemInstruction = agentSystem(a.id);
        const text = stripThinking(await llmChat(systemInstruction, context + stepInput, model));
        if (useProjectStore.getState().activeId !== startedProjectId) {
          setError("项目已切换，流水线中止");
          return;
        }
        parts.push(`### ${agentLabel(a.id)}\n${text}`);
        if (a.id === "writer") script = text;
        if (a.id === "storyboard") {
          storyboard = text;
          setShots(parseStoryboardShots(text));
        }
        // 等锚入库再进入下一步，保证 buildContext 能读到最新锚；
        // 质检输出的是问题清单，不作为可复用锚。
        const document = await persist(a.id, agentLabel(a.id), text, model, a.id !== "qc", {
          originalInput: rootInput,
          generationInput: stepInput,
          systemInstruction,
          contextSnapshot: context,
          sourceMaterials: [
            ...rootMaterials,
            ...upstreamDocuments.map((item) => snapshotAsset(item, getDocumentMeta(item)?.text, `上游：${item.source}`)),
          ],
          parentAssetIds: [
            ...(inputSource ? [inputSource.asset.id] : []),
            ...upstreamDocuments.map((item) => item.asset.id),
          ],
        });
        if (useProjectStore.getState().activeId !== startedProjectId) {
          setError("项目已切换，流水线中止");
          return;
        }
        setCurrentDocument(document);
        if (a.id === "storyboard") setStoryboardDocument(document);
        upstreamDocuments.push(document);
        cur = text;
      }
      const combined = parts.join("\n\n");
      setOutput(combined);
      setInput(combined);
      const pipelineDocument = await saveDocumentVersion({
        title: "完整流水线",
        text: combined,
        model,
        projectId: projectId ?? undefined,
        documentType: "pipeline",
        changeType: "generated",
        provenance: {
          originalInput: rootInput,
          generationInput: rootInput,
          sourceMaterials: [
            ...rootMaterials,
            ...upstreamDocuments.map((item) => snapshotAsset(item, getDocumentMeta(item)?.text, `流水线步骤：${item.source}`)),
          ],
          parentAssetIds: [
            ...(inputSource ? [inputSource.asset.id] : []),
            ...upstreamDocuments.map((item) => item.asset.id),
          ],
        },
      });
      if (useProjectStore.getState().activeId !== startedProjectId) {
        setError("项目已切换，流水线中止");
        return;
      }
      setCurrentDocument(pipelineDocument);
    } catch (e) {
      if (useProjectStore.getState().activeId === startedProjectId) setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const genShot = async (shot: StoryboardShot, ref?: string) => {
    const g = useGenerationStore.getState();
    const prompt = shot.prompt || [shot.shotType, shot.composition, shot.light, shot.camera, shot.action, shot.emotion].filter(Boolean).join(", ");
    const sourceDocument = storyboardDocument ?? currentDocument;
    const startedProjectId = projectId;
    g.load({ prompt, referencePath: ref ?? "", model: imageModel ?? "gpt-image-2", size: "1024x1024 (1:1)", quality: "high", background: "auto" });
    setError(null);
    try {
      await generateImage(g, {
        originalInput: prompt,
        generationInput: prompt,
        sourceMaterials: [
          ...(sourceDocument ? [snapshotAsset(sourceDocument, getDocumentMeta(sourceDocument)?.text, "分镜文档来源")] : []),
          { kind: "text", label: `镜头 ${shot.shotNo} 原始资料`, text: JSON.stringify(shot, null, 2) },
        ],
        parentAssetIds: sourceDocument ? [sourceDocument.asset.id] : [],
      });
    } catch (e) {
      if (useProjectStore.getState().activeId === startedProjectId) setError(String(e));
    }
  };

  const runOrchestrate = async () => {
    if (!input.trim() || busy) return;
    const startedProjectId = projectId;
    setBusy(true);
    setError(null);
    setOutput("");
    setShots([]);
    const tools: AgentToolDef[] = pipelineAgents().map((a) => ({ name: a.id, description: agentLabel(a.id), system: agentSystem(a.id) }));
    const sys = "你是短剧导演，自主编排各步骤完成用户需求。可调用工具：生成剧本(编剧)、生成分镜(分镜)、建立角色库(一致性)、质检。按需调用，最后输出一段完整中文结果；若调用分镜，保留其 JSON，且每镜 prompt 必须是中文出图提示词。";
    try {
      const rawInput = input;
      const context = buildContext();
      const sourceMaterials: SourceMaterialSnapshot[] = inputSource
        ? [snapshotAsset(inputSource, getDocumentMeta(inputSource)?.text, "导入的原始资料")]
        : [];
      const text = stripThinking(await agentRun(sys, context + rawInput, model, tools));
      if (useProjectStore.getState().activeId === startedProjectId) {
        setOutput(text);
        setShots(parseStoryboardShots(text));
      }
      const document = await saveDocumentVersion({
        title: "Agent编排",
        text,
        model,
        projectId: projectId ?? undefined,
        documentType: "orchestration",
        changeType: "generated",
        shots: parseStoryboardShots(text),
        provenance: {
          originalInput: rawInput,
          generationInput: rawInput,
          systemInstruction: sys,
          contextSnapshot: context,
          sourceMaterials,
          parentAssetIds: inputSource ? [inputSource.asset.id] : [],
        },
      });
      if (useProjectStore.getState().activeId === startedProjectId) {
        setCurrentDocument(document);
        if (parseStoryboardShots(text).length) setStoryboardDocument(document);
      }
    } catch (e) {
      if (useProjectStore.getState().activeId === startedProjectId) setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const modelOptions = model && !LLM_MODELS.includes(model) ? [model, ...LLM_MODELS] : LLM_MODELS;

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <WorkflowGuide current="输入原文或打开历史文档" next="运行单个 Agent/流水线后，在文档详情中手改或智能优化并保存新版本" detail="分镜确认后可逐镜生成图片" />
      <div className="flex min-h-0 flex-1 gap-6 overflow-y-auto p-6">
      {/* Left: controls */}
      <div className="w-[340px] shrink-0 space-y-4 rounded-2xl border border-slate-800 bg-slate-900/60 p-5">
        <div className="flex items-center gap-2 text-sm font-semibold">
          <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-emerald-500"><Bot size={15} className="text-white" /></span>
          剧本 · 分镜（文本 Agent）
        </div>

        <label className="block">
          <div className="mb-1 text-xs font-medium text-slate-300">LLM 模型</div>
          <select className={inputCls} value={model} onChange={(e) => setModel(e.target.value)}>
            {modelOptions.map((m) => (<option key={m} value={m}>{m}</option>))}
          </select>
        </label>

        <label className="block">
          <div className="mb-1 flex items-center justify-between text-xs font-medium text-slate-300">
            <span>Agent</span>
            <button type="button" onClick={() => onEditPrompt(agentId)} className="flex items-center gap-1 text-[10px] text-indigo-300 hover:text-white">
              <Pencil size={11} /> 修改 prompt
            </button>
          </div>
          <select className={inputCls} value={agentId} onChange={(e) => setAgentId(e.target.value)}>
            {pipelineAgents().map((a) => (<option key={a.id} value={a.id}>{agentLabel(a.id)}</option>))}
          </select>
        </label>

        <label className="block">
          <div className="mb-1 text-xs font-medium text-slate-300">输入</div>
          <textarea className={`${inputCls} h-40 resize-y leading-snug`} value={input} placeholder={agent.placeholder} onChange={(e) => { setInput(e.target.value); setInputSource(null); }} />
        </label>

        {error && <div className="rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-300">{error}</div>}

        <div className="flex gap-2">
          <button onClick={() => run()} disabled={!input.trim() || busy} className="flex flex-1 items-center justify-center gap-1.5 rounded-lg bg-emerald-500 px-3 py-2 text-xs font-medium text-white transition hover:bg-emerald-400 disabled:opacity-50">
            {busy ? <Loader2 size={14} className="animate-spin" /> : <Send size={14} />}
            单个运行
          </button>
          <button onClick={runPipeline} disabled={!input.trim() || busy} className="flex flex-1 items-center justify-center gap-1.5 rounded-lg bg-indigo-500 px-3 py-2 text-xs font-medium text-white transition hover:bg-indigo-400 disabled:opacity-50">
            <Wand2 size={14} />
            串联全部
          </button>
        </div>
        <button onClick={runOrchestrate} disabled={!input.trim() || busy} className="flex w-full items-center justify-center gap-1.5 rounded-lg bg-fuchsia-500 px-3 py-2 text-xs font-medium text-white transition hover:bg-fuchsia-400 disabled:opacity-50">
          {busy ? <Loader2 size={14} className="animate-spin" /> : <Bot size={14} />}
          Agent 编排（自主调用各步）
        </button>
        <p className="text-[10px] leading-snug text-slate-500">串联=固定顺序；编排=导演 Agent 自主决定调用哪些步骤工具。</p>
      </div>

      {/* Middle-right: output */}
      <div className="flex min-w-0 flex-1 flex-col gap-4">
        <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
          <div className="mb-2 flex items-center justify-between text-xs font-semibold uppercase tracking-wide text-slate-400">
            <span>输出</span>
            <div className="flex items-center gap-2 text-[10px] font-normal">
              <span className="text-cyan-200/75">恒定锚</span>
              <span className="text-violet-200/75">版本锚</span>
              <span className="text-slate-500">剧情内容</span>
              {currentDocument && <button onClick={() => setViewing(currentDocument)} className="text-cyan-200/70 hover:text-cyan-100">打开当前版本</button>}
              {output && <button onClick={() => void navigator.clipboard?.writeText(output).catch((error) => logEvent("warn", "clipboard.write_failed", { error: String(error) }))} className="text-slate-500 hover:text-white">复制</button>}
            </div>
          </div>
          {output ? shots.length > 0 ? (
            <button onClick={() => (storyboardDocument ?? currentDocument) && setViewing(storyboardDocument ?? currentDocument)} className="flex min-h-40 w-full items-center justify-center rounded-xl border border-cyan-300/10 bg-cyan-300/[0.025] p-6 text-center">
              <span><span className="block text-sm font-medium text-cyan-100">已解析为 {shots.length} 个结构化镜头</span><span className="mt-2 block text-xs text-slate-600">下方按内容动态分栏展示；点击这里进入完整编辑、智能优化和版本保存。</span></span>
            </button>
          ) : (
            <MarkedOutput text={output} />
          ) : (
            <pre className="min-h-[280px] whitespace-pre-wrap rounded-xl border border-slate-700 bg-slate-950/60 p-4 font-mono text-xs leading-relaxed text-slate-200">
              {busy ? "生成中…" : "选择 Agent、填入输入后点「运行」"}
            </pre>
          )}
        </div>

        {/* Parsed storyboard (shots) */}
        {shots.length > 0 && (
          <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
            <div className="mb-3 flex items-center justify-between gap-3">
              <div>
                <div className="text-xs font-semibold uppercase tracking-wide text-slate-400">分镜 · {shots.length} 镜</div>
                <div className="mt-1 text-[10px] text-slate-600">下一步：检查镜头内容，进入详情可逐字段修改或智能优化，再逐镜生成。</div>
              </div>
              {(storyboardDocument ?? currentDocument) && (
                <button onClick={() => setViewing(storyboardDocument ?? currentDocument)} className="rounded-lg border border-cyan-300/15 px-3 py-1.5 text-[10px] text-cyan-200 hover:bg-cyan-300/5">编辑完整分镜</button>
              )}
            </div>
            <StoryboardGrid
              shots={shots}
              editable={false}
              onChange={() => {}}
              renderActions={(shot, index) => (
                <div className="ml-2 flex shrink-0 gap-1">
                  <button onClick={() => setPickerFor(index)} className="rounded border border-violet-300/15 px-2 py-1 text-[9px] text-violet-200 hover:bg-violet-300/5">参考生成</button>
                  <button onClick={() => void genShot(shot)} className="rounded border border-cyan-300/15 px-2 py-1 text-[9px] text-cyan-200 hover:bg-cyan-300/5">直接生成</button>
                </div>
              )}
            />
            <p className="mt-2 text-[10px] text-slate-500">「带参考生成」选一张资产图当参考（角色/场景一致）；「文生图」直接按描述生成。</p>
          </div>
        )}

        {/* Saved history */}
        <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
          <div className="mb-3 flex items-end justify-between">
            <div>
              <div className="text-xs font-semibold uppercase tracking-wide text-slate-400">文档历史 · {textAssets.length}</div>
              <div className="mt-1 text-[10px] text-slate-600">点击任意历史，查看所有版本、完整内容并继续修改。</div>
            </div>
          </div>
          <div className="max-h-80 space-y-1.5 overflow-y-auto pr-1">
            {textAssets.length === 0 && <div className="text-xs text-slate-500">运行后自动保存为 .md 剧本资产</div>}
            {textAssets
              .sort((a, b) => b.createdAt - a.createdAt)
              .filter((asset, index, list) => {
                const id = getDocumentMeta(asset)?.documentId;
                return list.findIndex((candidate) => getDocumentMeta(candidate)?.documentId === id) === index;
              })
              .map((a) => {
                const meta = getDocumentMeta(a);
                const versionCount = meta
                  ? textAssets.filter((candidate) => getDocumentMeta(candidate)?.documentId === meta.documentId).length
                  : 1;
                return (
              <button key={a.asset.id} onClick={() => setViewing(a)} className="flex w-full items-center justify-between rounded-lg border border-slate-700/60 px-2.5 py-2 text-xs transition hover:border-slate-500">
                <span className="min-w-0 truncate text-slate-100">{a.source}<span className="text-slate-500"> · v{getDocumentDisplayVersion(a, textAssets)} · {versionCount} 版</span></span>
                <span className="flex shrink-0 items-center gap-2 text-slate-600">{new Date(a.createdAt).toLocaleString()} <ChevronRight size={12} /></span>
              </button>
                );
              })}
          </div>
        </div>
      </div>

      <AssetDetailModal
        asset={viewing}
        onClose={() => setViewing(null)}
        onUseText={(text, sourceAsset) => { setInput(text); setInputSource(sourceAsset); setOutput(""); setShots(parseStoryboardShots(text)); }}
        onSaved={(asset) => {
          const meta = getDocumentMeta(asset);
          setCurrentDocument(asset);
          if (meta?.documentType === "storyboard") setStoryboardDocument(asset);
          setOutput(meta?.text ?? output);
          setShots(meta?.shots ?? parseStoryboardShots(meta?.text ?? ""));
        }}
      />

      <AssetPicker
        open={pickerFor !== null}
        onClose={() => setPickerFor(null)}
        kinds={["image"]}
        onPick={(a) => {
          if (pickerFor !== null) genShot(shots[pickerFor], a.asset.path);
          setPickerFor(null);
        }}
      />

      </div>
    </div>
  );
}
