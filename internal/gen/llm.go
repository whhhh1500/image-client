package gen

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/google/uuid"

	"imageclient/internal/config"
	"imageclient/internal/platform"
)

const maxLLMResponseBytes = 100 << 20

// A streamed reply is bounded by silence (Header, Idle) rather than length,
// so a long document is not cut off while tokens still arrive; Total is the
// ceiling for the whole request.
var defaultLLMTimeouts = platform.Timeouts{Header: 300 * time.Second, Idle: 300 * time.Second, Total: 30 * time.Minute}

// Endpoint is a chat completions URL and its key.
type Endpoint struct {
	URL string
	Key string
}

// LLMEndpoint is the configured chat completions endpoint; false when the
// text service is not configured.
func LLMEndpoint(cfg config.Settings) (Endpoint, bool) {
	if !cfg.LLMReady() {
		return Endpoint{}, false
	}
	return Endpoint{URL: ChatEndpoint(cfg.LLMAPIURL), Key: cfg.LLMAPIKey}, true
}

// ChatEndpoint derives ".../chat/completions" from a configured base URL.
func ChatEndpoint(baseURL string) string {
	return strings.TrimRight(baseURL, "/") + "/chat/completions"
}

// Message is one chat message, e.g. {"role": "user", "content": "..."};
// content may also be a list of multimodal parts.
type Message map[string]any

// ToolCall is a function call the model asked for.
type ToolCall struct {
	ID        string
	Name      string
	Arguments string // JSON text
}

// Completion is a model reply.
type Completion struct {
	Content   string
	ToolCalls []ToolCall
	// AssistantMessage is the reply as a message, to continue the conversation.
	AssistantMessage Message
	// FinishReason is the normalized reason: stop, length, tool_calls,
	// content_filter, other, or "" when the provider sent none.
	FinishReason string
	// Completed reports a normal end. A reply cut off by a length or filter
	// stop keeps its text, but document-producing callers must not save it.
	Completed bool
}

// ChatRequest is one chat completion.
type ChatRequest struct {
	Endpoint  Endpoint
	Model     string
	Messages  []Message
	Tools     []map[string]any // function definitions; nil sends none
	Operation string           // names the call in logs
}

// TextRequest is a single system + user prompt.
type TextRequest struct {
	Endpoint  Endpoint
	Model     string
	System    string
	User      string
	Operation string
}

func (r TextRequest) chat() ChatRequest {
	return ChatRequest{Endpoint: r.Endpoint, Model: r.Model, Operation: r.Operation, Messages: []Message{
		{"role": "system", "content": r.System},
		{"role": "user", "content": r.User},
	}}
}

// CompleteText returns the reply text, partial or not.
func (s *Service) CompleteText(ctx context.Context, r TextRequest) (string, error) {
	c, err := s.CompleteTextResult(ctx, r)
	return c.Content, err
}

// CompleteTextResult returns the reply with its completion metadata; a reply
// without text is an error.
func (s *Service) CompleteTextResult(ctx context.Context, r TextRequest) (Completion, error) {
	c, err := s.Chat(ctx, r.chat())
	if err != nil {
		return Completion{}, err
	}
	if c.Content == "" {
		return Completion{}, errors.New("文本接口未返回内容")
	}
	return c, nil
}

// finishedText requires a normally finished reply and strips reasoning
// blocks: a truncated answer must never pass for a complete one.
func (s *Service) finishedText(ctx context.Context, r TextRequest) (string, error) {
	c, err := s.CompleteTextResult(ctx, r)
	if err != nil {
		return "", err
	}
	if !c.Completed {
		return "", errors.New("文本服务返回不完整结果（可能被截断），未使用该结果；请重试或缩短输入")
	}
	return StripThinking(c.Content), nil
}

// Chat streams a chat completion. A gateway that refuses streaming (400 or
// 422) gets one non-streaming retry.
func (s *Service) Chat(ctx context.Context, r ChatRequest) (Completion, error) {
	c := &chatCall{s: s, r: r, requestID: uuid.NewString(), started: time.Now()}
	for _, m := range r.Messages {
		if text, ok := m["content"].(string); ok {
			c.inputChars += utf8.RuneCountInString(text)
			c.estimatedInput += estimateTokens(text)
		}
	}
	slog.Info("llm.request.start", "requestId", c.requestID, "operation", r.Operation, "model", r.Model,
		"endpoint", platform.SafeURL(r.Endpoint.URL), "messageCount", len(r.Messages), "inputChars", c.inputChars,
		"toolCount", len(r.Tools), "streamRequested", true)

	body := map[string]any{
		"model": r.Model, "messages": r.Messages, "stream": true,
		"stream_options": map[string]any{"include_usage": true},
	}
	if r.Tools != nil {
		body["tools"] = r.Tools
	}
	resp, err := c.post(ctx, body, s.llmTimeouts)
	if err != nil {
		c.fail(nil, err.Error())
		return Completion{}, fmt.Errorf("请求文本接口失败：%v", err)
	}
	c.headerMs = time.Since(c.started).Milliseconds()
	status := resp.StatusCode
	if !isSuccess(resp) {
		text := readErrorText(resp)
		if status == 400 || status == 422 {
			slog.Warn("llm.stream.fallback", "requestId", c.requestID, "operation", r.Operation, "model", r.Model,
				"status", status, "reason", platform.ErrorText(text))
			delete(body, "stream")
			delete(body, "stream_options")
			return c.nonStream(ctx, body)
		}
		c.fail(status, text)
		return Completion{}, fmt.Errorf("文本接口返回 %s: %s", statusLine(status), providerErrorText(text))
	}
	defer resp.Body.Close()

	if !strings.Contains(strings.ToLower(resp.Header.Get("Content-Type")), "text/event-stream") {
		raw, err := readBody(resp, maxLLMResponseBytes, "文本接口响应", nil)
		if err != nil {
			c.fail(status, err.Error())
			return Completion{}, fmt.Errorf("读取文本响应失败：%v", err)
		}
		value, err := decodeJSON(raw)
		if err != nil {
			c.fail(status, err.Error())
			return Completion{}, fmt.Errorf("解析文本响应失败：%v", err)
		}
		return c.finishNonStream(value)
	}
	return c.stream(resp)
}

type chatCall struct {
	s              *Service
	r              ChatRequest
	requestID      string
	started        time.Time
	headerMs       int64
	inputChars     int
	estimatedInput int
}

func (c *chatCall) post(ctx context.Context, body map[string]any, timeouts platform.Timeouts) (*http.Response, error) {
	payload, err := encodeJSON(body)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequest(http.MethodPost, c.r.Endpoint.URL, bytes.NewReader(payload))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+c.r.Endpoint.Key)
	req.Header.Set("Content-Type", "application/json")
	return platform.Do(ctx, c.s.clientFor(c.r.Endpoint.URL), req, timeouts)
}

// nonStream is the compatibility retry. The whole body arrives at once, so it
// gets the full ceiling instead of the streaming timeouts.
func (c *chatCall) nonStream(ctx context.Context, body map[string]any) (Completion, error) {
	resp, err := c.post(ctx, body, platform.Timeouts{Total: c.s.llmTimeouts.Total})
	if err != nil {
		c.fail(nil, err.Error())
		return Completion{}, fmt.Errorf("请求文本接口失败：%v", err)
	}
	c.headerMs = time.Since(c.started).Milliseconds()
	if !isSuccess(resp) {
		text := readErrorText(resp)
		c.fail(resp.StatusCode, text)
		return Completion{}, fmt.Errorf("文本接口返回 %s: %s", statusLine(resp.StatusCode), providerErrorText(text))
	}
	raw, err := readBody(resp, maxLLMResponseBytes, "文本接口响应", nil)
	if err != nil {
		return Completion{}, err
	}
	value, err := decodeJSON(raw)
	if err != nil {
		return Completion{}, fmt.Errorf("解析文本响应失败：%v", err)
	}
	return c.finishNonStream(value)
}

func (c *chatCall) finishNonStream(value any) (Completion, error) {
	object, _ := value.(map[string]any)
	if e, ok := object["error"]; ok && e != nil {
		c.fail(200, "LLM_HTTP200_APPLICATION_ERROR")
		return Completion{}, errors.New("文本接口返回应用错误")
	}
	var choice map[string]any
	if choices, _ := object["choices"].([]any); len(choices) > 0 {
		choice, _ = choices[0].(map[string]any)
	}
	message, _ := choice["message"].(map[string]any)
	content, _ := message["content"].(string)
	reason := ""
	if text, ok := choice["finish_reason"].(string); ok {
		if reason = finishReasonCategory(text); reason == "none" {
			reason = ""
		}
	}
	// A fully parsed body is the whole reply: a missing finish_reason is
	// tolerated, an explicit length/filter/tool stop is not a finished text.
	completion := Completion{
		Content: content, ToolCalls: parseToolCalls(message["tool_calls"]), AssistantMessage: message,
		FinishReason: reason, Completed: normalCompletion(reason, true),
	}
	if completion.Content == "" && len(completion.ToolCalls) == 0 {
		c.fail(200, "非流式响应没有内容")
		return Completion{}, errors.New("文本接口未返回内容")
	}
	c.succeed(completion.Content, object["usage"], nil, "non_stream")
	return completion, nil
}

func (c *chatCall) stream(resp *http.Response) (Completion, error) {
	status := resp.StatusCode
	decoder := newSSEDecoder(c.started)
	protocolFailure := func(err error) error {
		var sse sseError
		errors.As(err, &sse)
		c.diagnostics(status, decoder, sse.code())
		c.fail(status, sse.code())
		return errors.New(sse.message())
	}
	chunk := make([]byte, 32<<10)
	done := false
	for !done {
		n, err := resp.Body.Read(chunk)
		if n > 0 {
			if decoder.diag.Bytes+n > maxLLMResponseBytes {
				c.fail(status, "流式响应超过 100 MiB 大小限制")
				return Completion{}, errors.New("文本流响应超过 100 MiB 大小限制")
			}
			var perr error
			if done, perr = decoder.push(chunk[:n]); perr != nil {
				return Completion{}, protocolFailure(perr)
			}
		}
		if done || err == io.EOF {
			break
		}
		if err != nil {
			c.fail(status, err.Error())
			return Completion{}, fmt.Errorf("读取文本流失败：%v", err)
		}
	}
	if !done {
		if _, err := decoder.finish(); err != nil {
			return Completion{}, protocolFailure(err)
		}
	}

	reason := decoder.diag.FinishReason
	if reason == "none" {
		reason = ""
	}
	completion := decoder.acc.completion(reason, normalCompletion(reason, decoder.diag.DoneReceived))
	if completion.Content == "" && len(completion.ToolCalls) == 0 {
		c.diagnostics(status, decoder, "LLM_SSE_NO_CONTENT")
		c.fail(status, "流式响应没有内容")
		return Completion{}, errors.New("文本接口未返回内容")
	}
	c.diagnostics(status, decoder, "LLM_SSE_COMPLETE")
	c.succeed(completion.Content, decoder.acc.usage, decoder.acc.firstTokenMs, "stream")
	return completion, nil
}

func (c *chatCall) succeed(output string, usage any, firstTokenMs *int64, mode string) {
	providerInput, hasInput := usageNumber(usage, "prompt_tokens", "input_tokens")
	providerOutput, hasOutput := usageNumber(usage, "completion_tokens", "output_tokens")
	providerTotal, hasTotal := usageNumber(usage, "total_tokens")
	input, outputTokens := c.estimatedInput, estimateTokens(output)
	if hasInput {
		input = providerInput
	}
	if hasOutput {
		outputTokens = providerOutput
	}
	total := input + outputTokens
	if hasTotal {
		total = providerTotal
	}
	source := "estimate_ascii4_nonascii1"
	if hasInput || hasOutput {
		source = "provider"
	}
	var firstToken any
	if firstTokenMs != nil {
		firstToken = *firstTokenMs
	}
	slog.Info("llm.request.end", "requestId", c.requestID, "operation", c.r.Operation, "model", c.r.Model,
		"status", "success", "responseMode", mode, "durationMs", time.Since(c.started).Milliseconds(),
		"responseHeaderMs", c.headerMs, "firstTokenMs", firstToken, "inputChars", c.inputChars,
		"outputChars", utf8.RuneCountInString(output), "inputTokens", input, "outputTokens", outputTokens,
		"totalTokens", total, "tokenUsageSource", source)
}

// fail logs a failed request; status is an HTTP status or nil.
func (c *chatCall) fail(status any, detail string) {
	slog.Error("llm.request.end", "requestId", c.requestID, "operation", c.r.Operation, "model", c.r.Model,
		"status", "error", "httpStatus", status, "durationMs", time.Since(c.started).Milliseconds(),
		"error", platform.ErrorText(detail))
}

// diagnostics logs fixed stream metrics, never stream payloads.
func (c *chatCall) diagnostics(status int, decoder *sseDecoder, outcome string) {
	slog.Info("llm.stream.diagnostics", "requestId", c.requestID, "operation", c.r.Operation, "model", c.r.Model,
		"httpStatus", status, "outcome", outcome, "metrics", decoder.diag)
}

// finishReasonCategory normalizes providers' spellings of the same outcomes
// (Gemini upper-cases them, TGI says eos_token, Anthropic-style gateways
// end_turn).
func finishReasonCategory(value string) string {
	switch asciiLower(strings.TrimSpace(value)) {
	case "":
		return "none"
	case "stop", "end_turn", "eos", "eos_token", "stop_sequence", "end":
		return "stop"
	case "length", "max_tokens", "max_output_tokens", "model_length":
		return "length"
	case "tool_calls", "function_call", "tool_use":
		return "tool_calls"
	case "content_filter", "safety", "recitation", "blocked", "prohibited_content":
		return "content_filter"
	}
	return "other"
}

// normalCompletion: an explicit stop finishes a reply; without a reason, the
// [DONE] marker (or a complete non-streamed body) does.
func normalCompletion(reason string, doneReceived bool) bool {
	switch reason {
	case "stop":
		return true
	case "":
		return doneReceived
	}
	return false
}

func parseToolCalls(value any) []ToolCall {
	items, _ := value.([]any)
	calls := make([]ToolCall, 0, len(items))
	for _, item := range items {
		call, _ := item.(map[string]any)
		function, _ := call["function"].(map[string]any)
		id, _ := call["id"].(string)
		name, _ := function["name"].(string)
		arguments, ok := function["arguments"].(string)
		if !ok {
			arguments = "{}"
		}
		calls = append(calls, ToolCall{ID: id, Name: name, Arguments: arguments})
	}
	return calls
}

func usageNumber(usage any, keys ...string) (int, bool) {
	object, _ := usage.(map[string]any)
	for _, key := range keys {
		if n, ok := jsonUint(object[key]); ok {
			return int(n), true
		}
	}
	return 0, false
}

// jsonUint reads a non-negative integer decoded with UseNumber.
func jsonUint(value any) (uint64, bool) {
	number, ok := value.(json.Number)
	if !ok {
		return 0, false
	}
	n, err := strconv.ParseUint(number.String(), 10, 64)
	return n, err == nil
}

// estimateTokens counts four ASCII characters or one other character as a
// token.
func estimateTokens(text string) int {
	ascii, other := 0, 0
	for _, r := range text {
		if r < utf8.RuneSelf {
			ascii++
		} else {
			other++
		}
	}
	return (ascii+3)/4 + other
}

// decodeJSON decodes exactly one JSON value, keeping numbers exact.
func decodeJSON(data []byte) (any, error) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		return nil, err
	}
	if _, err := decoder.Token(); err != io.EOF {
		return nil, errors.New("trailing data after JSON value")
	}
	return value, nil
}

func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if c >= 'A' && c <= 'Z' {
			b[i] = c + 'a' - 'A'
		}
	}
	return string(b)
}

// sseError is a stream that violates the protocol or reports an error.
type sseError int

const (
	sseInvalidUTF8 sseError = iota + 1
	sseInvalidJSON
	sseApplicationError
)

func (e sseError) Error() string { return e.code() }

func (e sseError) code() string {
	switch e {
	case sseInvalidUTF8:
		return "LLM_SSE_INVALID_UTF8"
	case sseInvalidJSON:
		return "LLM_SSE_INVALID_JSON_EVENT"
	}
	return "LLM_SSE_APPLICATION_ERROR"
}

func (e sseError) message() string {
	if e == sseApplicationError {
		return "文本流返回应用错误"
	}
	return "文本流响应协议错误"
}

// sseDiagnostics are fixed stream metrics for logs.
type sseDiagnostics struct {
	Bytes              int    `json:"bytes"`
	Lines              int    `json:"lines"`
	DataEvents         int    `json:"dataEvents"`
	InvalidJSONEvents  int    `json:"invalidJsonEvents"`
	UnknownEvents      int    `json:"unknownEvents"`
	ErrorEvents        int    `json:"errorEvents"`
	DoneReceived       bool   `json:"doneReceived"`
	ReasoningDeltaSeen bool   `json:"reasoningDeltaSeen"`
	FinishReason       string `json:"finishReason"`
}

// streamAccumulator collects the reply across chunks.
type streamAccumulator struct {
	content      strings.Builder
	toolCalls    map[int]*ToolCall
	usage        any
	firstTokenMs *int64
}

func (a *streamAccumulator) completion(reason string, completed bool) Completion {
	content := a.content.String()
	indices := make([]int, 0, len(a.toolCalls))
	for i := range a.toolCalls {
		indices = append(indices, i)
	}
	slices.Sort(indices)
	var calls []ToolCall
	var wire []any
	for _, i := range indices {
		call := *a.toolCalls[i]
		if call.ID == "" {
			call.ID = fmt.Sprintf("call_%d", i)
		}
		calls = append(calls, call)
		wire = append(wire, map[string]any{
			"id": call.ID, "type": "function",
			"function": map[string]any{"name": call.Name, "arguments": call.Arguments},
		})
	}
	message := Message{"role": "assistant", "content": content}
	if len(wire) > 0 {
		message["tool_calls"] = wire
	}
	return Completion{Content: content, ToolCalls: calls, AssistantMessage: message, FinishReason: reason, Completed: completed}
}

// sseDecoder parses a server-sent event stream: LF, CR or CRLF line ends, a
// leading BOM, multi-line data, named events and a [DONE] marker.
type sseDecoder struct {
	buf          []byte
	dataLines    []string
	eventName    *string
	sawFirstLine bool
	done         bool
	skipLF       bool // a CR ended the last line; a following LF belongs to it
	diag         sseDiagnostics
	acc          streamAccumulator
	started      time.Time
}

func newSSEDecoder(started time.Time) *sseDecoder {
	return &sseDecoder{diag: sseDiagnostics{FinishReason: "none"}, acc: streamAccumulator{toolCalls: map[int]*ToolCall{}}, started: started}
}

// push consumes a chunk; true means [DONE] arrived and the rest is ignored.
func (d *sseDecoder) push(chunk []byte) (bool, error) {
	if d.done {
		return true, nil
	}
	d.diag.Bytes += len(chunk)
	d.buf = append(d.buf, chunk...)
	return d.consumeLines()
}

// finish handles the end of the stream: a last line without a terminator and
// a pending event still count.
func (d *sseDecoder) finish() (bool, error) {
	if d.done {
		return true, nil
	}
	if done, err := d.consumeLines(); done || err != nil {
		return done, err
	}
	if len(d.buf) > 0 {
		line := d.buf
		d.buf = nil
		if done, err := d.consumeLine(line); done || err != nil {
			return done, err
		}
	}
	if len(d.dataLines) > 0 {
		return d.dispatch()
	}
	return false, nil
}

func (d *sseDecoder) consumeLines() (bool, error) {
	for {
		line, ok := d.takeLine()
		if !ok {
			return false, nil
		}
		if done, err := d.consumeLine(line); done || err != nil {
			return done, err
		}
	}
}

func (d *sseDecoder) takeLine() ([]byte, bool) {
	if d.skipLF {
		if len(d.buf) == 0 {
			return nil, false
		}
		if d.buf[0] == '\n' {
			d.buf = d.buf[1:]
		}
		d.skipLF = false
	}
	for i, c := range d.buf {
		if c == '\n' || c == '\r' {
			line := d.buf[:i:i]
			d.skipLF = c == '\r'
			d.buf = d.buf[i+1:]
			return line, true
		}
	}
	return nil, false
}

func (d *sseDecoder) consumeLine(raw []byte) (bool, error) {
	d.diag.Lines++
	if !utf8.Valid(raw) {
		return false, sseInvalidUTF8
	}
	line := string(raw)
	if !d.sawFirstLine {
		d.sawFirstLine = true
		line = strings.TrimPrefix(line, "\uFEFF")
	}
	if line == "" {
		return d.dispatch()
	}
	if line[0] == ':' {
		return false, nil // comment
	}
	field, value, _ := strings.Cut(line, ":")
	value = strings.TrimPrefix(value, " ")
	switch field {
	case "data":
		d.dataLines = append(d.dataLines, value)
	case "event":
		d.eventName = &value
	case "id", "retry":
	default:
		d.diag.UnknownEvents++
	}
	return false, nil
}

func (d *sseDecoder) dispatch() (bool, error) {
	name := d.eventName
	d.eventName = nil
	if name != nil && *name == "error" {
		d.diag.ErrorEvents++
		d.dataLines = nil
		return false, sseApplicationError
	}
	if len(d.dataLines) == 0 {
		return false, nil
	}
	d.diag.DataEvents++
	data := strings.Join(d.dataLines, "\n")
	d.dataLines = nil
	if strings.TrimSpace(data) == "[DONE]" {
		d.done = true
		d.diag.DoneReceived = true
		return true, nil
	}
	value, err := decodeJSON([]byte(data))
	if err != nil {
		d.diag.InvalidJSONEvents++
		return false, sseInvalidJSON
	}
	object, _ := value.(map[string]any)
	if e, ok := object["error"]; ok && e != nil {
		d.diag.ErrorEvents++
		return false, sseApplicationError
	}
	if name != nil && *name != "message" {
		d.diag.UnknownEvents++
	}
	if !d.apply(object) {
		d.diag.UnknownEvents++
	}
	return false, nil
}

// apply folds one event into the reply and reports whether it was a
// recognized completion chunk. Reasoning deltas are noted, never kept.
func (d *sseDecoder) apply(value map[string]any) bool {
	usage, hasUsage := value["usage"]
	hasUsage = hasUsage && usage != nil
	if hasUsage {
		d.acc.usage = usage
	}
	choices, _ := value["choices"].([]any)
	if len(choices) == 0 {
		return hasUsage
	}
	choice, _ := choices[0].(map[string]any)
	reason := "none"
	if text, ok := choice["finish_reason"].(string); ok {
		reason = finishReasonCategory(text)
	}
	// Some gateways repeat the choice in a trailing usage chunk with an odd or
	// empty finish_reason; that must not demote a recorded "stop".
	if reason != "none" && !(reason == "other" && d.diag.FinishReason == "stop") {
		d.diag.FinishReason = reason
	}
	rawDelta, hasDelta := choice["delta"]
	if !hasDelta {
		return reason != "none"
	}
	delta, _ := rawDelta.(map[string]any)
	if content, ok := delta["content"].(string); ok && content != "" {
		d.markFirstToken()
		d.acc.content.WriteString(content)
	}
	reasoning, ok := delta["reasoning_content"]
	if !ok {
		reasoning = delta["reasoning"]
	}
	text, _ := reasoning.(string)
	reasoningSeen := text != ""
	if reasoningSeen {
		d.diag.ReasoningDeltaSeen = true
	}
	if calls, ok := delta["tool_calls"].([]any); ok {
		for _, item := range calls {
			call, _ := item.(map[string]any)
			index := 0
			if n, ok := jsonUint(call["index"]); ok {
				index = int(n)
			}
			builder := d.acc.toolCalls[index]
			if builder == nil {
				builder = &ToolCall{}
				d.acc.toolCalls[index] = builder
			}
			if id, ok := call["id"].(string); ok {
				builder.ID += id
			}
			if function, ok := call["function"].(map[string]any); ok {
				if name, ok := function["name"].(string); ok {
					builder.Name += name
				}
				if arguments, ok := function["arguments"].(string); ok {
					builder.Arguments += arguments
				}
			}
			d.markFirstToken()
		}
	}
	_, hasContent := delta["content"]
	_, hasCalls := delta["tool_calls"]
	return reason != "none" || reasoningSeen || hasContent || hasCalls
}

func (d *sseDecoder) markFirstToken() {
	if d.acc.firstTokenMs == nil {
		ms := time.Since(d.started).Milliseconds()
		d.acc.firstTokenMs = &ms
	}
}
