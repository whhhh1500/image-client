package restapi

import (
	"fmt"
	"reflect"
	"strings"
	"testing"
)

func TestNormalizeStoryboardReferenceAssets(t *testing.T) {
	anchor := "# 视频分镜\n\n## 第1镜\n\n### 参考方式\nreference\n\n### 参考资产\ncharacter:hero:v1、scene:road:v1\n\n### 视频 Prompt\n测试"
	normalized, shots := normalizeStoryboardReferenceAssets(anchor)
	if !reflect.DeepEqual(shots, []uint64{1}) || !strings.Contains(normalized, "### 参考方式\ntext") || !strings.Contains(normalized, "### 参考资产\n无") {
		t.Fatalf("shots %v, normalized %q", shots, normalized)
	}

	media := strings.Replace(anchor, "character:hero:v1、scene:road:v1", "image_asset_123", 1)
	if unchanged, shots := normalizeStoryboardReferenceAssets(media); len(shots) != 0 || unchanged != media {
		t.Fatalf("real media asset rewritten: %v %q", shots, unchanged)
	}

	// Mixed lists keep their real asset; empty lists, "无" and anchor-only
	// lists fall back to text; CRLF input comes back with LF.
	doc := "## 第1镜\r\n### 参考方式\r\nfirst_frame\r\n### 参考资产\r\nstyle:a:v1, img_9\r\n" +
		"## 第2镜\n### 参考方式\nreference\n### 参考资产\n无\n" +
		"## 第3镜\n### 参考方式\ntext\n### 参考资产\ncharacter:x:v1\n" +
		"## 第4镜\n### 参考方式\nfirst_frame\n### 参考资产\nprop:cup:v1，state:prop:cup:ch1:broken:v1\n---\n"
	normalized, shots = normalizeStoryboardReferenceAssets(doc)
	if !reflect.DeepEqual(shots, []uint64{2, 4}) || strings.Contains(normalized, "\r") ||
		!strings.Contains(normalized, "### 参考方式\nfirst_frame\n### 参考资产\nstyle:a:v1, img_9") {
		t.Fatalf("shots %v, normalized %q", shots, normalized)
	}
	if !strings.HasSuffix(normalized, "## 第4镜\n### 参考方式\ntext\n### 参考资产\n无\n\n") {
		t.Fatalf("shot 4 = %q", normalized)
	}
}

func TestValidateAgentOutput(t *testing.T) {
	duplicateQC := "【质量评分】80\n【原文忠实度】80\n【剧情吸引力】70\n【视觉独特性】60\n【可执行性】80\n【硬规则结果】通过\n【锚点检查】通过\n【逐镜检查】通过\n【阻断问题】无\n【一般问题】无\n【结论】可生成\n【结论】需修改"
	valid := storyboardDoc("text", "无")
	twoShots := valid + strings.Replace(valid, "# 视频分镜\n\n## 第1镜", "## 第2镜", 1)
	for _, tc := range []struct {
		agent, output, err string
	}{
		{"writer", writerDoc, ""},
		{"writer", "  \n ", "Agent 没有返回内容"},
		{"storyboard", "The prompt could not be submitted. Prohibited Use policy.", "文本模型返回了安全策略拒答，拒答内容不会保存为 Agent 文档"},
		{"director", "提示词无法提交：包含敏感内容", "文本模型返回了安全策略拒答，拒答内容不会保存为 Agent 文档"},
		{"director", "# 改编规划\n只有标题", "Agent 输出缺少固定字段：## 输入边界、## 项目硬约束、## 核心戏剧判断、## 叙事节拍、## 核心视觉母题、## 风险与自检"},
		{"qc", duplicateQC, "Agent 输出存在重复字段：【结论】"},
		{"unknown", "anything", ""},
		{"storyboard", valid, ""},
		{"storyboard", twoShots, ""},
		{"storyboard", strings.Replace(twoShots, "## 第2镜", "## 第3镜", 1), "视频分镜镜号缺失或不连续"},
		{"storyboard", "# 视频分镜\n没有镜头", "视频分镜镜号缺失或不连续"},
		{"storyboard", valid + "## 附录\n", "视频分镜包含未知的二级标题"},
		{"storyboard", "# 视频分镜\n### 时长\n5\n" + strings.TrimPrefix(valid, "# 视频分镜\n"), "视频分镜在第1镜之前包含镜头字段"},
		{"storyboard", strings.Replace(valid, "### 道具锚\n", "", 1), "第1镜字段数量不正确"},
		{"storyboard", strings.Replace(valid, "### 道具锚\n", "### 角色锚\n", 1), "第1镜字段“角色锚”缺失或重复"},
		{"storyboard", strings.Replace(valid, "### 道具锚\n", "### 音效\n", 1), "第1镜字段“道具锚”缺失或重复"},
	} {
		err := validateAgentOutput(tc.agent, tc.output)
		if (err == nil) != (tc.err == "") || (err != nil && err.Error() != tc.err) {
			t.Fatalf("%s %q: error %v, want %q", tc.agent, tc.output, err, tc.err)
		}
	}
}

func TestShotNumber(t *testing.T) {
	for _, tc := range []struct {
		heading string
		shot    uint64
		ok      bool
	}{
		{"## 第1镜", 1, true},
		{"  ##第12镜  ", 12, true},
		{"## 第1-2镜", 12, true},
		{"### 第1镜", 0, false},
		{"## 第一镜", 0, false},
		{"## 第1镜头", 0, false},
		{"# 第1镜", 0, false},
	} {
		if shot, ok := shotNumber(tc.heading); shot != tc.shot || ok != tc.ok {
			t.Fatalf("%q: %d %v", tc.heading, shot, ok)
		}
	}
}

func TestValidateAgentTools(t *testing.T) {
	tool := func(name string) ToolDef { return ToolDef{Name: name, Description: "d", System: "s"} }
	many := make([]ToolDef, maxAgentTools+1)
	for i := range many {
		many[i] = tool(fmt.Sprint(i))
	}
	for _, tc := range []struct {
		tools []ToolDef
		err   string
	}{
		{[]ToolDef{tool("a"), tool("b")}, ""},
		{nil, "tools 数量必须在 1 到 32 之间"},
		{many, "tools 数量必须在 1 到 32 之间"},
		{[]ToolDef{tool(strings.Repeat("n", 101))}, "tools.name 超过 100 个字符"},
		{[]ToolDef{tool("a"), tool(" a ")}, "tools.name 重复：a"},
		{[]ToolDef{{Name: "a", Description: strings.Repeat("d", 2001), System: "s"}}, "tools.description 超过 2000 个字符"},
	} {
		err := validateAgentTools(tc.tools)
		if (err == nil) != (tc.err == "") || (err != nil && err.Error() != tc.err) {
			t.Fatalf("%d tools: error %v, want %q", len(tc.tools), err, tc.err)
		}
	}
}

func TestTextLines(t *testing.T) {
	for _, tc := range []struct {
		in   string
		want []string
	}{
		{"", nil},
		{"a", []string{"a"}},
		{"a\n", []string{"a"}},
		{"a\r\nb\n\nc", []string{"a", "b", "", "c"}},
		{"a\rb", []string{"a\rb"}},
	} {
		if got := textLines(tc.in); !reflect.DeepEqual(got, tc.want) {
			t.Fatalf("%q: %q", tc.in, got)
		}
	}
}
