export interface MarkedSection {
  marker: string;
  body: string;
}

export function stripThinking(text: string): string {
  return text.replace(/<think>[\s\S]*?<\/think>/gi, "").replace(/<think>[\s\S]*$/gi, "").trim();
}

export function parseMarkedSections(raw: string): MarkedSection[] {
  const text = stripThinking(raw);
  const matches = [...text.matchAll(/【([^】]+)】/g)];
  if (!matches.length) return text ? [{ marker: "", body: text }] : [];
  const sections: MarkedSection[] = [];
  const preamble = text.slice(0, matches[0].index).trim();
  if (preamble) sections.push({ marker: "", body: preamble });
  for (let index = 0; index < matches.length; index += 1) {
    const match = matches[index];
    const start = (match.index ?? 0) + match[0].length;
    const end = matches[index + 1]?.index ?? text.length;
    sections.push({ marker: match[1].trim(), body: text.slice(start, end).trim() });
  }
  return sections.filter((section) => section.marker || section.body);
}
