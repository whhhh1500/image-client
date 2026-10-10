package gen

import (
	"bytes"
	"context"
	"encoding/json"
	"image"
	"image/color"
	"image/png"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"imageclient/internal/config"
	"imageclient/internal/ipc"
	"imageclient/internal/platform"
)

// fastDownloadPolicy keeps stalls and retries within a test run.
var fastDownloadPolicy = platform.DownloadPolicy{
	Attempts:   4,
	Timeouts:   platform.Timeouts{Header: 2 * time.Second, Idle: 150 * time.Millisecond, Total: 5 * time.Second},
	RetryDelay: 5 * time.Millisecond,
	Limit:      50 << 20,
	Label:      "图片",
}

type testEnv struct {
	s      *Service
	root   string
	output string
	events *eventRecorder
}

// newTestService builds a service on a temporary data root whose settings
// come from a backend snapshot (so no .env is consulted), with direct HTTP
// clients and short timeouts.
func newTestService(t *testing.T, mutate func(*config.Settings)) *testEnv {
	t.Helper()
	root := t.TempDir()
	paths := platform.Paths{Root: filepath.Join(root, "data")}
	output := filepath.Join(root, "output")
	settings := config.Settings{ImageModel: "image", VideoModel: "video", LLMModel: "llm", OutputDir: output, Source: "test"}
	if mutate != nil {
		mutate(&settings)
	}
	snapshot, err := json.Marshal(settings)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(paths.Root, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(paths.BackendConfig(), snapshot, 0o600); err != nil {
		t.Fatal(err)
	}
	events := &eventRecorder{}
	s := New(Deps{Paths: paths, Config: config.Load(paths), Events: events})
	client := &http.Client{Transport: &http.Transport{}}
	t.Cleanup(client.CloseIdleConnections)
	s.clientFor = func(string) *http.Client { return client }
	s.downloadClient = func() *http.Client { return client }
	s.imageDownload = fastDownloadPolicy
	s.imageTimeouts = platform.Timeouts{Header: 5 * time.Second, Idle: 2 * time.Second, Total: 10 * time.Second}
	s.llmTimeouts = platform.Timeouts{Header: 5 * time.Second, Idle: 2 * time.Second, Total: 10 * time.Second}
	s.catalogTimeout = 5 * time.Second
	s.videoTiming = videoTiming{
		firstPoll: time.Millisecond, pollInterval: 5 * time.Millisecond, pollDeadline: 5 * time.Second,
		backoffUnit: 5 * time.Millisecond, create: 5 * time.Second, poll: 5 * time.Second,
	}
	s.videoDownload = fastDownloadPolicy
	s.videoDownload.Limit, s.videoDownload.Label = maxVideoBytes, "视频"
	if err := os.MkdirAll(output, 0o755); err != nil {
		t.Fatal(err)
	}
	return &testEnv{s: s, root: root, output: output, events: events}
}

type recordedEvent struct {
	name    string
	payload any
}

type eventRecorder struct {
	mu     sync.Mutex
	events []recordedEvent
}

func (r *eventRecorder) Emit(name string, payload any) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.events = append(r.events, recordedEvent{name, payload})
}

func (r *eventRecorder) all() []recordedEvent {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]recordedEvent(nil), r.events...)
}

func pngBytes(t *testing.T, w, h int, c color.RGBA) []byte {
	t.Helper()
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for y := range h {
		for x := range w {
			img.Set(x, y, c)
		}
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func writePNG(t *testing.T, path string, c color.RGBA) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, pngBytes(t, 24, 16, c), 0o644); err != nil {
		t.Fatal(err)
	}
}

func mustJSON(t *testing.T, v any) string {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func TestRegisterAddsExactlyTheFrontendCommands(t *testing.T) {
	env := newTestService(t, nil)
	router := ipc.NewRouter()
	env.s.Register(router)
	want := []string{"fetch_models", "list_providers", "list_video_model_capabilities", "list_video_models", "llm_chat", "run_node", "run_video"}
	if got := router.Names(); !reflect.DeepEqual(got, want) {
		t.Fatalf("commands = %v, want %v", got, want)
	}
}

func TestRunTagNamesTheRunFolder(t *testing.T) {
	at := time.Date(2026, 9, 1, 8, 5, 3, 0, time.Local)
	cases := []struct{ prompt, want string }{
		// md5("") = d41d8cd98f00b204e9800998ecf8427e
		{"", "20260901-080503_gen_d41d8c"},
		{"  !!  ", "20260901-080503_gen_"},
		{"一只猫, in the 花园!!", "20260901-080503_一只猫-in-the-花园_"},
		{strings.Repeat("ab ", 20), "20260901-080503_ab-ab-ab-ab-ab-ab-ab-ab-_"},
	}
	for _, c := range cases {
		got := runTag(c.prompt, at)
		if !strings.HasPrefix(got, c.want) {
			t.Errorf("runTag(%q) = %q, want prefix %q", c.prompt, got, c.want)
		}
		if tail := got[strings.LastIndex(got, "_")+1:]; len(tail) != 6 {
			t.Errorf("runTag(%q) md5 part = %q", c.prompt, tail)
		}
	}
	if kindFolder("video") != "视频" || kindFolder("image") != "图片" || kindFolder("other") != "图片" {
		t.Fatal("kind folders changed")
	}
}

func TestProvidersListTheGateway(t *testing.T) {
	got := mustJSON(t, Providers())
	want := `[{"id":"zzone","name":"ZZone 网关","active":true,"capabilities":["text_to_image","image_to_image","text_to_video","image_to_video","text_completion"]}]`
	if got != want {
		t.Fatalf("providers = %s", got)
	}
	if _, err := SetActiveProvider("zzone"); err != nil {
		t.Fatal(err)
	}
	if _, err := SetActiveProvider("other"); err == nil || err.Error() != "未知 provider: other" {
		t.Fatalf("err = %v", err)
	}
	env := newTestService(t, nil)
	if _, err := env.s.GenerateProviderImage(context.Background(), "other", ImageRequest{Prompt: "x"}, env.output); err == nil || err.Error() != "VISUAL_PROVIDER_UNAVAILABLE" {
		t.Fatalf("err = %v", err)
	}
}

func TestNodeConfigReadsOnlyValuesOfTheExpectedType(t *testing.T) {
	node := parseNodeConfig(json.RawMessage(`{"s":"x","n":4,"f":4.0,"neg":-1,"big":4294967297,"null":null,"num":3,
		"arr":[" a ","",3,null,"b"]}`))
	if v, ok := node.str("s"); !ok || v != "x" {
		t.Fatal("str")
	}
	for _, key := range []string{"num", "null", "missing"} {
		if _, ok := node.str(key); ok {
			t.Fatalf("str(%s) must be absent", key)
		}
	}
	if v, ok := node.u32("n"); !ok || v != 4 {
		t.Fatal("u32")
	}
	for _, key := range []string{"f", "neg", "s", "missing"} {
		if _, ok := node.u32(key); ok {
			t.Fatalf("u32(%s) must be absent", key)
		}
	}
	if v, _ := node.u32("big"); v != 1 {
		t.Fatalf("u32 keeps the low 32 bits, got %d", v)
	}
	if got := node.strings("arr"); !reflect.DeepEqual(got, []string{"a", "b"}) {
		t.Fatalf("strings = %v", got)
	}
	if got := node.strings("missing"); got == nil || len(got) != 0 {
		t.Fatal("missing arrays read as empty")
	}
	if len(parseNodeConfig(json.RawMessage(`[1]`))) != 0 || len(parseNodeConfig(nil)) != 0 {
		t.Fatal("non-objects have no fields")
	}
}

func TestStatusLineUsesCanonicalReasons(t *testing.T) {
	if statusLine(422) != "422 Unprocessable Entity" || statusLine(599) != "599 <unknown status code>" {
		t.Fatal(statusLine(422), statusLine(599))
	}
}

func TestRunNodeSavesIntoTheRunFolderAndEmitsProgress(t *testing.T) {
	gw := startGateway(t, pngBytes(t, 4, 3, color.RGBA{2, 3, 4, 255}), nil)
	env := newTestService(t, gw.configure)
	router := ipc.NewRouter()
	env.s.Register(router)

	args := `{"req":{"nodeType":"image","category":"generate","config":{"prompt":"一只猫","model":"gpt-image-2"},"inputAssets":[]},"progressId":"p-1"}`
	result, err := router.Invoke(context.Background(), "run_node", json.RawMessage(args))
	if err != nil {
		t.Fatal(err)
	}
	run := result.(RunResult)
	if len(run.Assets) != 1 {
		t.Fatalf("assets = %+v", run.Assets)
	}
	dir := filepath.Dir(run.Assets[0].Path)
	if filepath.Dir(dir) != filepath.Join(env.output, "图片") || !strings.Contains(filepath.Base(dir), "_一只猫_") {
		t.Fatalf("asset saved in %s", dir)
	}
	encoded := mustJSON(t, run)
	if !strings.Contains(encoded, `"kind":"image"`) || !strings.Contains(encoded, `"format":"png"`) || strings.Contains(encoded, "width") {
		t.Fatalf("result = %s", encoded)
	}

	events := env.events.all()
	if len(events) == 0 {
		t.Fatal("no progress events")
	}
	var stages []string
	for _, e := range events {
		if e.name != ImageProgressEvent {
			t.Fatalf("event %s", e.name)
		}
		var payload map[string]any
		json.Unmarshal([]byte(mustJSON(t, e.payload)), &payload)
		if payload["progressId"] != "p-1" {
			t.Fatalf("payload = %v", payload)
		}
		stages = append(stages, payload["stage"].(string))
	}
	if want := []string{"waiting", "receiving", "downloading", "saving"}; !reflect.DeepEqual(stages, want) {
		t.Fatalf("stages = %v", stages)
	}

	// Without progressId nothing is emitted; unknown categories are refused.
	before := len(env.events.all())
	args = `{"req":{"nodeType":"image","category":"ai_process","config":{"prompt":"一只猫"}}}`
	if _, err := router.Invoke(context.Background(), "run_node", json.RawMessage(args)); err != nil {
		t.Fatal(err)
	}
	if len(env.events.all()) != before {
		t.Fatal("events without progressId")
	}
	args = `{"req":{"nodeType":"image","category":"video","config":{"prompt":"x"}}}`
	if _, err := router.Invoke(context.Background(), "run_node", json.RawMessage(args)); err == nil || err.Error() != "未知节点类别：video" {
		t.Fatalf("err = %v", err)
	}
}

func TestRunNodeChecksTheConnectionBeforeTheConfig(t *testing.T) {
	env := newTestService(t, nil)
	_, err := env.s.runNode(context.Background(), runNodeArgs{Req: RunNodeRequest{Category: "generate", Config: json.RawMessage(`{}`)}})
	if err == nil || err.Error() != "未配置图像 API（请在设置中填写图像地址与 Key）" {
		t.Fatalf("err = %v", err)
	}
	env = newTestService(t, func(s *config.Settings) {
		s.ImageAPIURL, s.ImageAPIKey = "http://127.0.0.1:9/v1/images/generations", "k"
	})
	for config, want := range map[string]string{
		`{}`:                            "缺少提示词 (prompt)",
		`{"prompt":3}`:                  "缺少提示词 (prompt)",
		`{"prompt":"  "}`:               "提示词为空",
		`{"prompt":"x","references":5}`: "references 必须是参考图对象数组",
		`{"prompt":"x","references":[{"path":"a"}]}`: "references 必须是参考图对象数组",
	} {
		_, err := env.s.runNode(context.Background(), runNodeArgs{Req: RunNodeRequest{Category: "generate", Config: json.RawMessage(config)}})
		if err == nil || err.Error() != want {
			t.Errorf("%s: err = %v, want %s", config, err, want)
		}
	}
}

func TestProviderErrorTextShowsTheGatewayReason(t *testing.T) {
	cases := []struct{ body, want string }{
		{`{"error":{"message":"模型请求失败，请稍后重试或更换模型。","type":"upstream_error","code":null}}`, "模型请求失败，请稍后重试或更换模型。"},
		{`{"error":{"code":"","message":"无权访问 补贴分组-Gemini 分组","type":"new_api_error"}}`, "无权访问 补贴分组-Gemini 分组"},
		{`{"error":"quota exhausted"}`, "quota exhausted"},
		{`{"message":"  rate limited  "}`, "rate limited"},
		{`{"error":{"code":"x"}}`, `{"error":{"code":"x"}}`}, // no reason: the body as it is
		{"<html>Bad Gateway</html>", "<html>Bad Gateway</html>"},
		{"", ""},
		{strings.Repeat("错", 500), strings.Repeat("错", 400)},
	}
	for _, c := range cases {
		if got := providerErrorText(c.body); got != c.want {
			t.Errorf("providerErrorText(%q) = %q, want %q", c.body, got, c.want)
		}
	}
}
