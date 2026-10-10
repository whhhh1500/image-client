package restapi

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/google/uuid"
	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/config"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

// templateDB is a database migrated once for the whole test binary.
var templateDB string

func TestMain(m *testing.M) {
	dir, err := os.MkdirTemp("", "restapi-test-")
	if err != nil {
		panic(err)
	}
	templateDB = filepath.Join(dir, "template.db")
	db, err := store.Open(templateDB)
	if err != nil {
		panic(err)
	}
	db.Close()
	code := m.Run()
	os.RemoveAll(dir)
	os.Exit(code)
}

// readySettings has every service configured.
var readySettings = config.Settings{
	ImageAPIURL: "https://img.example/v1/images/generations", ImageAPIKey: "img-key", ImageModel: "gpt-image-2",
	VideoAPIURL: "https://vid.example/v1/videos", VideoAPIKey: "vid-key", VideoModel: "kling-video-v3",
	LLMAPIURL: "https://llm.example/v1", LLMAPIKey: "llm-key", LLMModel: "m-default",
	Source: "db",
}

// fakes implements every backend. Unset behaviours fail the test, which
// proves a rejected request never reached a provider.
type fakes struct {
	t *testing.T

	generateImage func(ctx context.Context, req ImageRequest, dir string) ([]store.AssetRef, error)
	generateVideo func(ctx context.Context, req VideoRequest, dir string) ([]store.AssetRef, error)
	complete      func(ctx context.Context, req TextRequest) (TextCompletion, error)
	runAgent      func(ctx context.Context, req AgentRequest) (string, error)
	imageModels   func(ctx context.Context) ([]string, error)
	videoModels   func(ctx context.Context) ([]string, error)

	mu        sync.Mutex
	images    []ImageRequest
	imageDirs []string
	videos    []VideoRequest
	videoDirs []string
	texts     []TextRequest
	agents    []AgentRequest
	saved     []savedFile
	active    string
}

type savedFile struct {
	label, text, model string // SaveText
	kind, extension    string // SaveMedia
	data               []byte
}

func (f *fakes) GenerateImage(ctx context.Context, req ImageRequest, dir string) ([]store.AssetRef, error) {
	f.mu.Lock()
	f.images, f.imageDirs = append(f.images, req), append(f.imageDirs, dir)
	f.mu.Unlock()
	if f.generateImage == nil {
		f.t.Errorf("unexpected GenerateImage(%+v)", req)
		return nil, errors.New("unexpected call")
	}
	return f.generateImage(ctx, req, dir)
}

func (f *fakes) GenerateVideo(ctx context.Context, req VideoRequest, dir string) ([]store.AssetRef, error) {
	f.mu.Lock()
	f.videos, f.videoDirs = append(f.videos, req), append(f.videoDirs, dir)
	f.mu.Unlock()
	if f.generateVideo == nil {
		f.t.Errorf("unexpected GenerateVideo(%+v)", req)
		return nil, errors.New("unexpected call")
	}
	return f.generateVideo(ctx, req, dir)
}

func (f *fakes) CheckPublicHTTPSURL(raw string) error {
	if !strings.HasPrefix(raw, "https://") || strings.Contains(raw, "127.0.0.1") {
		return errors.New("not public https")
	}
	return nil
}

func (f *fakes) Complete(ctx context.Context, req TextRequest) (TextCompletion, error) {
	f.mu.Lock()
	f.texts = append(f.texts, req)
	f.mu.Unlock()
	if f.complete == nil {
		f.t.Errorf("unexpected Complete(%s)", req.Operation)
		return TextCompletion{}, errors.New("unexpected call")
	}
	return f.complete(ctx, req)
}

func (f *fakes) RunAgent(ctx context.Context, req AgentRequest) (string, error) {
	f.mu.Lock()
	f.agents = append(f.agents, req)
	f.mu.Unlock()
	if f.runAgent == nil {
		f.t.Errorf("unexpected RunAgent")
		return "", errors.New("unexpected call")
	}
	return f.runAgent(ctx, req)
}

func (f *fakes) StripThinking(text string) string {
	for {
		lower := strings.ToLower(text)
		start := strings.Index(lower, "<think>")
		if start < 0 {
			break
		}
		end := strings.Index(lower[start:], "</think>")
		if end < 0 {
			text = text[:start]
			break
		}
		text = text[:start] + text[start+end+len("</think>"):]
	}
	return strings.TrimSpace(text)
}

func (f *fakes) ImageModels(ctx context.Context) ([]string, error) {
	if f.imageModels == nil {
		return []string{"gpt-image-2", "gemini-3-pro-image"}, nil
	}
	return f.imageModels(ctx)
}

func (f *fakes) VideoModels(ctx context.Context) ([]string, error) {
	if f.videoModels == nil {
		return []string{"kling-video-v3"}, nil
	}
	return f.videoModels(ctx)
}

func (f *fakes) VideoModelCapabilities() any {
	return []map[string]any{{"id": "kling-video-v3", "modes": []string{"text", "first_frame"}}}
}

func (f *fakes) Providers() any {
	return []map[string]any{{"id": "zzone", "name": "ZZone 网关", "active": f.active == "zzone"}}
}

func (f *fakes) SetActiveProvider(id string) error {
	if id != "zzone" {
		return errors.New("未知 provider: " + id)
	}
	f.active = id
	return nil
}

func (f *fakes) SaveText(_ context.Context, label, text, model string) (store.AssetRef, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.saved = append(f.saved, savedFile{label: label, text: text, model: model})
	id := uuid.NewString()
	return store.AssetRef{ID: id, Kind: "text", Path: filepath.Join("out", "剧本", id+".md"), Format: "md"}, nil
}

func (f *fakes) SaveMedia(_ context.Context, kind, extension string, data []byte) (store.AssetRef, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.saved = append(f.saved, savedFile{kind: kind, extension: extension, data: data})
	id := uuid.NewString()
	return store.AssetRef{ID: id, Kind: kind, Path: filepath.Join("out", id+"."+extension), Format: extension}, nil
}

type event struct {
	name    string
	payload any
}

type eventLog struct {
	mu     sync.Mutex
	events []event
}

func (l *eventLog) Emit(name string, payload any) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.events = append(l.events, event{name, payload})
}

func (l *eventLog) all() []event {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]event(nil), l.events...)
}

type testEnv struct {
	t      *testing.T
	srv    *Server
	fake   *fakes
	events *eventLog
	db     *store.DB
	cfg    *config.Store
	paths  platform.Paths
}

// newEnv serves the API over a temp data root seeded with settings, so the
// config store never falls back to a developer's .env.
func newEnv(t *testing.T, settings config.Settings) *testEnv {
	t.Helper()
	paths := platform.Paths{Root: t.TempDir()}
	if err := paths.Ensure(); err != nil {
		t.Fatal(err)
	}
	snapshot, err := json.Marshal(settings)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(paths.BackendConfig(), snapshot, 0o600); err != nil {
		t.Fatal(err)
	}
	// A copy of the migrated template opens without running migrations.
	template, err := os.ReadFile(templateDB)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(paths.Database(), template, 0o600); err != nil {
		t.Fatal(err)
	}
	db, err := store.Open(paths.Database())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	e := &testEnv{t: t, fake: &fakes{t: t, active: "zzone"}, events: &eventLog{}, db: db, cfg: config.Load(paths), paths: paths}
	e.srv = New(Deps{
		Config: e.cfg, DB: db, Feed: &store.HistoryFeed{}, Events: e.events, Paths: paths, Version: "9.9.9-test",
		Images: e.fake, Videos: e.fake, Text: e.fake, Catalog: e.fake, Files: e.fake,
		Addr: "127.0.0.1:0",
	})
	return e
}

// do serves one request in memory. A non-empty body is sent as JSON.
func (e *testEnv) do(method, path, body string, header ...string) *httptest.ResponseRecorder {
	e.t.Helper()
	req := httptest.NewRequest(method, path, strings.NewReader(body))
	if body != "" {
		req.Header.Set("Content-Type", "application/json")
	}
	for i := 0; i+1 < len(header); i += 2 {
		req.Header.Set(header[i], header[i+1])
	}
	rec := httptest.NewRecorder()
	e.srv.handler.ServeHTTP(rec, req)
	return rec
}

// jsonOf decodes a response body.
func jsonOf(t *testing.T, rec *httptest.ResponseRecorder) map[string]any {
	t.Helper()
	var v map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &v); err != nil {
		t.Fatalf("response is not a JSON object (%d): %q", rec.Code, rec.Body.String())
	}
	return v
}

// wantError checks the 400 error envelope and its message.
func wantError(t *testing.T, rec *httptest.ResponseRecorder, message string) {
	t.Helper()
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400; body %s", rec.Code, rec.Body.String())
	}
	got := jsonOf(t, rec)["error"].(map[string]any)
	if got["code"] != "bad_request" || got["message"] != message {
		t.Fatalf("error = %v, want message %q", got, message)
	}
}

// metadata reads an asset row's metadata JSON.
func (e *testEnv) metadata(id string) map[string]any {
	e.t.Helper()
	var raw string
	err := e.db.With(context.Background(), func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, "SELECT metadata FROM assets WHERE id = ?", &sqlitex.ExecOptions{
			Args:       []any{id},
			ResultFunc: func(stmt *sqlite.Stmt) error { raw = stmt.ColumnText(0); return nil },
		})
	})
	if err != nil || raw == "" {
		e.t.Fatalf("asset %s not recorded: %v", id, err)
	}
	var meta map[string]any
	if err := json.Unmarshal([]byte(raw), &meta); err != nil {
		e.t.Fatal(err)
	}
	return meta
}

func (e *testEnv) assetCount() int {
	e.t.Helper()
	n := 0
	err := e.db.With(context.Background(), func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, "SELECT COUNT(*) FROM assets", &sqlitex.ExecOptions{
			ResultFunc: func(stmt *sqlite.Stmt) error { n = stmt.ColumnInt(0); return nil },
		})
	})
	if err != nil {
		e.t.Fatal(err)
	}
	return n
}

// wantHistoryChange checks that exactly one history change was announced.
func (e *testEnv) wantHistoryChange(operation string, ids ...string) {
	e.t.Helper()
	events := e.events.all()
	if len(events) != 1 || events[0].name != "history://changed" {
		e.t.Fatalf("events = %+v, want one history://changed", events)
	}
	change, ok := events[0].payload.(store.HistoryChange)
	if !ok || change.Revision != 1 || change.Operation != operation || strings.Join(change.AssetIDs, ",") != strings.Join(ids, ",") {
		e.t.Fatalf("change = %+v, want revision 1 %s %v", events[0].payload, operation, ids)
	}
	if status := e.srv.feed.Status(); status["emittedRevision"] != uint64(1) {
		e.t.Fatalf("feed status = %v", status)
	}
}

func (e *testEnv) wantNothingRecorded() {
	e.t.Helper()
	if n := e.assetCount(); n != 0 {
		e.t.Fatalf("%d assets recorded, want none", n)
	}
	if events := e.events.all(); len(events) != 0 {
		e.t.Fatalf("events = %+v, want none", events)
	}
}
