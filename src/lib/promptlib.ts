import catalogJson from "../data/promptlib/catalog.json";

export type PromptlibKind = "case" | "template";

export interface PromptlibVariant {
  title: string;
  lang: string;
  prompt: string;
}

export interface PromptlibEntry {
  id: string;
  kind: PromptlibKind;
  title: string;
  titleEn?: string;
  description?: string;
  prompt: string;
  jsonPrompt?: string;
  promptTitle?: string;
  variants?: PromptlibVariant[];
  category: string;
  styles: string[];
  scenes: string[];
  tags: string[];
  useWhen?: string;
  guidance?: string[];
  pitfalls?: string[];
  exampleCases?: number[];
  image: string;
  sourceLabel?: string;
  sourceUrl?: string;
  featured?: boolean;
  caseId?: number;
  templateId?: string;
  anchor?: string;
}

export interface PromptlibCatalog {
  version: number;
  generatedAt: string;
  source: { name: string; url: string; license: string };
  imageBaseUrl: string;
  categories: string[];
  styles: string[];
  scenes: string[];
  caseCount: number;
  templateCount: number;
  cases: PromptlibEntry[];
  templates: PromptlibEntry[];
}

export interface PromptlibOverride {
  prompt: string;
  jsonPrompt?: string;
  updatedAt: number;
}

export const CATEGORY_ZH: Record<string, string> = {
  "Architecture & Spaces": "建筑与空间",
  "Brand & Logos": "品牌与标志",
  "Characters & People": "人物与角色",
  "Charts & Infographics": "图表与信息图",
  "Documents & Publishing": "文档与出版",
  "History & Classical Themes": "历史与古风",
  "Illustration & Art": "插画与艺术",
  "Other Use Cases": "其他",
  "Photography & Realism": "摄影与写实",
  "Posters & Typography": "海报与排版",
  "Products & E-commerce": "商品与电商",
  "Scenes & Storytelling": "场景与叙事",
  "UI & Interfaces": "UI 与界面",
};

export const catalog = catalogJson as PromptlibCatalog;

export function categoryLabel(category: string): string {
  return CATEGORY_ZH[category] ?? category;
}

export function findEntry(id: string): PromptlibEntry | undefined {
  return catalog.templates.find((item) => item.id === id) ?? catalog.cases.find((item) => item.id === id);
}

export function caseImageUrl(image: string): string {
  const file = image.split("/").filter(Boolean).pop() ?? "";
  return `${catalog.imageBaseUrl}/images/${file}`;
}

export function effectivePrompt(
  entry: PromptlibEntry,
  overrides: Record<string, PromptlibOverride>,
  field: "prompt" | "jsonPrompt" = "prompt",
): string {
  const override = overrides[entry.id];
  if (field === "jsonPrompt") return override?.jsonPrompt ?? entry.jsonPrompt ?? "";
  return override?.prompt ?? entry.prompt;
}

export function isModified(id: string, overrides: Record<string, PromptlibOverride>): boolean {
  return Boolean(overrides[id]);
}

export function extractPlaceholders(prompt: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  const matches = prompt.matchAll(/\[([^\[\]]+)\]/g);
  for (const match of matches) {
    const token = match[1].trim();
    if (!token || seen.has(token)) continue;
    seen.add(token);
    out.push(token);
  }
  return out;
}

export function applyPlaceholders(prompt: string, values: Record<string, string>): string {
  return prompt.replace(/\[([^\[\]]+)\]/g, (all, raw: string) => {
    const value = values[raw.trim()]?.trim();
    return value || all;
  });
}

export interface PromptlibQuery {
  kind?: PromptlibKind | "all";
  category?: string;
  style?: string;
  scene?: string;
  modifiedOnly?: boolean;
  query?: string;
}

const SEARCH_SYNONYMS: Record<string, string[]> = {
  "动漫": ["动漫", "二次元", "anime", "manga", "chibi"],
  "二次元": ["二次元", "动漫", "anime", "manga"],
  "anime": ["anime", "动漫", "二次元", "manga"],
  "漫画": ["漫画", "manga", "comic", "动漫"],
  "插画": ["插画", "illustration", "illustrative"],
  "海报": ["海报", "poster", "typography"],
  "人像": ["人像", "portrait", "人物"],
  "写实": ["写实", "realistic", "photoreal"],
  "摄影": ["摄影", "photography", "photo"],
  "ui": ["ui", "界面", "screenshot", "dashboard"],
  "界面": ["界面", "ui", "screenshot"],
  "logo": ["logo", "标志", "品牌"],
  "标志": ["标志", "logo", "品牌"],
  "信息图": ["信息图", "infographic", "图表"],
  "图表": ["图表", "chart", "infographic", "信息图"],
};

function entryHaystack(entry: PromptlibEntry, overrides: Record<string, PromptlibOverride>): string {
  return [
    entry.title,
    entry.titleEn,
    entry.description,
    entry.useWhen,
    entry.category,
    categoryLabel(entry.category),
    effectivePrompt(entry, overrides),
    effectivePrompt(entry, overrides, "jsonPrompt"),
    ...(entry.guidance ?? []),
    ...(entry.pitfalls ?? []),
    ...(entry.tags ?? []),
    ...(entry.styles ?? []),
    ...(entry.scenes ?? []),
    entry.sourceLabel,
    ...(entry.variants ?? []).flatMap((variant) => [variant.title, variant.prompt]),
  ]
    .filter(Boolean)
    .join("\n")
    .toLowerCase();
}

export function searchEntries(
  entries: PromptlibEntry[],
  overrides: Record<string, PromptlibOverride>,
  filter: PromptlibQuery,
): PromptlibEntry[] {
  const raw = (filter.query ?? "").trim().toLowerCase().split(/\s+/).filter(Boolean);
  return entries.filter((entry) => {
    if (filter.kind && filter.kind !== "all" && entry.kind !== filter.kind) return false;
    if (filter.category && entry.category !== filter.category) return false;
    if (filter.style && !(entry.styles ?? []).includes(filter.style)) return false;
    if (filter.scene && !(entry.scenes ?? []).includes(filter.scene)) return false;
    if (filter.modifiedOnly && !overrides[entry.id]) return false;
    if (!raw.length) return true;
    const haystack = entryHaystack(entry, overrides);
    return raw.every((token) => {
      const aliases = SEARCH_SYNONYMS[token] ?? [token];
      return aliases.some((alias) => haystack.includes(alias.toLowerCase()));
    });
  });
}
