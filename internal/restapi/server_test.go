package restapi

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"regexp"
	"strings"
	"testing"
	"time"

	"imageclient/internal/store"
)

func TestRetiredAndUnknownPathsAreNotFound(t *testing.T) {
	e := newEnv(t, readySettings)
	for _, path := range []string{
		"/api/novel_analysis_start",
		"/api/novel_production_start",
		"/api/novel_adaptation_analysis_start",
		"/api/comic_visual_batch_resume",
		"/api/v1/novel/production/start",
		"/api/v1/comic/visual/render",
		"/api/v1/health/",  // no trailing-slash redirect
		"/api//v1/health",  // no path cleaning redirect
		"/api/v1/%68ealth", // routes match the raw path
	} {
		rec := e.do("POST", path, "{}")
		if rec.Code != http.StatusNotFound {
			t.Fatalf("%s: status %d", path, rec.Code)
		}
		got := jsonOf(t, rec)["error"].(map[string]any)
		if got["code"] != "not_found" || got["message"] != "接口不存在，请读取 GET /api/v1/system/info" {
			t.Fatalf("%s: error = %v", path, got)
		}
	}
}

func TestRoutesRejectOtherMethods(t *testing.T) {
	e := newEnv(t, readySettings)
	for _, tc := range []struct{ method, path string }{
		{"GET", "/api/director"}, {"GET", "/api/script"}, {"GET", "/api/storyboard"},
		{"GET", "/api/v1/agents/orchestrations"}, {"GET", "/api/image"}, {"GET", "/api/video"},
		{"POST", "/api/v1/health"}, {"DELETE", "/api/v1/system/config"}, {"PUT", "/api/config"},
	} {
		if rec := e.do(tc.method, tc.path, ""); rec.Code != http.StatusMethodNotAllowed || rec.Header().Get("Allow") == "" {
			t.Fatalf("%s %s: status %d, Allow %q", tc.method, tc.path, rec.Code, rec.Header().Get("Allow"))
		}
	}
}

func TestVersionedContractAndLegacyAliases(t *testing.T) {
	e := newEnv(t, readySettings)
	for _, path := range []string{"/api/v1/health", "/api/health"} {
		rec := e.do("GET", path, "")
		health := jsonOf(t, rec)
		if rec.Code != 200 || health["status"] != "ok" || health["apiVersion"] != "v1" || health["service"] != "image-client-api" {
			t.Fatalf("%s: %d %v", path, rec.Code, health)
		}
		if ct := rec.Header().Get("Content-Type"); ct != "application/json" {
			t.Fatalf("content type %q", ct)
		}
	}

	info := jsonOf(t, e.do("GET", "/api/v1/system/info", ""))
	sync := info["historySynchronization"].(map[string]any)
	if info["authentication"] != "none" || info["historyPersistence"] != "sqlite" || info["version"] != "9.9.9-test" ||
		info["basePath"] != "/api/v1" || info["legacyBasePath"] != "/api" ||
		sync["mode"] != "event_driven" || sync["polling"] != false || sync["manualRefresh"] != true || sync["event"] != "history://changed" {
		t.Fatalf("info = %v", info)
	}
	routes := info["routes"].([]any)
	if len(routes) != 26 {
		t.Fatalf("%d routes, want 26", len(routes))
	}
	pathPattern := regexp.MustCompile(`^/api/v1/(system|catalog|text|agents|media|assets)(/[a-z0-9-]+)*$`)
	namePattern := regexp.MustCompile(`^[a-z][a-z0-9_]*$`)
	for _, item := range routes {
		rt := item.(map[string]any)
		path, method := rt["path"].(string), rt["method"].(string)
		if (!pathPattern.MatchString(path) && path != "/api/v1/health") || !namePattern.MatchString(rt["name"].(string)) {
			t.Fatalf("badly named route %v", rt)
		}
		// Every catalogued route is served: anything but 404/405.
		if rec := e.do(method, path, ""); rec.Code == http.StatusNotFound || rec.Code == http.StatusMethodNotAllowed {
			t.Fatalf("%s %s is catalogued but not served (%d)", method, path, rec.Code)
		}
	}

	status := jsonOf(t, e.do("GET", "/api/v1/system/history-sync", ""))
	if status["mode"] != "event_driven" || status["polling"] != false || status["emittedRevision"] != 0.0 {
		t.Fatalf("history sync = %v", status)
	}
	if cfg := jsonOf(t, e.do("GET", "/api/config", "")); cfg["imageReady"] != true || cfg["source"] != "db" {
		t.Fatalf("legacy config = %v", cfg)
	}
}

func TestLogPolicy(t *testing.T) {
	e := newEnv(t, readySettings)
	logs := jsonOf(t, e.do("GET", "/api/v1/system/logs", ""))
	if logs["directory"] != e.paths.Logs() || logs["format"] != "jsonl" || logs["dailyFiles"] != true ||
		logs["maxFileBytesExclusive"] != float64(10*1024*1024) || logs["retentionDays"] != 10.0 {
		t.Fatalf("logs = %v", logs)
	}
}

func TestRequestIDAndCORS(t *testing.T) {
	t.Setenv("API_CORS_ORIGINS", " https://tool.example ,*,,")
	e := newEnv(t, readySettings)
	for _, tc := range []struct {
		origin string
		allow  bool
	}{
		{"http://localhost:1420", true},
		{"http://127.0.0.1:1420", true},
		{"http://wails.localhost", true}, // Wails page origin on Windows
		{"wails://wails", true},          // Wails page origin on macOS/Linux
		{"https://tool.example", true},   // from API_CORS_ORIGINS
		{"http://tauri.localhost", false},
		{"tauri://localhost", false},
		{"https://evil.example", false}, // "*" in the variable grants nothing
	} {
		rec := e.do("GET", "/api/v1/health", "", "Origin", tc.origin)
		got := rec.Header().Get("Access-Control-Allow-Origin")
		if (got == tc.origin) != tc.allow || (!tc.allow && got != "") {
			t.Fatalf("origin %s: allow-origin %q", tc.origin, got)
		}
		if rec.Header().Get("X-Request-Id") == "" {
			t.Fatal("missing x-request-id")
		}
	}

	// Every OPTIONS request is a preflight, whatever the path.
	for _, path := range []string{"/api/v1/media/images/generations", "/api/nope"} {
		rec := e.do("OPTIONS", path, "", "Origin", "http://wails.localhost", "Access-Control-Request-Method", "POST")
		h := rec.Header()
		if rec.Code != 200 || rec.Body.Len() != 0 || h.Get("Access-Control-Allow-Origin") != "http://wails.localhost" ||
			h.Get("Access-Control-Allow-Methods") != "GET,POST,PUT,OPTIONS" || h.Get("Access-Control-Allow-Headers") != "*" ||
			h.Get("X-Request-Id") == "" {
			t.Fatalf("preflight %s: %d %v", path, rec.Code, h)
		}
	}
	if rec := e.do("GET", "/api/nope", ""); rec.Header().Get("X-Request-Id") == "" {
		t.Fatal("404 lacks x-request-id")
	}
}

func TestCORSOrigins(t *testing.T) {
	origins := corsOrigins(" https://a.example , * ,,bad\x01origin")
	for _, want := range append(defaultOrigins, "https://a.example") {
		if !origins[want] {
			t.Fatalf("missing %s in %v", want, origins)
		}
	}
	if len(origins) != len(defaultOrigins)+1 {
		t.Fatalf("origins = %v", origins)
	}
}

func TestJSONBodyRejections(t *testing.T) {
	e := newEnv(t, readySettings)
	for _, tc := range []struct {
		name, path, contentType, body string
		status                        int
	}{
		{"no content type", "/api/v1/media/images/generations", "", `{"prompt":"a"}`, 415},
		{"text content type", "/api/v1/media/images/generations", "text/plain", `{"prompt":"a"}`, 415},
		{"malformed", "/api/v1/media/images/generations", "application/json", `{"prompt":`, 400},
		{"trailing data", "/api/v1/media/images/generations", "application/json", `{"prompt":"a"} x`, 400},
		{"empty", "/api/v1/media/images/generations", "application/json", ``, 400},
		{"missing field", "/api/v1/media/images/generations", "application/json", `{}`, 422},
		{"null field", "/api/v1/media/images/generations", "application/json", `{"prompt":null}`, 422},
		{"wrong type", "/api/v1/media/images/generations", "application/json", `{"prompt":1}`, 422},
		{"not an object", "/api/v1/media/images/generations", "application/json", `[]`, 422},
		{"negative count", "/api/v1/media/images/generations", "application/json", `{"prompt":"a","n":-1}`, 422},
		{"null list", "/api/v1/media/videos/generations", "application/json", `{"prompt":"a","images":null}`, 422},
		{"duplicate duration", "/api/v1/media/videos/generations", "application/json", `{"prompt":"a","durationS":5,"duration_s":5}`, 422},
		{"incomplete tool", "/api/v1/agents/orchestrations", "application/json", `{"system":"s","input":"i","tools":[{"name":"a","description":"d"}]}`, 422},
		{"missing tools", "/api/v1/agents/orchestrations", "application/json", `{"system":"s","input":"i"}`, 422},
		{"partial config", "/api/v1/system/config", "application/json", `{"imageApiUrl":"https://x.example"}`, 422},
		{"null flag", "/api/config", "application/json", `{"imageApiUrl":"","imageApiKey":"","imageApiModel":"","videoApiUrl":"","videoApiKey":"","videoApiModel":"","llmApiUrl":"","llmApiKey":"","llmApiModel":"","outputDir":"","clearConnections":null}`, 422},
		{"missing provider id", "/api/v1/system/provider", "application/json", `{}`, 422},
		// A +json media type is JSON: the request reaches validation.
		{"json suffix", "/api/v1/media/images/generations", "application/vnd.api+json; charset=utf-8", `{"prompt":" "}`, 400},
	} {
		t.Run(tc.name, func(t *testing.T) {
			method := "POST"
			if strings.HasPrefix(tc.path, "/api/v1/system/") {
				method = "PUT"
			}
			req := httptest.NewRequest(method, tc.path, strings.NewReader(tc.body))
			if tc.contentType != "" {
				req.Header.Set("Content-Type", tc.contentType)
			}
			rec := httptest.NewRecorder()
			e.srv.handler.ServeHTTP(rec, req)
			if rec.Code != tc.status {
				t.Fatalf("status %d, want %d: %s", rec.Code, tc.status, rec.Body.String())
			}
		})
	}
	e.wantNothingRecorded()
}

func TestBodyLimits(t *testing.T) {
	e := newEnv(t, readySettings)
	post := func(path string, body io.Reader, contentLength int64) *httptest.ResponseRecorder {
		req := httptest.NewRequest("POST", path, body)
		req.Header.Set("Content-Type", "application/json")
		req.ContentLength = contentLength
		rec := httptest.NewRecorder()
		e.srv.handler.ServeHTTP(rec, req)
		return rec
	}
	// Declared too large: refused before reading.
	if rec := post("/api/v1/assets/text", strings.NewReader("{}"), maxJSONBody+1); rec.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("declared oversize: %d", rec.Code)
	}
	// Streamed too large: refused at the limit.
	stream := io.MultiReader(strings.NewReader(`{"label":"a","text":"`), io.LimitReader(repeatByte('x'), maxJSONBody))
	if rec := post("/api/v1/assets/text", stream, -1); rec.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("streamed oversize: %d", rec.Code)
	}
	// Only the media route accepts bodies beyond the JSON limit.
	rec := post("/api/v1/assets/media", strings.NewReader(`{"kind":"audio","extension":"mp3","dataBase64":"AA=="}`), maxJSONBody+1)
	wantError(t, rec, "kind 必须是 image 或 video")
	if maxMediaBody != 715_893_420 {
		t.Fatalf("media body limit = %d", maxMediaBody)
	}
}

type repeatByte byte

func (b repeatByte) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = byte(b)
	}
	return len(p), nil
}

func TestEnvAddr(t *testing.T) {
	for _, tc := range []struct {
		env  map[string]string
		want string
	}{
		{nil, "127.0.0.1:8123"},
		{map[string]string{"API_PORT": "9001"}, "127.0.0.1:9001"},
		{map[string]string{"API_HOST": "::1", "API_PORT": "9001"}, "[::1]:9001"},
		{map[string]string{"API_HOST": "localhost"}, "localhost:8123"},
		{map[string]string{"API_HOST": "", "API_PORT": "abc"}, "127.0.0.1:8123"},
		{map[string]string{"API_PORT": "70000"}, "127.0.0.1:8123"},
	} {
		lookup := func(key string) (string, bool) { v, ok := tc.env[key]; return v, ok }
		if got := envAddr(lookup); got != tc.want {
			t.Fatalf("%v: %s, want %s", tc.env, got, tc.want)
		}
	}
}

func startServer(t *testing.T, e *testEnv) string {
	t.Helper()
	if err := e.srv.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	addr := e.srv.Addr()
	if addr == "" {
		t.Fatal("server is not listening")
	}
	t.Cleanup(func() { e.srv.Shutdown(context.Background()) })
	return addr
}

func TestGracefulShutdownStopsAcceptingConnections(t *testing.T) {
	e := newEnv(t, readySettings)
	addr := startServer(t, e)

	conn, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	fmt.Fprint(conn, "GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
	response, _ := io.ReadAll(conn)
	conn.Close()
	if !bytes.Contains(response, []byte("200 OK")) {
		t.Fatalf("response %q", response)
	}

	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := e.srv.Shutdown(ctx); err != nil {
		t.Fatalf("graceful server must stop: %v", err)
	}
	if conn, err := net.DialTimeout("tcp", addr, 300*time.Millisecond); err == nil {
		conn.Close()
		t.Fatal("server still accepts connections")
	}
	if err := e.srv.Start(context.Background()); err == nil {
		t.Fatal("a stopped server must not start again")
	}
}

func TestShutdownWaitsForInFlightRequests(t *testing.T) {
	e := newEnv(t, readySettings)
	entered, release := make(chan struct{}), make(chan struct{})
	e.fake.generateImage = func(context.Context, ImageRequest, string) ([]store.AssetRef, error) {
		close(entered)
		<-release
		return []store.AssetRef{{ID: "img-1", Kind: "image", Path: "a.png"}}, nil
	}
	addr := startServer(t, e)
	responses := make(chan int, 1)
	go func() {
		resp, err := http.Post("http://"+addr+"/api/v1/media/images/generations", "application/json", strings.NewReader(`{"prompt":"猫"}`))
		if err != nil {
			responses <- 0
			return
		}
		resp.Body.Close()
		responses <- resp.StatusCode
	}()
	<-entered

	stopped := make(chan error, 1)
	go func() { stopped <- e.srv.Shutdown(context.Background()) }()
	select {
	case err := <-stopped:
		t.Fatalf("Shutdown returned before the in-flight request finished: %v", err)
	case <-time.After(150 * time.Millisecond):
	}
	if conn, err := net.DialTimeout("tcp", addr, 300*time.Millisecond); err == nil {
		conn.Close()
		t.Fatal("draining server still accepts connections")
	}
	close(release)
	if err := <-stopped; err != nil {
		t.Fatalf("drain: %v", err)
	}
	if status := <-responses; status != http.StatusOK {
		t.Fatalf("in-flight request ended with %d", status)
	}
	e.wantHistoryChange("rest_image_generation", "img-1")
}

func TestShutdownCancelsRequestsPastTheDeadline(t *testing.T) {
	e := newEnv(t, readySettings)
	entered, cancelled := make(chan struct{}), make(chan struct{})
	e.fake.generateImage = func(ctx context.Context, _ ImageRequest, _ string) ([]store.AssetRef, error) {
		close(entered)
		<-ctx.Done()
		close(cancelled)
		return nil, ctx.Err()
	}
	addr := startServer(t, e)
	clientDone := make(chan error, 1)
	go func() {
		resp, err := http.Post("http://"+addr+"/api/v1/media/images/generations", "application/json", strings.NewReader(`{"prompt":"猫"}`))
		if err == nil {
			resp.Body.Close()
		}
		clientDone <- err
	}()
	<-entered

	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	err := e.srv.Shutdown(ctx)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Shutdown = %v, want the deadline", err)
	}
	if again := e.srv.Shutdown(context.Background()); again != err {
		t.Fatalf("second Shutdown = %v, want the first result", again)
	}
	select {
	case <-cancelled:
	case <-time.After(2 * time.Second):
		t.Fatal("the straggling request was not cancelled")
	}
	select {
	case err := <-clientDone:
		if err == nil {
			t.Fatal("the straggler's connection should have been dropped")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("client still waiting")
	}
	e.wantNothingRecorded()
}

func TestStartSkipsATakenPort(t *testing.T) {
	taken, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer taken.Close()

	var logs bytes.Buffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewJSONHandler(&logs, nil)))
	defer slog.SetDefault(previous)

	e := newEnv(t, readySettings)
	e.srv.addr = taken.Addr().String()
	if err := e.srv.Start(context.Background()); err != nil {
		t.Fatalf("a taken port must not fail startup: %v", err)
	}
	if e.srv.Addr() != "" {
		t.Fatal("server claims to listen")
	}
	if err := e.srv.Shutdown(context.Background()); err != nil {
		t.Fatalf("Shutdown without a listener: %v", err)
	}
	found := false
	for scanner := bufio.NewScanner(&logs); scanner.Scan(); {
		found = found || strings.Contains(scanner.Text(), `"msg":"api.server.bind_failed"`)
	}
	if !found {
		t.Fatalf("bind failure not logged: %s", logs.String())
	}
}
