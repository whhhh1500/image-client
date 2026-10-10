package gen

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"imageclient/internal/config"
)

func tool(name string) AgentTool {
	return AgentTool{Name: name, Description: "步骤说明", System: "步骤 system"}
}

func TestValidatesAgentRequestAndTools(t *testing.T) {
	tools := []AgentTool{tool("writer"), tool("storyboard")}
	if err := (AgentRequest{System: "系统", User: "输入", Tools: tools}).validate(); err != nil {
		t.Fatal(err)
	}
	many := make([]AgentTool, 33)
	for i := range many {
		many[i] = tool(fmt.Sprintf("t%d", i))
	}
	for _, c := range []struct {
		tools []AgentTool
		want  string
	}{
		{nil, "tools 数量必须在 1 到 32 之间"},
		{many, "tools 数量必须在 1 到 32 之间"},
		{[]AgentTool{tool("  ")}, "tools.name 不能为空"},
		{[]AgentTool{tool("writer"), tool(" writer ")}, "tools.name 重复：writer"},
		{[]AgentTool{tool(strings.Repeat("a", 101))}, "tools.name 超过 100 个字符"},
		{[]AgentTool{{Name: "w", Description: "   ", System: "s"}}, "tools.description 不能为空"},
		{[]AgentTool{{Name: "w", Description: strings.Repeat("d", 2001), System: "s"}}, "tools.description 超过 2000 个字符"},
		{[]AgentTool{{Name: "w", Description: "d"}}, "tools.system 不能为空"},
	} {
		if err := ValidateAgentTools(c.tools); err == nil || err.Error() != c.want {
			t.Errorf("err = %v, want %s", err, c.want)
		}
	}
	if err := ValidateAgentTools([]AgentTool{tool(strings.Repeat("a", 100))}); err != nil {
		t.Fatal(err)
	}
	long := strings.Repeat("x", MaxAgentTextChars+1)
	for _, c := range []struct {
		req  AgentRequest
		want string
	}{
		{AgentRequest{System: long, User: "输入", Tools: tools}, "system 超过 2000000 个字符"},
		{AgentRequest{System: "系统", User: long, Tools: tools}, "input 超过 2000000 个字符"},
		{AgentRequest{System: "系统", User: "输入", Model: strings.Repeat("m", 201), Tools: tools}, "model 超过 200 个字符"},
	} {
		if err := c.req.validate(); err == nil || err.Error() != c.want {
			t.Errorf("err = %v, want %s", err, c.want)
		}
	}
	if err := (AgentRequest{System: "系统", User: "输入", Model: strings.Repeat("m", 200), Tools: tools}).validate(); err != nil {
		t.Fatal(err)
	}
}

// agentLLM plays both roles: with tools it orchestrates (scripted by
// orchestrate), without tools it runs a step and echoes its input.
type agentLLM struct {
	server      *httptest.Server
	mu          sync.Mutex
	steps       []map[string]any
	inFlight    atomic.Int32
	maxInFlight atomic.Int32
}

func startAgentLLM(t *testing.T, orchestrate func(step int, messages []any) string) *agentLLM {
	t.Helper()
	a := &agentLLM{}
	a.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body map[string]any
		json.NewDecoder(r.Body).Decode(&body)
		messages := body["messages"].([]any)
		if _, orchestrating := body["tools"]; orchestrating {
			a.mu.Lock()
			a.steps = append(a.steps, body)
			step := len(a.steps)
			a.mu.Unlock()
			w.Header().Set("Content-Type", "text/event-stream")
			fmt.Fprint(w, orchestrate(step, messages))
			return
		}
		n := a.inFlight.Add(1)
		for {
			highest := a.maxInFlight.Load()
			if n <= highest || a.maxInFlight.CompareAndSwap(highest, n) {
				break
			}
		}
		time.Sleep(20 * time.Millisecond)
		a.inFlight.Add(-1)
		input := messages[1].(map[string]any)["content"].(string)
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]any{"choices": []any{map[string]any{
			"message": map[string]any{"content": "结果:" + input}, "finish_reason": "stop"}}})
	}))
	t.Cleanup(a.server.Close)
	return a
}

func toolCallsFrame(calls ...[2]string) string {
	var items []string
	for i, c := range calls {
		args, _ := json.Marshal(map[string]string{"input": c[1]})
		items = append(items, fmt.Sprintf(`{"index":%d,"id":"call_%s_%d","function":{"name":%q,"arguments":%q}}`, i, c[0], i, c[0], string(args)))
	}
	return "data: {\"choices\":[{\"delta\":{\"tool_calls\":[" + strings.Join(items, ",") + "]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"
}

const finalFrame = "data: {\"choices\":[{\"delta\":{\"content\":\"<think>x</think>完成\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"

func agentEnv(t *testing.T, llm *agentLLM) *testEnv {
	return newTestService(t, func(s *config.Settings) {
		s.LLMAPIURL, s.LLMAPIKey, s.LLMModel = llm.server.URL, "k", "agent-model"
	})
}

func TestRunAgentChainsToolsAndKeepsTheModelsCallOrder(t *testing.T) {
	llm := startAgentLLM(t, func(step int, _ []any) string {
		if step == 1 {
			return toolCallsFrame([2]string{"writer", "故事"}, [2]string{"storyboard", "分镜"})
		}
		return finalFrame
	})
	env := agentEnv(t, llm)
	result, err := env.s.RunAgent(context.Background(), AgentRequest{System: "导演", User: "拍一部短片",
		Tools: []AgentTool{tool("writer"), tool("storyboard")}})
	if err != nil || result != "完成" {
		t.Fatalf("result = %q, err = %v", result, err)
	}
	first := llm.steps[0]
	tools := first["tools"].([]any)
	definition := tools[0].(map[string]any)["function"].(map[string]any)
	if first["model"] != "agent-model" || len(tools) != 2 || definition["name"] != "writer" ||
		!reflect.DeepEqual(definition["parameters"].(map[string]any)["required"], []any{"input"}) {
		t.Fatalf("first step = %v", first)
	}
	messages := llm.steps[1]["messages"].([]any)
	if len(messages) != 5 {
		t.Fatalf("messages = %v", messages)
	}
	assistant := messages[2].(map[string]any)
	if assistant["role"] != "assistant" || len(assistant["tool_calls"].([]any)) != 2 {
		t.Fatalf("assistant = %v", assistant)
	}
	for i, want := range []map[string]any{
		{"role": "tool", "tool_call_id": "call_writer_0", "content": "结果:故事"},
		{"role": "tool", "tool_call_id": "call_storyboard_1", "content": "结果:分镜"},
	} {
		if !reflect.DeepEqual(messages[3+i], any(want)) {
			t.Fatalf("tool message %d = %v", i, messages[3+i])
		}
	}
}

func TestRunAgentStopsAtTheToolCallBudget(t *testing.T) {
	llm := startAgentLLM(t, func(int, []any) string {
		calls := make([][2]string, 9)
		for i := range calls {
			calls[i] = [2]string{"writer", fmt.Sprint(i)}
		}
		return toolCallsFrame(calls...)
	})
	env := agentEnv(t, llm)
	result, err := env.s.RunAgent(context.Background(), AgentRequest{System: "s", User: "u", Tools: []AgentTool{tool("writer")}})
	want := "编排已停止：工具调用达到预算上限（每轮最多 8 次、全程最多 24 次），已执行 8 次，另有 1 次调用未执行。"
	if err != nil || result != want {
		t.Fatalf("result = %q, err = %v", result, err)
	}
	if peak := llm.maxInFlight.Load(); peak > maxConcurrentToolRuns || peak < 2 {
		t.Fatalf("tool calls ran %d at a time", peak)
	}
}

func TestRunAgentEndsAfterSixRounds(t *testing.T) {
	llm := startAgentLLM(t, func(int, []any) string { return toolCallsFrame([2]string{"writer", "x"}) })
	env := agentEnv(t, llm)
	if _, err := env.s.RunAgent(context.Background(), AgentRequest{System: "s", User: "u", Tools: []AgentTool{tool("writer")}}); err == nil || err.Error() != "编排超过步数上限" {
		t.Fatalf("err = %v", err)
	}
	if len(llm.steps) != 6 {
		t.Fatalf("%d rounds", len(llm.steps))
	}
}

func TestRunAgentRejectsUnknownToolsAndTruncatedAnswers(t *testing.T) {
	llm := startAgentLLM(t, func(int, []any) string { return toolCallsFrame([2]string{"ghost", "x"}) })
	env := agentEnv(t, llm)
	if _, err := env.s.RunAgent(context.Background(), AgentRequest{System: "s", User: "u", Tools: []AgentTool{tool("writer")}}); err == nil || err.Error() != "未知工具" {
		t.Fatalf("err = %v", err)
	}
	llm = startAgentLLM(t, func(int, []any) string {
		return "data: {\"choices\":[{\"delta\":{\"content\":\"半\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n"
	})
	env = agentEnv(t, llm)
	if _, err := env.s.RunAgent(context.Background(), AgentRequest{System: "s", User: "u", Tools: []AgentTool{tool("writer")}}); err == nil ||
		err.Error() != "文本服务返回不完整结果（可能被截断），未保存文档；请重试或缩短输入" {
		t.Fatalf("err = %v", err)
	}
	env = newTestService(t, nil)
	if _, err := env.s.RunAgent(context.Background(), AgentRequest{System: "s", User: "u", Tools: []AgentTool{tool("writer")}}); err == nil || err.Error() != "未配置文本 LLM" {
		t.Fatalf("err = %v", err)
	}
	if _, err := env.s.RunAgent(context.Background(), AgentRequest{System: "s", User: "u"}); err == nil || !strings.HasPrefix(err.Error(), "tools 数量") {
		t.Fatalf("validation comes first: %v", err)
	}
}
