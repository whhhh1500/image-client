package comic

import (
	"fmt"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

// heading is a Markdown heading outside code fences: its title and the byte
// range of its whole line.
type heading struct {
	title      string
	start, end int
}

func headings(md string) []heading {
	var out []heading
	fence := false
	for offset := 0; offset < len(md); {
		end := len(md)
		if i := strings.IndexByte(md[offset:], '\n'); i >= 0 {
			end = offset + i + 1
		}
		text := strings.TrimSpace(md[offset:end])
		switch {
		case strings.HasPrefix(text, "```") || strings.HasPrefix(text, "~~~"):
			fence = !fence
		case !fence && strings.HasPrefix(text, "#"):
			title := strings.TrimSpace(strings.TrimRight(strings.TrimSpace(strings.TrimLeft(text, "#")), "#"))
			if title != "" {
				out = append(out, heading{title: title, start: offset, end: end})
			}
		}
		offset = end
	}
	return out
}

// headingLevel counts the leading '#' of the heading line starting at at.
func headingLevel(md string, at int) int {
	line := strings.TrimLeftFunc(md[at:], unicode.IsSpace)
	return len(line) - len(strings.TrimLeft(line, "#"))
}

// canonicalSection maps the natural aliases models and people use to the
// section names the validator knows.
func canonicalSection(name string) string {
	switch name {
	case "相关世界观与场景":
		return "世界观与场景"
	case "当前剧情对人物锚点的补充":
		return "人物锚点补充"
	case "本页剧情与分镜":
		return "剧情与分镜"
	}
	return name
}

// section returns the trimmed body under the first heading named name, up to
// the next heading of the same or a higher level.
func section(md, name string) (string, bool) {
	hs := headings(md)
	for i, h := range hs {
		if canonicalSection(h.title) != name {
			continue
		}
		own := headingLevel(md, h.start)
		end := len(md)
		for _, next := range hs[i+1:] {
			if headingLevel(md, next.start) <= own {
				end = next.start
				break
			}
		}
		return strings.TrimSpace(md[h.end:end]), true
	}
	return "", false
}

func sectionOrEmpty(md, name string) string {
	s, _ := section(md, name)
	return s
}

// numbered reads the page or panel number of a "第N页" / "第N格" heading. It
// accepts fullwidth digits, simple Chinese numerals and a separated subtitle
// ("第3页（高潮）", "第3页 雨夜"); text glued on ("第1页剧情") is not a heading.
func numbered(title string, suffix rune) (int64, bool) {
	digits, tail, ok := splitNumbered(title, suffix)
	if !ok {
		return 0, false
	}
	if tail != "" {
		first, _ := utf8.DecodeRuneInString(tail)
		if !unicode.IsSpace(first) && !strings.ContainsRune("（(：:·-—|、【[，,", first) {
			return 0, false
		}
	}
	return parseNumber(digits)
}

// splitNumbered splits "第<number><suffix><tail>" into the normalised number
// text and the tail.
func splitNumbered(title string, suffix rune) (digits, tail string, ok bool) {
	rest, ok := strings.CutPrefix(strings.TrimSpace(title), "第")
	if !ok {
		return "", "", false
	}
	end := strings.IndexRune(rest, suffix)
	if end < 0 {
		return "", "", false
	}
	var b strings.Builder
	for _, c := range rest[:end] {
		if unicode.IsSpace(c) {
			continue
		}
		if c >= '０' && c <= '９' {
			c = c - '０' + '0'
		}
		b.WriteRune(c)
	}
	return b.String(), rest[end+utf8.RuneLen(suffix):], true
}

func parseNumber(text string) (int64, bool) {
	var n int64
	if text != "" && strings.Trim(text, "0123456789") == "" {
		parsed, err := strconv.ParseInt(text, 10, 64)
		if err != nil {
			return 0, false
		}
		n = parsed
	} else {
		parsed, ok := chineseNumber(text)
		if !ok {
			return 0, false
		}
		n = parsed
	}
	return n, n > 0
}

// chineseNumber reads 一..九十九 (and 两), enough for page and panel numbers.
func chineseNumber(text string) (int64, bool) {
	digit := func(c rune) (int64, bool) {
		if c == '两' {
			return 2, true
		}
		for i, d := range []rune("零一二三四五六七八九") {
			if d == c {
				return int64(i), true
			}
		}
		return 0, false
	}
	r := []rune(text)
	switch {
	case len(r) == 1 && r[0] == '十':
		return 10, true
	case len(r) == 1:
		return digit(r[0])
	case len(r) == 2 && r[0] == '十':
		u, ok := digit(r[1])
		return 10 + u, ok
	case len(r) == 2 && r[1] == '十':
		t, ok := digit(r[0])
		return t * 10, ok
	case len(r) == 3 && r[1] == '十':
		t, ok1 := digit(r[0])
		u, ok2 := digit(r[2])
		return t*10 + u, ok1 && ok2
	}
	return 0, false
}

// looksLikePageHeading flags "第…页" whose number cannot be read, such as
// "第3a页" or "第一百页".
func looksLikePageHeading(title string) bool {
	digits, _, ok := splitNumbered(title, '页')
	if !ok || utf8.RuneCountInString(digits) > 4 {
		return false
	}
	_, readable := parseNumber(digits)
	return !readable
}

// page is one "# 第N页" block of a storyboard or page prompt.
type page struct {
	no   int64
	body string
}

func pages(md string) []page {
	type start struct {
		no int64
		at int
	}
	var starts []start
	for _, h := range headings(md) {
		if n, ok := numbered(h.title, '页'); ok {
			starts = append(starts, start{n, h.start})
		}
	}
	out := make([]page, len(starts))
	for i, s := range starts {
		end := len(md)
		if i+1 < len(starts) {
			end = starts[i+1].at
		}
		out[i] = page{no: s.no, body: strings.TrimSpace(md[s.at:end])}
	}
	return out
}

func pageNumbers(md string) []int64 {
	ps := pages(md)
	out := make([]int64, len(ps))
	for i, p := range ps {
		out[i] = p.no
	}
	return out
}

// requireSections reports each named section that is missing or has no
// content line.
func requireSections(md string, names []string) []string {
	var issues []string
	for _, name := range names {
		body, ok := section(md, name)
		filled := false
		if ok {
			for line := range strings.SplitSeq(body, "\n") {
				line = strings.TrimSpace(line)
				if line != "" && !strings.HasPrefix(line, "#") && !strings.HasPrefix(line, "```") && !strings.HasPrefix(line, "~~~") {
					filled = true
					break
				}
			}
		}
		if !filled {
			issues = append(issues, fmt.Sprintf("请补全“%s”标题及内容", name))
		}
	}
	return issues
}

var (
	storyboardSections = []string{"本页剧情", "分镜", "画面文字", "人物状态"}
	pagePromptSections = []string{"画面要求", "世界观与场景", "人物锚点", "人物锚点补充", "剧情与分镜", "画面文字", "连续性要求"}
)

// validate lists the structural problems of a document; empty means complete.
func validate(kind string, pageNo *int64, md string) []string {
	out := []string{}
	switch kind {
	case kindSettings:
		out = append(out, requireSections(md, []string{"世界观", "画风", "人物锚点"})...)
	case kindScript:
		out = append(out, requireSections(md, []string{"剧情", "场景与对白", "人物锚点补充"})...)
	case kindStoryboard, kindPagePrompt:
		ps := pages(md)
		if len(ps) == 0 {
			out = append(out, "请使用“# 第1页”等正整数页标题")
		}
		// Only level-1 headings: deeper ones may legitimately mention pages
		// ("### 第一、二页回顾") and were never page boundaries.
		for _, h := range headings(md) {
			if headingLevel(md, h.start) == 1 && looksLikePageHeading(h.title) {
				out = append(out, fmt.Sprintf("无法识别页标题“%s”，请写成“# 第N页”", h.title))
			}
		}
		if kind == kindPagePrompt && (len(ps) != 1 || pageNo == nil || ps[0].no != *pageNo) {
			out = append(out, "页 Prompt 必须只包含与页号一致的一页")
		}
		if kind == kindStoryboard {
			for i, p := range ps {
				if p.no != int64(i)+1 {
					out = append(out, "分页编号须从第1页开始连续且不重复")
					break
				}
			}
		}
		names, panels := pagePromptSections, "剧情与分镜"
		if kind == kindStoryboard {
			names, panels = storyboardSections, "分镜"
		}
		for _, p := range ps {
			local := requireSections(p.body, names)
			hasPanel := false
			for _, h := range headings(sectionOrEmpty(p.body, panels)) {
				if _, ok := numbered(h.title, '格'); ok {
					hasPanel = true
					break
				}
			}
			if !hasPanel {
				local = append(local, "请添加“第1格”等分镜标题和画面内容")
			}
			for _, h := range headings(p.body) {
				if _, ok := numbered(h.title, '格'); !ok {
					continue
				}
				if body, ok := section(p.body, h.title); !ok || body == "" {
					local = append(local, fmt.Sprintf("请补全%s的画面内容", h.title))
				}
			}
			for _, issue := range local {
				out = append(out, fmt.Sprintf("第%d页：%s", p.no, issue))
			}
		}
	default:
		out = append(out, "未知文档类型")
	}
	return out
}

// documentLabel names a document the way messages and prompts refer to it.
func documentLabel(kind string, pageNo *int64) string {
	switch kind {
	case kindSettings:
		return "作品设定"
	case kindScript:
		return "本章剧本"
	case kindStoryboard:
		return "分页分镜"
	}
	return fmt.Sprintf("第%d页 Prompt", orZero(pageNo))
}

// stageRank orders kinds along the pipeline.
func stageRank(kind string) int {
	switch kind {
	case kindSettings:
		return 0
	case kindScript:
		return 1
	case kindStoryboard:
		return 2
	}
	return 3
}
