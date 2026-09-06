export interface StoryboardShot {
  shotNo: number;
  shotType?: string;
  composition?: string;
  light?: string;
  camera?: string;
  action?: string;
  emotion?: string;
  prompt?: string;
}

export interface MarkedSection {
  marker: string;
  body: string;
}

/** Remove model reasoning blocks before displaying or persisting AI output. */
export function stripThinking(text: string): string {
  return text
    .replace(/<think>[\s\S]*?<\/think>/gi, "")
    .replace(/<think>[\s\S]*$/gi, "")
    .trim();
}

export function parseMarkedSections(raw: string): MarkedSection[] {
  const text = stripThinking(raw);
  const matches = [...text.matchAll(/【([^】]+)】/g)];
  if (!matches.length) return text ? [{ marker: "", body: text }] : [];
  const sections: MarkedSection[] = [];
  const preamble = text.slice(0, matches[0].index).trim();
  if (preamble) sections.push({ marker: "", body: preamble });
  for (let index = 0; index < matches.length; index++) {
    const match = matches[index];
    const start = (match.index ?? 0) + match[0].length;
    const end = matches[index + 1]?.index ?? text.length;
    sections.push({ marker: match[1].trim(), body: text.slice(start, end).trim() });
  }
  return sections.filter((section) => section.marker || section.body);
}

function balancedObject(text: string): string | null {
  let start = -1;
  let depth = 0;
  let inString = false;
  let escaped = false;

  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (start === -1) {
      if (char !== "{") continue;
      start = i;
      depth = 1;
      continue;
    }
    if (inString) {
      if (escaped) escaped = false;
      else if (char === "\\") escaped = true;
      else if (char === '"') inString = false;
      continue;
    }
    if (char === '"') inString = true;
    else if (char === "{") depth++;
    else if (char === "}" && --depth === 0) return text.slice(start, i + 1);
  }
  return null;
}

function asText(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

/** Parse storyboard JSON even when the model wraps it in prose or a code fence. */
export function parseStoryboardShots(raw: string): StoryboardShot[] {
  const text = stripThinking(raw);
  const fenced = /```(?:json)?\s*([\s\S]*?)```/i.exec(text)?.[1];
  const candidate = balancedObject(fenced ?? text);
  if (!candidate) return [];

  try {
    const value = JSON.parse(candidate) as { shots?: unknown };
    if (!Array.isArray(value.shots)) return [];
    return value.shots.flatMap((item, index) => {
      if (!item || typeof item !== "object") return [];
      const shot = item as Record<string, unknown>;
      const shotNo = typeof shot.shotNo === "number" && Number.isFinite(shot.shotNo)
        ? shot.shotNo
        : index + 1;
      const normalized: StoryboardShot = {
        shotNo,
        shotType: asText(shot.shotType),
        composition: asText(shot.composition),
        light: asText(shot.light),
        camera: asText(shot.camera),
        action: asText(shot.action),
        emotion: asText(shot.emotion),
        prompt: asText(shot.prompt),
      };
      return normalized.prompt || normalized.action || normalized.shotType ? [normalized] : [];
    });
  } catch {
    return [];
  }
}
