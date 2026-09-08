import { create } from "zustand";
import { dbExecute, dbSelect } from "../lib/db";
import { logEvent } from "../lib/logger";

export interface Project {
  id: string;
  name: string;
  description: string;
  storyStyle: string;
  artStyle: string;
  aspectRatio: string;
  imageModel: string;
  imageQuality: string;
  videoModel: string;
  videoResolution: string;
}

export type ProjectPatch = Partial<Omit<Project, "id">>;

const PROJECT_DEFAULTS: Omit<Project, "id" | "name"> = {
  description: "",
  storyStyle: "通用短剧",
  artStyle: "电影写实",
  aspectRatio: "16:9",
  imageModel: "gpt-image-2",
  imageQuality: "high",
  videoModel: "grok-imagine-video",
  videoResolution: "720p",
};

function makeProject(name: string): Project {
  return {
    id: `p_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
    name,
    ...PROJECT_DEFAULTS,
  };
}

function normalizeProject(value: Partial<Project>, index: number): Project {
  return {
    id: typeof value.id === "string" && value.id ? value.id : `p_legacy_${index}`,
    name: typeof value.name === "string" && value.name.trim() ? value.name.trim() : `项目 ${index + 1}`,
    ...PROJECT_DEFAULTS,
    ...value,
  } as Project;
}

const KEY = "projects";
const ACTIVE_KEY = "active_project";

async function setSetting(key: string, value: string) {
  await dbExecute(
    "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    [key, value],
  );
}
async function getSetting(key: string): Promise<string | null> {
  const rows = await dbSelect<{ value: string }[]>("SELECT value FROM settings WHERE key = ?", [key]);
  return rows.length ? rows[0].value : null;
}

interface ProjectState {
  projects: Project[];
  activeId: string | null;
  load: () => Promise<void>;
  create: (name?: string) => Promise<string>;
  remove: (id: string) => Promise<void>;
  rename: (id: string, name: string) => Promise<void>;
  update: (id: string, patch: ProjectPatch) => Promise<void>;
  switch: (id: string) => Promise<boolean>;
}

export const useProjectStore = create<ProjectState>((set, get) => {
  // 切换项目请求序号：快速连续切换时只有最后一次允许写回 state。
  let switchSeq = 0;
  // Every settings write is serialized: create/rename/update used to read the
  // same stale snapshot and then overwrite each other's result. Each task reads
  // `get()` inside the chain, so it always builds on the previous write.
  let writeChain: Promise<unknown> = Promise.resolve();
  const serialize = <T,>(task: () => Promise<T>): Promise<T> => {
    const run = writeChain.then(task, task);
    writeChain = run.then(() => undefined, () => undefined);
    return run;
  };
  const persist = async (projects: Project[], activeId: string) => {
    await setSetting(KEY, JSON.stringify(projects));
    await setSetting(ACTIVE_KEY, activeId);
    set({ projects, activeId });
    logEvent("info", "projects.persisted", { projectCount: projects.length, activeId });
  };

  return {
    projects: [],
    activeId: null,

    load: async () => {
      await serialize(async () => {
        let projects: Project[] = [];
        try {
          const raw = await getSetting(KEY);
          if (raw) {
            const parsed = JSON.parse(raw);
            if (Array.isArray(parsed)) projects = parsed.map((p, index) => normalizeProject(p, index));
          }
        } catch (error) {
          logEvent("warn", "projects.parse_failed", { error: String(error) });
        }
        if (!projects.length) {
          const p = makeProject("默认项目");
          projects = [p];
        }
        let activeId = await getSetting(ACTIVE_KEY);
        if (!activeId || !projects.find((p) => p.id === activeId)) activeId = projects[0].id;
        await persist(projects, activeId);
      });
    },

    create: async (name) => serialize(async () => {
      const p = makeProject(name?.trim() || `项目 ${get().projects.length + 1}`);
      const projects = [...get().projects, p];
      await persist(projects, p.id);
      return p.id;
    }),

    remove: async (id) => serialize(async () => {
      const projects = get().projects.filter((p) => p.id !== id);
      const activeId = get().activeId === id ? (projects[0]?.id ?? null) : get().activeId;
      if (!projects.length) {
        const p = makeProject("默认项目");
        projects.push(p);
      }
      await persist(projects, activeId ?? projects[0].id);
    }),

    rename: async (id, name) => serialize(async () => {
      const projects = get().projects.map((p) => (p.id === id ? { ...p, name: name.trim() || p.name } : p));
      await persist(projects, get().activeId ?? id);
    }),

    update: async (id, patch) => serialize(async () => {
      const projects = get().projects.map((p) => (p.id === id ? normalizeProject({ ...p, ...patch, id }, 0) : p));
      await persist(projects, get().activeId ?? id);
    }),

    switch: async (id) => {
      if (!get().projects.some((project) => project.id === id)) return false;
      const seq = ++switchSeq;
      return serialize(async () => {
        const projects = get().projects;
        await setSetting(KEY, JSON.stringify(projects));
        await setSetting(ACTIVE_KEY, id);
        // 只有最新一次 switch 允许写回 state，避免快速连续切换时旧结果覆盖新选择。
        if (seq !== switchSeq) {
          logEvent("info", "projects.switch_superseded", { activeId: id });
          return false;
        }
        set({ projects, activeId: id });
        logEvent("info", "projects.persisted", { projectCount: projects.length, activeId: id });
        return true;
      });
    },
  };
});
