package gen

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"unicode/utf8"
)

// Agent input limits, shared with the REST API.
const (
	MaxAgentTextChars  = 2_000_000
	MaxAgentModelChars = 200
	MaxAgentTools      = 32

	maxToolNameChars        = 100
	maxToolDescriptionChars = 2_000

	maxAgentSteps         = 6
	maxToolCallsPerStep   = 8
	maxToolCallsTotal     = 24
	maxConcurrentToolRuns = 3 // keeps one step from saturating provider rate limits
)

// AgentTool is a step the orchestrating model may call; System is the step's
// own system prompt.
type AgentTool struct {
	Name        string `json:"name"`
	Description string `json:"description"`
	System      string `json:"system"`
}

// AgentRequest is one orchestration. An empty Model uses the configured one.
type AgentRequest struct {
	System string
	User   string
	Model  string
	Tools  []AgentTool
}

// ValidateAgentTools requires 1..32 tools with unique non-empty names and
// bounded descriptions and prompts.
func ValidateAgentTools(tools []AgentTool) error {
	if len(tools) == 0 || len(tools) > MaxAgentTools {
		return fmt.Errorf("tools 数量必须在 1 到 %d 之间", MaxAgentTools)
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
			return fmt.Errorf("tools.name 重复: %s", name)
		case strings.TrimSpace(tool.Description) == "":
			return errors.New("tools.description 不能为空")
		case utf8.RuneCountInString(tool.Description) > maxToolDescriptionChars:
			return fmt.Errorf("tools.description 超过 %d 个字符", maxToolDescriptionChars)
		case strings.TrimSpace(tool.System) == "":
			return errors.New("tools.system 不能为空")
		case utf8.RuneCountInString(tool.System) > MaxAgentTextChars:
			return fmt.Errorf("tools.system 超过 %d 个字符", MaxAgentTextChars)
		}
		seen[name] = true
	}
	return nil
}

func (r AgentRequest) validate() error {
	if utf8.RuneCountInString(r.System) > MaxAgentTextChars {
		return fmt.Errorf("system 超过 %d 个字符", MaxAgentTextChars)
	}
	if utf8.RuneCountInString(r.User) > MaxAgentTextChars {
		return fmt.Errorf("input 超过 %d 个字符", MaxAgentTextChars)
	}
	if utf8.RuneCountInString(r.Model) > MaxAgentModelChars {
		return fmt.Errorf("model 超过 %d 个字符", MaxAgentModelChars)
	}
	return ValidateAgentTools(r.Tools)
}

// LLMChat is one system + user completion with the configured text service
// (empty model: the configured one). Truncated replies are refused and
// reasoning blocks removed.
func (s *Service) LLMChat(ctx context.Context, system, user, model string) (string, error) {
	cfg := s.cfg.Get()
	endpoint, ok := LLMEndpoint(cfg)
	if !ok {
		return "", errors.New("未配置文本 LLM（请在设置中填写 LLM 地址与 Key）")
	}
	if model == "" {
		model = cfg.LLMModel
	}
	return s.finishedText(ctx, TextRequest{Endpoint: endpoint, Model: model, System: system, User: user, Operation: "text_completion"})
}

// RunAgent lets the model chain the given tools (script, storyboard,
// consistency, QC, ...) for up to six rounds. Each tool call is a separate
// completion with the tool's system prompt; calls beyond the budget stop the
// orchestration with a summary instead of running.
func (s *Service) RunAgent(ctx context.Context, req AgentRequest) (string, error) {
	if err := req.validate(); err != nil {
		return "", err
	}
	cfg := s.cfg.Get()
	endpoint, ok := LLMEndpoint(cfg)
	if !ok {
		return "", errors.New("未配置文本 LLM")
	}
	model := req.Model
	if model == "" {
		model = cfg.LLMModel
	}
	definitions := make([]map[string]any, len(req.Tools))
	for i, tool := range req.Tools {
		definitions[i] = map[string]any{
			"type": "function",
			"function": map[string]any{
				"name":        tool.Name,
				"description": tool.Description,
				"parameters": map[string]any{
					"type": "object",
					"properties": map[string]any{
						"input": map[string]any{"type": "string", "description": "该步骤的任务输入内容"},
					},
					"required": []string{"input"},
				},
			},
		}
	}
	messages := []Message{{"role": "system", "content": req.System}, {"role": "user", "content": req.User}}

	executedTotal := 0
	for step := 1; step <= maxAgentSteps; step++ {
		completion, err := s.Chat(ctx, ChatRequest{
			Endpoint: endpoint, Model: model, Messages: messages, Tools: definitions,
			Operation: fmt.Sprintf("agent_orchestration_step_%d", step),
		})
		if err != nil {
			return "", err
		}
		if len(completion.ToolCalls) == 0 {
			if !completion.Completed {
				return "", errors.New("文本服务返回不完整结果（可能被截断），未保存文档；请重试或缩短输入")
			}
			return StripThinking(completion.Content), nil
		}
		messages = append(messages, completion.AssistantMessage)

		// Resolve every call first, so the budget and the message order do
		// not depend on which tool finishes first.
		type job struct {
			index int
			tool  AgentTool
			input string
		}
		var jobs []job
		skipped := make([]bool, len(completion.ToolCalls))
		executedStep, skippedByBudget := 0, 0
		for i, call := range completion.ToolCalls {
			if executedStep >= maxToolCallsPerStep || executedTotal >= maxToolCallsTotal {
				skipped[i] = true
				skippedByBudget++
				continue
			}
			executedStep++
			executedTotal++
			var args struct {
				Input any `json:"input"`
			}
			json.Unmarshal([]byte(call.Arguments), &args)
			input, _ := args.Input.(string)
			found := false
			for _, tool := range req.Tools {
				if tool.Name == call.Name {
					jobs = append(jobs, job{index: i, tool: tool, input: input})
					found = true
					break
				}
			}
			if !found {
				return "", errors.New("未知工具")
			}
		}

		results := make([]string, len(completion.ToolCalls))
		var wg sync.WaitGroup
		slots := make(chan struct{}, maxConcurrentToolRuns)
		for _, j := range jobs {
			wg.Add(1)
			go func() {
				defer wg.Done()
				slots <- struct{}{}
				defer func() { <-slots }()
				text, err := s.finishedText(ctx, TextRequest{
					Endpoint: endpoint, Model: model, System: j.tool.System, User: j.input,
					Operation: "agent_tool_" + j.tool.Name,
				})
				if err != nil {
					text = err.Error()
				}
				results[j.index] = text
			}()
		}
		wg.Wait()

		for i, call := range completion.ToolCalls {
			content := results[i]
			if skipped[i] {
				content = "工具调用预算已用尽，本次调用未执行。"
			}
			messages = append(messages, Message{"role": "tool", "tool_call_id": call.ID, "content": content})
		}
		if skippedByBudget > 0 {
			return fmt.Sprintf("编排已停止：工具调用达到预算上限（每轮最多 %d 次、全程最多 %d 次），已执行 %d 次，另有 %d 次调用未执行。",
				maxToolCallsPerStep, maxToolCallsTotal, executedTotal, skippedByBudget), nil
		}
	}
	return "", errors.New("编排超过步数上限")
}

// StripThinking removes <think>...</think> blocks (any case) that reasoning
// models inline, including an unterminated trailing one, and trims the rest.
func StripThinking(text string) string {
	var out strings.Builder
	remaining := text
	for {
		start := strings.Index(asciiLower(remaining), "<think>")
		if start < 0 {
			out.WriteString(remaining)
			break
		}
		out.WriteString(remaining[:start])
		tail := remaining[start+len("<think>"):]
		end := strings.Index(asciiLower(tail), "</think>")
		if end < 0 {
			break
		}
		remaining = tail[end+len("</think>"):]
	}
	return strings.TrimSpace(out.String())
}
