package restapi

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/google/uuid"

	"imageclient/internal/store"
)

// agentStep is one document-producing agent endpoint. Each run sends the
// input with the step's built-in system prompt (unless the caller supplies
// one), checks the answer's fixed structure and saves it as a text asset.
type agentStep struct {
	id           string // agentId recorded with the document
	label        string // document title and history source
	documentType string
	operation    string // LLM log label and history change operation
	system       string // built-in system prompt
}

var (
	directorStep    = agentStep{"director", "导演规划", "director", "rest_agent_director", directorPrompt}
	writerStep      = agentStep{"writer", "剧本", "script", "rest_agent_writer", writerPrompt}
	storyboardStep  = agentStep{"storyboard", "分镜", "storyboard", "rest_agent_storyboard", storyboardPrompt}
	consistencyStep = agentStep{"consistency", "一致性", "consistency", "rest_agent_consistency", consistencyPrompt}
	qcStep          = agentStep{"qc", "质检", "qc", "rest_agent_qc", qcPrompt}
)

type textBody struct {
	System string  `json:"system"`
	Input  string  `json:"input"`
	Model  *string `json:"model"`
}

func (textBody) checkFields(f fieldSet) error { return f.require("system", "input") }

func (s *Server) textCompletion(w http.ResponseWriter, r *http.Request) {
	var body textBody
	if !readJSON(w, r, maxJSONBody, &body) {
		return
	}
	if err := firstError(
		requiredText(body.System, "system", maxTextChars),
		requiredText(body.Input, "input", maxTextChars),
		optionalText(body.Model, "model", maxModelChars),
	); err != nil {
		fail(w, err)
		return
	}
	cfg := s.cfg.Get()
	if !cfg.LLMReady() {
		fail(w, errLLMNotConfigured)
		return
	}
	completion, err := s.text.Complete(r.Context(), TextRequest{
		Model: valueOr(body.Model, cfg.LLMModel), System: body.System, Input: body.Input, Operation: "rest_text_completion",
	})
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"result": completion.Content})
}

type stepBody struct {
	Input     string  `json:"input"`
	System    *string `json:"system"`
	Model     *string `json:"model"`
	ProjectID *string `json:"projectId"`
}

func (stepBody) checkFields(f fieldSet) error { return f.require("input") }

// runStep serves one document-producing agent: complete, validate, save and
// record the document.
func (s *Server) runStep(step agentStep) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var body stepBody
		if !readJSON(w, r, maxJSONBody, &body) {
			return
		}
		if err := firstError(
			requiredText(body.Input, "input", maxTextChars),
			optionalText(body.System, "system", maxTextChars),
			optionalText(body.Model, "model", maxModelChars),
			optionalText(body.ProjectID, "projectId", maxProjectIDChars),
		); err != nil {
			fail(w, err)
			return
		}
		cfg := s.cfg.Get()
		if !cfg.LLMReady() {
			fail(w, errLLMNotConfigured)
			return
		}
		ctx := r.Context()
		model := valueOr(body.Model, cfg.LLMModel)
		system := valueOr(body.System, step.system)
		completion, err := s.text.Complete(ctx, TextRequest{Model: model, System: system, Input: body.Input, Operation: step.operation})
		if err != nil {
			fail(w, err)
			return
		}
		// A document must never be saved from a truncated answer.
		if !completion.Completed {
			fail(w, errors.New("文本服务返回不完整结果（可能被截断），未保存文档；请重试或缩短输入"))
			return
		}
		// Reasoning models inline <think> blocks, which would fail validation
		// or leak into the saved document.
		result := s.text.StripThinking(completion.Content)
		corrected := []uint64{}
		if step.id == storyboardStep.id {
			result, corrected = normalizeStoryboardReferenceAssets(result)
		}
		if err := validateAgentOutput(step.id, result); err != nil {
			fail(w, err)
			return
		}
		asset, err := s.files.SaveText(ctx, step.label, result, model)
		if err != nil {
			fail(w, err)
			return
		}
		normalization := map[string]any{"correctedReferenceShots": corrected}
		params := map[string]any{
			"text":           result,
			"title":          step.label,
			"documentType":   step.documentType,
			"documentId":     uuid.NewString(),
			"version":        1,
			"changeType":     "generated",
			"agentId":        step.id,
			"provenance":     store.GeneratedProvenance(body.Input, body.Input, &system, []any{}, []any{}),
			"updatedAt":      time.Now().UnixMilli(),
			"origin":         "rest_api",
			"workflowStatus": "unreviewed",
			"normalization":  normalization,
		}
		assets := []store.AssetRef{asset}
		if !s.record(w, r, step.operation, assets, store.AssetMeta{Source: step.label, Model: model, ProjectID: body.ProjectID, Params: params}) {
			return
		}
		writeJSON(w, http.StatusOK, map[string]any{
			"result": result, "asset": asset, "workflowStatus": "unreviewed", "normalization": normalization,
		})
	}
}

type orchestrationBody struct {
	System    string    `json:"system"`
	Input     string    `json:"input"`
	Model     *string   `json:"model"`
	Tools     []ToolDef `json:"tools"`
	ProjectID *string   `json:"projectId"`
}

func (orchestrationBody) checkFields(f fieldSet) error { return f.require("system", "input", "tools") }

func (s *Server) orchestrate(w http.ResponseWriter, r *http.Request) {
	var body orchestrationBody
	if !readJSON(w, r, maxJSONBody, &body) {
		return
	}
	if err := firstError(
		requiredText(body.System, "system", maxTextChars),
		requiredText(body.Input, "input", maxTextChars),
		optionalText(body.Model, "model", maxModelChars),
		optionalText(body.ProjectID, "projectId", maxProjectIDChars),
		validateAgentTools(body.Tools),
	); err != nil {
		fail(w, err)
		return
	}
	cfg := s.cfg.Get()
	if !cfg.LLMReady() {
		fail(w, errLLMNotConfigured)
		return
	}
	ctx := r.Context()
	model := valueOr(body.Model, cfg.LLMModel)
	result, err := s.text.RunAgent(ctx, AgentRequest{System: body.System, Input: body.Input, Model: model, Tools: body.Tools})
	if err != nil {
		fail(w, err)
		return
	}
	const title = "Agent编排"
	asset, err := s.files.SaveText(ctx, title, result, model)
	if err != nil {
		fail(w, err)
		return
	}
	params := map[string]any{
		"text":           result,
		"title":          title,
		"documentType":   "orchestration",
		"documentId":     uuid.NewString(),
		"version":        1,
		"changeType":     "generated",
		"provenance":     store.GeneratedProvenance(body.Input, body.Input, &body.System, []any{}, []any{}),
		"updatedAt":      time.Now().UnixMilli(),
		"origin":         "rest_api",
		"workflowStatus": "unreviewed",
	}
	if !s.record(w, r, "rest_agent_orchestration", []store.AssetRef{asset}, store.AssetMeta{Source: title, Model: model, ProjectID: body.ProjectID, Params: params}) {
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"result": result, "asset": asset})
}

// agentMarkers are the headings each agent's document must contain; all but
// the storyboard's exactly once.
var agentMarkers = map[string][]string{
	"director": {"# 改编规划", "## 输入边界", "## 项目硬约束", "## 核心戏剧判断", "## 叙事节拍", "## 核心视觉母题", "## 风险与自检"},
	"writer":   {"# 视频剧本", "## 改编边界", "## 人物当前状态", "## 分场剧本", "## 编剧自检"},
	"consistency": {"# 视频锚点", "## 固定锚定", "### 画风锚", "### 角色基础锚", "### 长期场景锚", "### 核心道具锚",
		"## 剧情锚点", "### 角色状态锚", "### 场景状态锚", "### 道具状态锚", "### 变化记录"},
	"storyboard": {"# 视频分镜", "### 时长", "### 画风锚", "### 场景锚", "### 角色锚", "### 参考方式", "### 参考资产", "### 视频 Prompt"},
	"qc": {"【质量评分】", "【原文忠实度】", "【剧情吸引力】", "【视觉独特性】", "【可执行性】", "【硬规则结果】",
		"【锚点检查】", "【逐镜检查】", "【阻断问题】", "【一般问题】", "【结论】"},
}

// refusalMarkers identify a provider's safety refusal returned as content.
var refusalMarkers = []string{"prompt could not be submitted", "prohibited use policy", "sensitive words", "violates a policy"}

// storyboardFields are the fields of every storyboard shot, each exactly once.
var storyboardFields = []string{
	"场次", "景别", "构图", "光线", "运镜", "画面动作", "情绪", "时长", "起始状态", "动作过程",
	"结束状态", "承接镜头", "画风锚", "场景锚", "角色锚", "道具锚", "参考方式", "参考资产", "来源对白", "视频 Prompt",
}

// validateAgentOutput rejects empty answers, safety refusals and documents
// missing (or repeating) their fixed structure, so none is saved.
func validateAgentOutput(agentID, output string) error {
	text := strings.TrimSpace(output)
	if text == "" {
		return errors.New("Agent 没有返回内容")
	}
	lower := asciiLower(text)
	refused := slices.ContainsFunc(refusalMarkers, func(m string) bool { return strings.Contains(lower, m) })
	if refused || (strings.Contains(text, "无法提交") && (strings.Contains(text, "敏感") || strings.Contains(text, "安全策略"))) {
		return errors.New("文本模型返回了安全策略拒答，拒答内容不会保存为 Agent 文档")
	}
	if agentID == "storyboard" {
		if err := validateStoryboardStructure(text); err != nil {
			return err
		}
	}
	markers, ok := agentMarkers[agentID]
	if !ok {
		return nil
	}
	var missing, repeated []string
	for _, marker := range markers {
		switch n := strings.Count(text, marker); {
		case n == 0:
			missing = append(missing, marker)
		case n > 1 && agentID != "storyboard":
			repeated = append(repeated, marker)
		}
	}
	if len(missing) > 0 {
		return fmt.Errorf("Agent 输出缺少固定字段：%s", strings.Join(missing, "、"))
	}
	if len(repeated) > 0 {
		return fmt.Errorf("Agent 输出存在重复字段：%s", strings.Join(repeated, "、"))
	}
	return nil
}

type shotStart struct {
	line int
	shot uint64
}

func shotStarts(lines []string) []shotStart {
	var starts []shotStart
	for i, line := range lines {
		if shot, ok := shotNumber(line); ok {
			starts = append(starts, shotStart{i, shot})
		}
	}
	return starts
}

// blockEnd is the line after shot p's block.
func blockEnd(starts []shotStart, p, total int) int {
	if p+1 < len(starts) {
		return starts[p+1].line
	}
	return total
}

// validateStoryboardStructure requires consecutively numbered "## 第N镜"
// shots, no other second-level headings, and exactly the storyboard fields
// as "### " headings in every shot.
func validateStoryboardStructure(text string) error {
	lines := textLines(text)
	starts := shotStarts(lines)
	if len(starts) == 0 {
		return errors.New("视频分镜镜号缺失或不连续")
	}
	isStart := make(map[int]bool, len(starts))
	for i, st := range starts {
		if st.shot != uint64(i+1) {
			return errors.New("视频分镜镜号缺失或不连续")
		}
		isStart[st.line] = true
	}
	for i, line := range lines {
		if strings.HasPrefix(trimLeft(line), "## ") && !isStart[i] {
			return errors.New("视频分镜包含未知的二级标题")
		}
	}
	for _, line := range lines[:starts[0].line] {
		if strings.HasPrefix(trimLeft(line), "### ") {
			return errors.New("视频分镜在第1镜之前包含镜头字段")
		}
	}
	for p, st := range starts {
		var labels []string
		for _, line := range lines[st.line+1 : blockEnd(starts, p, len(lines))] {
			if label, ok := strings.CutPrefix(strings.TrimSpace(line), "### "); ok {
				labels = append(labels, strings.TrimSpace(label))
			}
		}
		if len(labels) != len(storyboardFields) {
			return fmt.Errorf("第%d镜字段数量不正确", st.shot)
		}
		for _, field := range storyboardFields {
			if countOf(labels, field) != 1 {
				return fmt.Errorf("第%d镜字段“%s”缺失或重复", st.shot, field)
			}
		}
		for _, label := range labels {
			if !slices.Contains(storyboardFields, label) {
				return fmt.Errorf("第%d镜包含未知字段", st.shot)
			}
		}
	}
	return nil
}

// shotNumber reads "## 第N镜" headings; N is every ASCII digit in the heading.
func shotNumber(heading string) (uint64, bool) {
	value, ok := strings.CutPrefix(strings.TrimSpace(heading), "##")
	if !ok {
		return 0, false
	}
	value = strings.TrimSpace(value)
	if !strings.HasPrefix(value, "第") || !strings.HasSuffix(value, "镜") {
		return 0, false
	}
	digits := strings.Map(func(r rune) rune {
		if r >= '0' && r <= '9' {
			return r
		}
		return -1
	}, value)
	n, err := strconv.ParseUint(digits, 10, 64)
	return n, err == nil
}

// normalizeStoryboardReferenceAssets corrects a type error some LLMs make:
// text anchor IDs (style:, character:, ...) are not image/video asset IDs
// and cannot select a reference mode. Shots whose reference assets are all
// anchors (or none) become "text" + "无"; real media asset IDs are kept. It
// returns the corrected Markdown and the corrected shot numbers.
func normalizeStoryboardReferenceAssets(markdown string) (string, []uint64) {
	normalized := strings.ReplaceAll(strings.ReplaceAll(markdown, "\r\n", "\n"), "\r", "\n")
	lines := strings.Split(normalized, "\n")
	starts := shotStarts(lines)
	corrected := []uint64{}
	for p, st := range starts {
		end := blockEnd(starts, p, len(lines))
		strategy, ok := fieldSpan(lines, st.line, end, "参考方式")
		if !ok {
			continue
		}
		assets, ok := fieldSpan(lines, st.line, end, "参考资产")
		if !ok {
			continue
		}
		if fieldText(lines, strategy) == "text" {
			continue
		}
		var values []string
		for _, v := range strings.FieldsFunc(fieldText(lines, assets), func(r rune) bool { return r == ',' || r == '，' || r == '、' }) {
			if v = strings.TrimSpace(v); v != "" && v != "无" {
				values = append(values, v)
			}
		}
		if !slices.ContainsFunc(values, func(v string) bool { return !isAnchorID(v) }) {
			setField(lines, strategy, "text")
			setField(lines, assets, "无")
			corrected = append(corrected, st.shot)
		}
	}
	return strings.Join(lines, "\n"), corrected
}

// fieldSpan is the content lines [start, end) of the "### label" field
// within a shot block, if the field has any.
func fieldSpan(lines []string, blockStart, blockEnd int, label string) ([2]int, bool) {
	heading := "### " + label
	for i := blockStart; i < blockEnd; i++ {
		if strings.TrimSpace(lines[i]) != heading {
			continue
		}
		end := blockEnd
		for j := i + 1; j < blockEnd; j++ {
			if strings.HasPrefix(trimLeft(lines[j]), "### ") {
				end = j
				break
			}
		}
		return [2]int{i + 1, end}, i+1 < end
	}
	return [2]int{}, false
}

func fieldText(lines []string, span [2]int) string {
	var parts []string
	for _, line := range lines[span[0]:span[1]] {
		if line = strings.TrimSpace(line); line != "" && line != "---" {
			parts = append(parts, line)
		}
	}
	return strings.Join(parts, "、")
}

func setField(lines []string, span [2]int, value string) {
	clear(lines[span[0]:span[1]])
	lines[span[0]] = value
}

func isAnchorID(value string) bool {
	for _, prefix := range []string{"style:", "character:", "scene:", "prop:", "state:"} {
		if strings.HasPrefix(value, prefix) {
			return true
		}
	}
	return false
}

// ToolDef is a tool the orchestration agent may call: a sub-prompt run with
// the tool's own system prompt.
type ToolDef struct {
	Name        string `json:"name"`
	Description string `json:"description"`
	System      string `json:"system"`
}

// UnmarshalJSON requires all three fields, like the Rust struct did.
func (t *ToolDef) UnmarshalJSON(b []byte) error {
	var fields fieldSet
	if err := json.Unmarshal(b, &fields); err != nil {
		return err
	}
	if err := fields.require("name", "description", "system"); err != nil {
		return err
	}
	type plain ToolDef
	return json.Unmarshal(b, (*plain)(t))
}

const (
	maxAgentTools          = 32
	maxToolNameChars       = 100
	maxToolDescriptionChar = 2_000
)

// validateAgentTools bounds the tool list: 1–32 tools with unique non-empty
// names and non-empty descriptions and prompts.
func validateAgentTools(tools []ToolDef) error {
	if len(tools) == 0 || len(tools) > maxAgentTools {
		return fmt.Errorf("tools 数量必须在 1 到 %d 之间", maxAgentTools)
	}
	seen := map[string]bool{}
	for _, tool := range tools {
		name := strings.TrimSpace(tool.Name)
		switch {
		case name == "":
			return errors.New("tools.name 不能为空")
		case utf8.RuneCountInString(tool.Name) > maxToolNameChars:
			return fmt.Errorf("tools.name 超过 %d 个字符", maxToolNameChars)
		case seen[name]:
			return fmt.Errorf("tools.name 重复：%s", name)
		}
		seen[name] = true
		switch {
		case strings.TrimSpace(tool.Description) == "":
			return errors.New("tools.description 不能为空")
		case utf8.RuneCountInString(tool.Description) > maxToolDescriptionChar:
			return fmt.Errorf("tools.description 超过 %d 个字符", maxToolDescriptionChar)
		case strings.TrimSpace(tool.System) == "":
			return errors.New("tools.system 不能为空")
		case utf8.RuneCountInString(tool.System) > maxTextChars:
			return fmt.Errorf("tools.system 超过 %d 个字符", maxTextChars)
		}
	}
	return nil
}

// textLines splits like Rust's str::lines: "\n" or "\r\n" end a line and a
// final line ending adds no empty line.
func textLines(s string) []string {
	var lines []string
	for s != "" {
		line, rest, found := strings.Cut(s, "\n")
		if found {
			line = strings.TrimSuffix(line, "\r")
		}
		lines = append(lines, line)
		s = rest
	}
	return lines
}

func trimLeft(s string) string { return strings.TrimLeftFunc(s, unicode.IsSpace) }

func countOf(values []string, want string) int {
	n := 0
	for _, v := range values {
		if v == want {
			n++
		}
	}
	return n
}

// asciiLower lowercases ASCII letters only, so no other character can turn
// into a marker's letters.
func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if c >= 'A' && c <= 'Z' {
			b[i] = c + 'a' - 'A'
		}
	}
	return string(b)
}
