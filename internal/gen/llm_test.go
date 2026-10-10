package gen

import (
	"context"
	"encoding/json"
	"errors"
	"io"
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

func decodeChunks(chunks ...string) (*sseDecoder, bool, error) {
	decoder := newSSEDecoder(time.Now())
	for _, chunk := range chunks {
		done, err := decoder.push([]byte(chunk))
		if err != nil || done {
			return decoder, done, err
		}
	}
	done, err := decoder.finish()
	return decoder, done, err
}

func bytewise(s string) []string {
	out := make([]string, len(s))
	for i := range len(s) {
		out[i] = s[i : i+1]
	}
	return out
}

func mustDecode(t *testing.T, s string) map[string]any {
	t.Helper()
	value, err := decodeJSON([]byte(s))
	if err != nil {
		t.Fatal(err)
	}
	return value.(map[string]any)
}

func TestAccumulatesStreamContentUsageAndToolCalls(t *testing.T) {
	decoder := newSSEDecoder(time.Now())
	for _, event := range []string{
		`{"choices":[{"delta":{"content":"你好"}}]}`,
		`{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"writer","arguments":"{\"input\":"}}]}}]}`,
		`{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"故事\"}"}},{"index":1,"function":{"name":"qc"}}]}}]}`,
		`{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}}`,
	} {
		if !decoder.apply(mustDecode(t, event)) {
			t.Fatalf("%s must be recognized", event)
		}
	}
	if decoder.acc.content.String() != "你好" || decoder.acc.firstTokenMs == nil {
		t.Fatal("content")
	}
	if n, _ := usageNumber(decoder.acc.usage, "total_tokens"); n != 14 {
		t.Fatal("usage")
	}
	completion := decoder.acc.completion("", false)
	want := []ToolCall{{ID: "call_1", Name: "writer", Arguments: `{"input":"故事"}`}, {ID: "call_1", Name: "qc"}}
	if !reflect.DeepEqual(completion.ToolCalls, want) {
		t.Fatalf("calls = %+v", completion.ToolCalls)
	}
	message := mustJSON(t, completion.AssistantMessage)
	if !strings.Contains(message, `"tool_calls":[{"function":{"arguments":"{\"input\":\"故事\"}","name":"writer"},"id":"call_1","type":"function"}`) {
		t.Fatalf("assistant message = %s", message)
	}
}

func TestToolCallsWithoutAnIDAreNumberedByIndex(t *testing.T) {
	decoder := newSSEDecoder(time.Now())
	decoder.apply(mustDecode(t, `{"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"name":"a"}},{"function":{"name":"b"}}]}}]}`))
	calls := decoder.acc.completion("", true).ToolCalls
	if len(calls) != 2 || calls[0] != (ToolCall{ID: "call_0", Name: "b"}) || calls[1] != (ToolCall{ID: "call_2", Name: "a"}) {
		t.Fatalf("calls = %+v", calls)
	}
}

func TestFinishReasonSpellingsOfOtherProvidersCountAsNormalStops(t *testing.T) {
	for _, reason := range []string{"stop", "STOP", "end_turn", "eos_token", "stop_sequence", " End "} {
		if finishReasonCategory(reason) != "stop" {
			t.Errorf("%q", reason)
		}
	}
	for reason, want := range map[string]string{"MAX_TOKENS": "length", "SAFETY": "content_filter", "": "none", "mystery": "other", "tool_use": "tool_calls"} {
		if got := finishReasonCategory(reason); got != want {
			t.Errorf("%q = %q, want %q", reason, got, want)
		}
	}
}

func TestTrailingUsageChunkDoesNotDemoteARecordedStop(t *testing.T) {
	decoder, done, err := decodeChunks("data: {\"choices\":[{\"delta\":{\"content\":\"完整\"},\"finish_reason\":\"stop\"}]}\n\n" +
		"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"\"}],\"usage\":{\"total_tokens\":3}}\n\n" +
		"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"mystery\"}]}\n\n" +
		"data: [DONE]\n\n")
	if err != nil || !done || decoder.acc.content.String() != "完整" || decoder.diag.FinishReason != "stop" {
		t.Fatalf("err = %v, done = %v, diag = %+v", err, done, decoder.diag)
	}
	if !normalCompletion(decoder.diag.FinishReason, decoder.diag.DoneReceived) {
		t.Fatal("must be a normal completion")
	}
}

func TestDecoderHandlesBOMCRLFSplitUTF8AndANamedCompletionEvent(t *testing.T) {
	frame := "\xef\xbb\xbfevent: completion\r\ndata: {\"choices\":[\r\ndata: {\"delta\":{\"content\":\"你好\"}}]}\r\n\r\ndata: [DONE]\r\n\r\n"
	decoder, done, err := decodeChunks(bytewise(frame)...)
	if err != nil || !done || decoder.acc.content.String() != "你好" || !decoder.diag.DoneReceived || decoder.diag.UnknownEvents < 1 {
		t.Fatalf("err = %v, done = %v, diag = %+v, content = %q", err, done, decoder.diag, decoder.acc.content.String())
	}
}

func TestCROnlyDoneDispatchesWithoutEOFAndIgnoresTheTail(t *testing.T) {
	decoder, done, err := decodeChunks("data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\r\rdata: [DONE]\r\rdata: {\"choices\":[{\"delta\":{\"content\":\"tail\"}}]}\r\r")
	if err != nil || !done || decoder.acc.content.String() != "first" || !decoder.diag.DoneReceived {
		t.Fatalf("err = %v, done = %v, content = %q", err, done, decoder.acc.content.String())
	}
}

func TestAcceptsALegacyFinalDataLineWithoutABlankLine(t *testing.T) {
	decoder, done, err := decodeChunks(`data: {"choices":[{"delta":{"content":"最后一段"}}]}`)
	if err != nil || done || decoder.acc.content.String() != "最后一段" {
		t.Fatalf("err = %v, done = %v, content = %q", err, done, decoder.acc.content.String())
	}
}

func TestErrorEventWinsOverPartialContentAndTheDoneMarker(t *testing.T) {
	for _, stream := range []string{
		"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\nevent: error\ndata: [DONE]\n\n",
		"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\nevent: error\n\n",
	} {
		if _, _, err := decodeChunks(stream); !errors.Is(err, sseApplicationError) {
			t.Fatalf("err = %v", err)
		}
	}
}

func TestInvalidStreamsFailClosed(t *testing.T) {
	if _, _, err := decodeChunks("data: {not-json}\n\n"); !errors.Is(err, sseInvalidJSON) {
		t.Fatalf("err = %v", err)
	}
	if _, _, err := decodeChunks("data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: \xff\n\n"); !errors.Is(err, sseInvalidUTF8) {
		t.Fatalf("err = %v", err)
	}
	if _, _, err := decodeChunks("data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: {\"error\":{\"message\":\"synthetic provider detail\"}}\n\n"); !errors.Is(err, sseApplicationError) {
		t.Fatalf("err = %v", err)
	}
	if sseApplicationError.message() != "文本流返回应用错误" || sseInvalidJSON.message() != "文本流响应协议错误" || sseInvalidUTF8.code() != "LLM_SSE_INVALID_UTF8" {
		t.Fatal("messages")
	}
}

func TestTracksReasoningAndFinishReasonWithoutEmittingReasoning(t *testing.T) {
	decoder, _, err := decodeChunks("data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"private\",\"content\":\"visible\"},\"finish_reason\":\"stop\"}]}\n\n")
	if err != nil || decoder.acc.content.String() != "visible" || !decoder.diag.ReasoningDeltaSeen || decoder.diag.FinishReason != "stop" {
		t.Fatalf("err = %v, diag = %+v", err, decoder.diag)
	}
}

func TestDiagnosticsAreFixedMetricsNotStreamPayloads(t *testing.T) {
	decoder := newSSEDecoder(time.Now())
	if _, err := decoder.push([]byte("data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"synthetic-secret-reasoning\"}}]}\n\n")); err != nil {
		t.Fatal(err)
	}
	if _, err := decoder.push([]byte("data: {\"error\":{\"message\":\"synthetic-secret-error\"}}\n\n")); !errors.Is(err, sseApplicationError) {
		t.Fatalf("err = %v", err)
	}
	serialized := mustJSON(t, decoder.diag)
	if !decoder.diag.ReasoningDeltaSeen || strings.Contains(serialized, "synthetic-secret") {
		t.Fatalf("diagnostics = %s", serialized)
	}
	want := []string{"bytes", "lines", "dataEvents", "invalidJsonEvents", "unknownEvents", "errorEvents", "doneReceived", "reasoningDeltaSeen", "finishReason"}
	var fields map[string]any
	json.Unmarshal([]byte(serialized), &fields)
	for _, key := range want {
		if _, ok := fields[key]; !ok {
			t.Fatalf("missing %s in %s", key, serialized)
		}
	}
}

func TestAUsageFrameDoesNotClassifyLaterUnknownObjectsAsRecognized(t *testing.T) {
	decoder, _, err := decodeChunks("data: {\"usage\":{\"total_tokens\":1}}\n\ndata: {\"unrelated\":true}\n\n")
	if err != nil || decoder.diag.UnknownEvents != 1 {
		t.Fatalf("err = %v, diag = %+v", err, decoder.diag)
	}
	if n, ok := usageNumber(decoder.acc.usage, "total_tokens"); !ok || n != 1 {
		t.Fatal("usage")
	}
}

func TestStopAndDoneAreNormalButExplicitTruncationWins(t *testing.T) {
	if !normalCompletion("stop", false) || !normalCompletion("", true) || normalCompletion("", false) {
		t.Fatal("stop / done")
	}
	for _, reason := range []string{"length", "content_filter", "tool_calls", "other"} {
		if normalCompletion(reason, true) {
			t.Errorf("%s must not count as complete", reason)
		}
	}
}

func TestEstimatesASCIIAndNonASCIITokensSeparately(t *testing.T) {
	if estimateTokens("abcdefgh") != 2 || estimateTokens("你好世界") != 4 || estimateTokens("abcd你好") != 3 || estimateTokens("a") != 1 {
		t.Fatal("estimate")
	}
}

func TestRemovesReasoningBlocksFromModelOutput(t *testing.T) {
	for in, want := range map[string]string{
		"<think>hidden</think>结果":                 "结果",
		"前文<think>unfinished":                     "前文",
		"A<THINK>x</THINK>B":                      "AB",
		"  a<Think>1</think>b<think>2</think>c  ": "abc",
		"İ<think>x</think>ok":                     "İok",
	} {
		if got := StripThinking(in); got != want {
			t.Errorf("StripThinking(%q) = %q, want %q", in, got, want)
		}
	}
}

// llmServer answers chat completions from a queue of responses.
type llmResponse struct {
	status      int
	contentType string
	body        string
	holdOpen    bool
}

type llmServer struct {
	server   *httptest.Server
	requests atomic.Int32
	mu       sync.Mutex
	bodies   []map[string]any
	queue    []llmResponse
	release  chan struct{}
}

func startLLM(t *testing.T, responses ...llmResponse) *llmServer {
	t.Helper()
	l := &llmServer{queue: responses, release: make(chan struct{})}
	l.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		l.requests.Add(1)
		var body map[string]any
		json.NewDecoder(r.Body).Decode(&body)
		l.mu.Lock()
		l.bodies = append(l.bodies, body)
		answer := l.queue[0]
		if len(l.queue) > 1 {
			l.queue = l.queue[1:]
		}
		l.mu.Unlock()
		w.Header().Set("Content-Type", answer.contentType)
		w.WriteHeader(answer.status)
		io.WriteString(w, answer.body)
		w.(http.Flusher).Flush()
		if answer.holdOpen {
			select {
			case <-l.release:
			case <-r.Context().Done():
			}
		}
	}))
	t.Cleanup(l.server.Close)
	t.Cleanup(func() { close(l.release) })
	return l
}

func (l *llmServer) body(i int) map[string]any {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.bodies[i]
}

func chat(t *testing.T, env *testEnv, url string) (Completion, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	return env.s.Chat(ctx, ChatRequest{
		Endpoint: Endpoint{URL: url, Key: "local-test-key"}, Model: "local-test-model",
		Messages: []Message{{"role": "user", "content": "test"}}, Operation: "test",
	})
}

const sse = "text/event-stream"

func TestPartialFinishPreservesTheReturnedText(t *testing.T) {
	for _, c := range []struct {
		label, contentType, body, reason string
	}{
		{"stream_length", sse, "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n", "length"},
		{"stream_eof", sse, "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n", ""},
		{"json_length", "application/json", `{"choices":[{"message":{"content":"partial"},"finish_reason":"length"}]}`, "length"},
	} {
		server := startLLM(t, llmResponse{status: 200, contentType: c.contentType, body: c.body})
		env := newTestService(t, nil)
		result, err := chat(t, env, server.server.URL)
		if err != nil {
			t.Fatalf("%s: %v", c.label, err)
		}
		if result.Content != "partial" || result.Completed || result.FinishReason != c.reason || server.requests.Load() != 1 {
			t.Fatalf("%s: %+v", c.label, result)
		}
	}
}

func TestDoneStopsTheNetworkReadBeforeTheServerCloses(t *testing.T) {
	server := startLLM(t, llmResponse{status: 200, contentType: sse, holdOpen: true,
		body: "data: {\"choices\":[{\"delta\":{\"content\":\"test\"}}]}\r\rdata: [DONE]\r\r"})
	env := newTestService(t, nil)
	started := time.Now()
	result, err := chat(t, env, server.server.URL)
	if err != nil || result.Content != "test" || !result.Completed || server.requests.Load() != 1 {
		t.Fatalf("result = %+v, err = %v", result, err)
	}
	if time.Since(started) > time.Second {
		t.Fatal("DONE must return before the server closes")
	}
	body := server.body(0)
	if body["stream"] != true || !reflect.DeepEqual(body["stream_options"], map[string]any{"include_usage": true}) || body["model"] != "local-test-model" {
		t.Fatalf("request = %v", body)
	}
	if _, ok := body["tools"]; ok {
		t.Fatal("no tools were given")
	}
}

func TestHTTP200PartialThenErrorFailsWithoutASecondRequest(t *testing.T) {
	server := startLLM(t, llmResponse{status: 200, contentType: sse, holdOpen: true,
		body: "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\nevent: error\n\n"})
	env := newTestService(t, nil)
	_, err := chat(t, env, server.server.URL)
	if err == nil || err.Error() != "文本流返回应用错误" || server.requests.Load() != 1 {
		t.Fatalf("err = %v", err)
	}
}

func TestARefusedStreamGetsOneNonStreamingRetry(t *testing.T) {
	for _, status := range []int{400, 422} {
		server := startLLM(t,
			llmResponse{status: status, contentType: "application/json", body: `{"error":{}}`},
			llmResponse{status: 200, contentType: "application/json", body: `{"choices":[{"message":{"content":"fallback-ok"}}]}`})
		env := newTestService(t, nil)
		result, err := chat(t, env, server.server.URL)
		if err != nil || result.Content != "fallback-ok" || !result.Completed || server.requests.Load() != 2 {
			t.Fatalf("%d: result = %+v, err = %v", status, result, err)
		}
		retry := server.body(1)
		if _, ok := retry["stream"]; ok {
			t.Fatal("the retry must not ask for a stream")
		}
		if _, ok := retry["stream_options"]; ok {
			t.Fatal("the retry must not send stream options")
		}
	}
}

func TestOrdinaryJSONAndErrorEnvelopesStaySupported(t *testing.T) {
	server := startLLM(t, llmResponse{status: 200, contentType: "application/json",
		body: `{"choices":[{"message":{"content":"json-test","tool_calls":[{"id":"c","function":{"name":"n"}}]}}],"usage":{"prompt_tokens":3}}`})
	env := newTestService(t, nil)
	result, err := chat(t, env, server.server.URL)
	if err != nil || result.Content != "json-test" || !result.Completed || result.AssistantMessage["content"] != "json-test" {
		t.Fatalf("result = %+v, err = %v", result, err)
	}
	if !reflect.DeepEqual(result.ToolCalls, []ToolCall{{ID: "c", Name: "n", Arguments: "{}"}}) {
		t.Fatalf("calls = %+v", result.ToolCalls)
	}
	for body, want := range map[string]string{
		`{"error":{"message":"synthetic error"}}`: "文本接口返回应用错误",
		`{"choices":[]}`: "文本接口未返回内容",
		`{"choices"`:     "解析文本响应失败：",
	} {
		server := startLLM(t, llmResponse{status: 200, contentType: "application/json", body: body})
		if _, err := chat(t, env, server.server.URL); err == nil || !strings.HasPrefix(err.Error(), want) {
			t.Errorf("%s: err = %v", body, err)
		}
	}
}

func TestHTTPErrorsQuoteTheStartOfTheBody(t *testing.T) {
	server := startLLM(t, llmResponse{status: 401, contentType: "application/json", body: strings.Repeat("x", 500)})
	env := newTestService(t, nil)
	_, err := chat(t, env, server.server.URL)
	if err == nil || err.Error() != "文本接口返回 401 Unauthorized: "+strings.Repeat("x", 400) {
		t.Fatalf("err = %v", err)
	}
}

func TestAnEmptyStreamIsAnError(t *testing.T) {
	server := startLLM(t, llmResponse{status: 200, contentType: sse, body: "data: [DONE]\n\n"})
	env := newTestService(t, nil)
	if _, err := chat(t, env, server.server.URL); err == nil || err.Error() != "文本接口未返回内容" {
		t.Fatalf("err = %v", err)
	}
}

func TestSilenceEndsAStream(t *testing.T) {
	server := startLLM(t, llmResponse{status: 200, contentType: sse, holdOpen: true,
		body: "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n"})
	env := newTestService(t, nil)
	env.s.llmTimeouts.Idle = 100 * time.Millisecond
	_, err := chat(t, env, server.server.URL)
	if err == nil || !strings.HasPrefix(err.Error(), "读取文本流失败：超过 ") || !strings.HasSuffix(err.Error(), "秒没有收到新数据") {
		t.Fatalf("err = %v", err)
	}
}

func TestTheWaitForLLMHeadersIsBounded(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		io.ReadAll(r.Body) // lets the server notice the client going away
		select {
		case <-time.After(3 * time.Second):
		case <-r.Context().Done():
		}
	}))
	defer server.Close()
	env := newTestService(t, nil)
	env.s.llmTimeouts.Header = 100 * time.Millisecond
	_, err := chat(t, env, server.URL)
	if err == nil || !strings.HasPrefix(err.Error(), "请求文本接口失败：等待响应超过 ") {
		t.Fatalf("err = %v", err)
	}
}

func TestCompleteTextRequiresContentAndLLMChatAFinishedReply(t *testing.T) {
	server := startLLM(t, llmResponse{status: 200, contentType: "application/json",
		body: `{"choices":[{"message":{"content":"<think>plan</think> 答案 "},"finish_reason":"stop"}]}`})
	env := newTestService(t, func(s *config.Settings) {
		s.LLMAPIURL, s.LLMAPIKey, s.LLMModel = server.server.URL+"/v1/", "k", "configured-model"
	})
	got, err := env.s.LLMChat(context.Background(), "系统", "用户", "")
	if err != nil || got != "答案" {
		t.Fatalf("got %q, %v", got, err)
	}
	body := server.body(0)
	messages := body["messages"].([]any)
	if body["model"] != "configured-model" || len(messages) != 2 || messages[0].(map[string]any)["role"] != "system" {
		t.Fatalf("body = %v", body)
	}

	truncated := startLLM(t, llmResponse{status: 200, contentType: "application/json",
		body: `{"choices":[{"message":{"content":"半"},"finish_reason":"length"}]}`})
	env = newTestService(t, func(s *config.Settings) { s.LLMAPIURL, s.LLMAPIKey = truncated.server.URL, "k" })
	if _, err := env.s.LLMChat(context.Background(), "s", "u", "m"); err == nil ||
		err.Error() != "文本服务返回不完整结果（可能被截断），未使用该结果；请重试或缩短输入" {
		t.Fatalf("err = %v", err)
	}
	text, err := env.s.CompleteText(context.Background(), TextRequest{Endpoint: Endpoint{URL: truncated.server.URL}, Model: "m", Operation: "t"})
	if err != nil || text != "半" {
		t.Fatalf("CompleteText keeps partial text: %q, %v", text, err)
	}

	env = newTestService(t, nil)
	if _, err := env.s.LLMChat(context.Background(), "s", "u", ""); err == nil || err.Error() != "未配置文本 LLM（请在设置中填写 LLM 地址与 Key）" {
		t.Fatalf("err = %v", err)
	}
	if ChatEndpoint("https://x.example/v1/") != "https://x.example/v1/chat/completions" {
		t.Fatal(ChatEndpoint("https://x.example/v1/"))
	}
}
