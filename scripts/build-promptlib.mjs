import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoot = path.join(root, "docs", "awesome-gpt-image-2");
const outDir = path.join(root, "src", "data", "promptlib");
const IMAGE_BASE = "https://pub-7ecb2a3a62b94375a9abd336abf0bcc6.r2.dev/img-case-assets";

const TEMPLATE_MATCH = {
  "ui-screenshot-system": { titles: ["常规模板"], extras: ["截图生成模板", "直播界面模板"] },
  "infographic-engine": { titles: ["常规模板"] },
  "scientific-scale-diagram": { titles: ["尺度缩放科学信息图模板"] },
  "poster-layout-system": { titles: ["常规模板"] },
  "sports-campaign-poster": { titles: ["运动商业 Campaign 模板"] },
  "conceptual-typography-poster": {
    titles: ["中文版：概念字体海报模板", "概念字体海报模板"],
    extras: ["多风格签名选择海报模板", "单款签名提取模板", "签名练习拆解图模板"],
  },
  "ink-double-exposure-poster": { titles: ["水墨双重曝光人物海报模板"] },
  "nature-science-poster": { titles: ["自然科普海报模板"] },
  "product-commerce-visual": { titles: ["常规模板"] },
  "personalized-beauty-report": { titles: ["个人化美妆推荐报告模板"] },
  "brand-identity-package": {
    titles: ["完整品牌身份包模板", "常规模板"],
    extras: ["品牌包络产品广告模板", "品牌人格漫画信息图模板"],
  },
  "brand-touchpoint-board": { titles: ["品牌触点系统视觉板模板"] },
  "architecture-space": { titles: ["常规模板"] },
  "realistic-photography": { titles: ["常规模板"] },
  "street-accident-moment": { titles: ["街头意外瞬间写实摄影模板"] },
  "illustration-art-style": { titles: ["常规模板"] },
  "character-design-sheet": { titles: ["常规模板"], extras: ["动作分解参考表模板"] },
  "3d-collectible-toy": { titles: ["参考图转 3D 收藏玩具模板"] },
  "scene-storytelling": { titles: ["常规模板"] },
  "history-classical-themes": { titles: ["常规模板"] },
  "document-publishing": { titles: ["常规模板"], extras: ["企业画册系统模板"] },
  "concept-product-breakdown": { titles: ["概念产品研发拆解板模板", "常规模板"] },
};

function localized(value, fallback = "") {
  if (!value) return fallback;
  if (typeof value === "string") return value;
  return value.zh || value.en || fallback;
}

function localizedList(value) {
  if (!value) return [];
  if (Array.isArray(value)) return value.map(String);
  if (typeof value === "object") return (value.zh || value.en || []).map(String);
  return [];
}

function repairBrokenFences(markdown) {
  return markdown.replace(
    /不要生成无关书法字帖。\s*\n\s*\*\*中文版：概念字体海报模板\*\*/,
    "不要生成无关书法字帖。\n```\n\n**中文版：概念字体海报模板**",
  );
}

function parseTemplateSections(markdown) {
  const parts = markdown.split(/<a name="(tpl-[^"]+)"><\/a>/);
  const sections = new Map();
  for (let i = 1; i < parts.length; i += 2) {
    const anchor = parts[i];
    const body = parts[i + 1] ?? "";
    const items = [];
    let currentTitle = "常规模板";
    const lines = body.split(/\r?\n/);
    for (let lineIndex = 0; lineIndex < lines.length; lineIndex++) {
      const heading = lines[lineIndex].match(/^\*\*([^*]+)\*\*\s*$/);
      if (heading) {
        currentTitle = heading[1].trim();
        continue;
      }
      const inlineHeading = lines[lineIndex].match(/\*\*([^*]+)\*\*\s*$/);
      if (inlineHeading && !lines[lineIndex].startsWith("```")) {
        currentTitle = inlineHeading[1].trim();
      }
      const open = lines[lineIndex].trim().match(/^```([A-Za-z0-9_-]+)\s*$/);
      if (!open) continue;
      const lang = open[1];
      const buf = [];
      lineIndex += 1;
      while (lineIndex < lines.length && !lines[lineIndex].trim().startsWith("```")) {
        buf.push(lines[lineIndex]);
        lineIndex += 1;
      }
      items.push({ title: currentTitle, lang, prompt: buf.join("\n").trim() });
    }
    sections.set(anchor, items);
  }
  return sections;
}

const ZH_JSON = {
  "tpl-ui": `{
  "类型": "UI 截图",
  "平台": "iOS",
  "产品": "健身应用",
  "布局": "卡片信息流 + 底部 Tab 栏",
  "风格": {
    "主题": "深色模式",
    "主色": "霓虹绿",
    "字体": "干净无衬线"
  },
  "内容": {
    "标题": "今日活动",
    "卡片": [
      {"标题": "跑步", "数据": "5.2 公里", "按钮": "开始"},
      {"标题": "卡路里", "数据": "340 kcal"}
    ]
  },
  "约束": "高保真，文字清晰可读，比例 9:16"
}`,
  "tpl-infographic": `{
  "类型": "信息图",
  "主题": "城市代谢",
  "读者": "普通公众",
  "结构": {
    "标题区": "城市生命系统图谱",
    "布局": "等轴测剖视，12 个编号模块",
    "模块": [
      {"标题": "能源", "图标": "闪电", "说明": "能源流动"},
      {"标题": "水循环", "图标": "水滴", "说明": "水流循环"}
    ]
  },
  "风格": {
    "审美": "科学图谱",
    "色彩": "低饱和、按流向分色",
    "背景": "浅色纸张质感"
  },
  "约束": "不要赛博朋克，不要乱码文字，结构必须严谨"
}`,
  "tpl-poster": `{
  "类型": "电影海报",
  "主题": "星际旅程",
  "字体": {
    "主标题": "BEYOND STARS",
    "副标题": "A New Era Begins",
    "版式": "居中，粗电影字体，下重上轻"
  },
  "视觉": {
    "主体": "宇航员剪影凝视发光星云",
    "风格": "电影光，高对比，戏剧阴影",
    "配色": "深空蓝，发光橙点缀"
  },
  "氛围": "史诗、神秘、辽阔"
}`,
  "tpl-product": `{
  "类型": "电商主图",
  "商品": {
    "名称": "降噪耳机",
    "材质": "哑光黑 + 金属点缀",
    "角度": "四分之三侧面，轻微悬浮"
  },
  "场景": {
    "背景": "极简棚拍，浅灰渐变",
    "灯光": "顶光柔箱，边缘锐利轮廓光"
  },
  "文案": {
    "角标": ["新品", "¥299"],
    "口号": "让世界安静"
  },
  "约束": "商业摄影质感，超写实材质"
}`,
  "tpl-brand": `{
  "类型": "品牌识别设计",
  "品牌": {
    "名称": "Nova Dynamics",
    "行业": "人工智能",
    "关键词": ["创新", "极简", "可信"]
  },
  "交付物": [
    "Logo 图形（神经网络节点与星形的几何融合）",
    "配色（电光蓝 + 纯白）",
    "名片样机"
  ],
  "风格": "现代企业、扁平矢量、高对比",
  "约束": "不要渐变，可缩放矢量风格，Logo 用干净白底"
}`,
  "tpl-architecture": `{
  "类型": "建筑可视化",
  "空间": {
    "类型": "现代木屋室内",
    "功能": "起居室",
    "材质": "裸露混凝土、落地玻璃、暖色木材点缀"
  },
  "环境": "窗外是积雪的茂密松林",
  "镜头": {
    "角度": "平视透视，广角镜头",
    "光线": "金色时刻，室内暖光，室外冷蓝环境光"
  },
  "渲染": "虚幻引擎 5 风格，超写实，8K，光线追踪"
}`,
  "tpl-photo": `{
  "类型": "超写实摄影",
  "主体": {
    "描述": "一名疲惫的 30 岁咖啡师正在擦杯子",
    "细节": "额角细汗、清晰毛孔、穿牛仔围裙"
  },
  "场景": "灯光偏暗的复古咖啡馆，身后窗户可见雨水",
  "相机": {
    "器材": "Sony A7R IV，50mm 镜头",
    "光圈": "f/1.4（浅景深，背景完全虚化）",
    "光线": "电影光，霓虹招牌映在湿窗上，发丝有柔和轮廓光"
  },
  "胶片感": "Kodak Portra 400 模拟，轻微颗粒"
}`,
  "tpl-illustration": `{
  "类型": "艺术插画",
  "画风": "吉卜力启发的日式动画风格",
  "场景": {
    "描述": "一头巨大的飞鲸，背上载着一座温馨小村庄",
    "细节": "风车转动，小人探出边缘张望，蓬松白云"
  },
  "配色": "明亮天蓝、浓绿、柔和粉彩点缀",
  "技法": "赛璐珞上色、精细背景、柔和魔法光晕",
  "情绪": "奇幻、冒险、怀旧"
}`,
  "tpl-character": `{
  "类型": "角色概念设定",
  "角色": {
    "身份": "义体赏金猎人",
    "外形": "银色短发、发光红色义眼、精干体格",
    "服装": "带霓虹滚边的战术风衣，手持等离子步枪"
  },
  "姿势": "动态备战，回头带笑",
  "环境": "下雨的霓虹巷（背景虚化）",
  "风格": "概念设定、利落线稿、赛博朋克高饱和配色"
}`,
  "tpl-scene": `{
  "类型": "叙事场景",
  "剧情瞬间": "远古封印被打破的那一刻",
  "环境": "被发光蓝藤蔓覆盖的崩塌石殿",
  "动作": "年轻探险者火把落地，一道巨光冲上天空",
  "氛围": {
    "情绪": "敬畏、骇人",
    "光线": "中心强光投下修长戏剧阴影"
  },
  "镜头": "低机位，强调光柱尺度"
}`,
  "tpl-history": `{
  "类型": "历史 / 东方场景",
  "设定": "夜晚的唐长安",
  "主体": {
    "身份": "贵族女子",
    "服装": "传统襦裙，精细花卉刺绣",
    "动作": "手提发光绢灯，仰看烟花"
  },
  "风格": "电影写实，带淡水墨质感",
  "细节": "准确的唐风建筑，背景有熙攘人群",
  "约束": "不要现代元素，服饰结构需符合史实"
}`,
  "tpl-document": `{
  "类型": "编辑排版",
  "文档": "时尚杂志跨页",
  "网格": "三栏网格，宽边距",
  "内容": {
    "左页": "通版高定时装照片，红裙模特",
    "右页": {
      "主标题": "THE RED RENAISSANCE",
      "正文": "（模拟文字块）",
      "引语": "「色彩即力量。」"
    }
  },
  "字体": "标题优雅衬线，正文干净无衬线",
  "配色": "黑白为主，鲜红点缀"
}`,
  "tpl-other": `{
  "类型": "自定义生成",
  "目标": "生成[具体内容]",
  "输入": {
    "主体": "[主体细节]",
    "场景": "[背景与情境]",
    "风格": "[艺术/视觉风格]",
    "配色": "[色彩方案]"
  },
  "质量约束": {
    "分辨率": "8K，超精细",
    "比例": "[例如 16:9]",
    "构图": "[例如 三分法]"
  },
  "输出要求": {
    "用途": "[使用场景]",
    "重点": "[需要强调的关键元素]"
  }
}`,
};

function pickItem(items, titles, lang) {
  for (const title of titles) {
    const found = items.find((item) => item.title === title && item.lang === lang && item.prompt);
    if (found) return found;
  }
  return items.find((item) => item.lang === lang && item.prompt) ?? null;
}

function imageFileName(imagePath) {
  return String(imagePath || "").split("/").filter(Boolean).pop() || "";
}

const casesRaw = JSON.parse(await readFile(path.join(sourceRoot, "data", "cases.json"), "utf8"));
const styleRaw = JSON.parse(await readFile(path.join(sourceRoot, "data", "style-library.json"), "utf8"));
const templatesMd = repairBrokenFences(await readFile(path.join(sourceRoot, "docs", "templates.md"), "utf8"));
const sections = parseTemplateSections(templatesMd);

const cases = casesRaw.cases.map((item) => ({
  id: `case:${item.id}`,
  caseId: item.id,
  kind: "case",
  title: item.title,
  prompt: item.prompt,
  category: item.category,
  styles: item.styles ?? [],
  scenes: item.scenes ?? [],
  tags: [...(item.styles ?? []), ...(item.scenes ?? [])],
  image: imageFileName(item.image),
  sourceLabel: item.sourceLabel ?? "",
  sourceUrl: item.sourceUrl ?? "",
  featured: Boolean(item.featured),
}));

const usedExtras = new Set();
const templates = styleRaw.templates.map((item) => {
  const match = TEMPLATE_MATCH[item.id] ?? { titles: ["常规模板"] };
  const sectionItems = sections.get(item.anchor) ?? [];
  const textItem = pickItem(sectionItems, match.titles, "text");
  const jsonItem = pickItem(sectionItems, [...match.titles, "JSON 进阶模板（推荐给 Agent 调用）"], "json")
    ?? sectionItems.find((entry) => entry.lang === "json");
  const variants = (match.extras ?? [])
    .map((title) => {
      const found = sectionItems.find((entry) => entry.title === title && entry.prompt);
      if (!found) return null;
      usedExtras.add(`${item.anchor}:${title}`);
      return { title, lang: found.lang, prompt: found.prompt };
    })
    .filter(Boolean);
  if (!textItem?.prompt) {
    throw new Error(`missing prompt body for template ${item.id} (${item.anchor})`);
  }
  return {
    id: `tpl:${item.id}`,
    templateId: item.id,
    kind: "template",
    title: localized(item.title, item.id),
    titleEn: typeof item.title === "object" ? item.title.en ?? "" : "",
    description: localized(item.description),
    category: item.category,
    styles: item.styles ?? [],
    scenes: item.scenes ?? [],
    tags: item.tags ?? [],
    useWhen: localized(item.useWhen),
    guidance: localizedList(item.guidance),
    pitfalls: localizedList(item.pitfalls),
    exampleCases: item.exampleCases ?? [],
    prompt: textItem.prompt,
    jsonPrompt: ZH_JSON[item.anchor] ?? (jsonItem?.lang === "json" ? jsonItem.prompt : ""),
    promptTitle: textItem.title,
    variants,
    image: imageFileName(item.cover),
    anchor: item.anchor,
  };
});

const missing = templates.filter((item) => !item.prompt);
if (missing.length) {
  throw new Error(`templates missing prompt: ${missing.map((item) => item.id).join(", ")}`);
}

const catalog = {
  version: 1,
  generatedAt: new Date().toISOString(),
  source: {
    name: "awesome-gpt-image-2",
    url: "https://github.com/freestylefly/awesome-gpt-image-2",
    license: "MIT",
  },
  imageBaseUrl: IMAGE_BASE,
  categories: casesRaw.categories,
  styles: casesRaw.styles,
  scenes: casesRaw.scenes,
  tagLabels: styleRaw.tagLabels ?? {},
  caseCount: cases.length,
  templateCount: templates.length,
  cases,
  templates,
};

await mkdir(outDir, { recursive: true });
await writeFile(path.join(outDir, "catalog.json"), `${JSON.stringify(catalog)}\n`, "utf8");
console.log(`wrote ${cases.length} cases, ${templates.length} templates -> src/data/promptlib/catalog.json`);
console.log(`bytes=${Buffer.byteLength(JSON.stringify(catalog))}`);
