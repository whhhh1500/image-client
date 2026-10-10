package comic

import (
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/config"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

const (
	settingsMD = "## 世界观\n古代小镇。\n## 画风\n彩色漫画。\n## 人物锚点\n林青，黑发，左眉旧伤疤。"
	scriptMD   = "## 剧情\n林青送信。\n## 场景与对白\n客栈，林青：别出声。\n## 人物锚点补充\n左臂包扎，持续到康复。"
	boardMD    = "# 第1页\n## 本页剧情\n林青送信。\n## 分镜\n### 第1格\n远景，人物进入客栈。\n## 画面文字\n无对白。\n## 人物状态\n左臂包扎。"
	promptMD   = "# 第1页\n## 画面要求\n竖版彩色漫画。\n## 世界观与场景\n古代客栈。\n## 人物锚点\n林青，黑发，左眉旧伤疤。\n## 人物锚点补充\n左臂包扎。\n## 剧情与分镜\n### 第1格\n远景，林青走进客栈。\n## 画面文字\n无对白。\n## 连续性要求\n左臂包扎保持一致。"
)

const novelFixtureSQL = `
INSERT INTO novel_works(id,project_id,title,status,created_at,updated_at) VALUES('work','project','测试作品','active',1,1);
INSERT INTO novel_chapters(id,novel_work_id,sequence_no,chapter_no,current_revision_id,created_at,updated_at)
VALUES('ch1','work',1,1,'src1',1,1),('ch2','work',2,2,'src2',1,1),('ch3','work',3,3,'src3',1,1);
INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,source_kind,created_at)
VALUES('src1','ch1',1,'第一章正文','h1','paste',1),('src2','ch2',1,'第二章正文','h2','paste',1),('src3','ch3',1,'第三章正文','h3','paste',1);
`

var pngBytes = []byte("\x89PNG\r\n\x1a\nstyle reference")

func TestMain(m *testing.M) {
	slog.SetDefault(slog.New(slog.DiscardHandler))
	os.Exit(m.Run())
}

// A migrated database is built once and copied per test: running every
// migration for each test would dominate the run time.
var (
	templateOnce sync.Once
	templateDB   []byte
	templateErr  error
)

func migratedTemplate(t *testing.T) []byte {
	templateOnce.Do(func() {
		dir, err := os.MkdirTemp("", "comic-template-")
		if err != nil {
			templateErr = err
			return
		}
		defer os.RemoveAll(dir)
		path := filepath.Join(dir, "image-client.db")
		db, err := store.Open(path)
		if err != nil {
			templateErr = err
			return
		}
		if err := db.Close(); err != nil {
			templateErr = err
			return
		}
		templateDB, templateErr = os.ReadFile(path)
	})
	if templateErr != nil {
		t.Fatal(templateErr)
	}
	return templateDB
}

type fixture struct {
	t     *testing.T
	paths platform.Paths
	db    *store.DB
	conn  *sqlite.Conn // a direct connection for testing the storage functions
	scope Scope
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	paths := platform.Paths{Root: t.TempDir()}
	if err := paths.Ensure(); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(paths.Database(), migratedTemplate(t), 0o600); err != nil {
		t.Fatal(err)
	}
	db, err := store.Open(paths.Database())
	if err != nil {
		t.Fatal(err)
	}
	conn, err := sqlite.OpenConn(paths.Database(), sqlite.OpenReadWrite|sqlite.OpenWAL)
	if err != nil {
		t.Fatal(err)
	}
	conn.SetBusyTimeout(5 * time.Second)
	t.Cleanup(func() {
		conn.Close()
		db.Close()
	})
	f := &fixture{t: t, paths: paths, db: db, conn: conn, scope: Scope{ProjectID: "project", NovelWorkID: "work", ChapterID: "ch1"}}
	f.exec("PRAGMA foreign_keys=ON")
	if err := sqlitex.ExecuteScript(conn, novelFixtureSQL, nil); err != nil {
		t.Fatal(err)
	}
	return f
}

func (f *fixture) chapter(id string) Scope {
	s := f.scope
	s.ChapterID = id
	return s
}

func (f *fixture) exec(query string, args ...any) {
	f.t.Helper()
	if err := sqlitex.Execute(f.conn, query, &sqlitex.ExecOptions{Args: args}); err != nil {
		f.t.Fatalf("%s: %v", query, err)
	}
}

func (f *fixture) queryText(query string, args ...any) string {
	f.t.Helper()
	var out string
	found, err := queryOne(f.conn, query, func(stmt *sqlite.Stmt) { out = stmt.ColumnText(0) }, args...)
	if err != nil || !found {
		f.t.Fatalf("%s: found=%v err=%v", query, found, err)
	}
	return out
}

func (f *fixture) queryInt(query string, args ...any) int64 {
	f.t.Helper()
	var out int64
	found, err := queryOne(f.conn, query, func(stmt *sqlite.Stmt) { out = stmt.ColumnInt64(0) }, args...)
	if err != nil || !found {
		f.t.Fatalf("%s: found=%v err=%v", query, found, err)
	}
	return out
}

func rev(n int64) *int64 { return &n }

// put saves with acknowledgement; page prompts are page 1.
func (f *fixture) put(s Scope, kind, md string, expected *int64) Document {
	f.t.Helper()
	var pageNo *int64
	if kind == kindPagePrompt {
		pageNo = rev(1)
	}
	d, err := saveDocument(f.conn, SaveInput{Scope: s, Kind: kind, PageNo: pageNo, Markdown: md, ExpectedRevision: expected, AcknowledgeUpdates: true})
	if err != nil {
		f.t.Fatalf("save %s: %v", kind, err)
	}
	return d
}

// pagePut saves page n of promptMD-like Markdown, renumbering "第1页".
func (f *fixture) pagePut(s Scope, n int64, md string, expected *int64) Document {
	f.t.Helper()
	d, err := saveDocument(f.conn, SaveInput{
		Scope: s, Kind: kindPagePrompt, PageNo: rev(n), Markdown: strings.ReplaceAll(md, "第1页", "第"+strconv.FormatInt(n, 10)+"页"),
		ExpectedRevision: expected, AcknowledgeUpdates: true,
	})
	if err != nil {
		f.t.Fatalf("save page %d: %v", n, err)
	}
	return d
}

// metadataPut changes only the optimization instruction, without
// acknowledging upstream changes.
func (f *fixture) metadataPut(s Scope, d Document) Document {
	f.t.Helper()
	next, err := saveDocument(f.conn, SaveInput{
		Scope: s, Kind: d.Kind, PageNo: d.PageNo, Markdown: d.Markdown, OptimizationInstruction: "只改优化要求",
		ExpectedRevision: rev(d.Revision),
	})
	if err != nil {
		f.t.Fatal(err)
	}
	return next
}

func (f *fixture) pipeline(s Scope) {
	f.put(s, kindSettings, settingsMD, nil)
	f.put(s, kindScript, scriptMD, nil)
	f.put(s, kindStoryboard, boardMD, nil)
}

func (f *fixture) docs(s Scope) []Document {
	f.t.Helper()
	docs, err := listDocuments(f.conn, s)
	if err != nil {
		f.t.Fatal(err)
	}
	return docs
}

func (f *fixture) doc(s Scope, pred func(Document) bool) Document {
	f.t.Helper()
	docs := f.docs(s)
	i := slices.IndexFunc(docs, pred)
	if i < 0 {
		f.t.Fatal("document not found")
	}
	return docs[i]
}

func byKind(kind string) func(Document) bool { return func(d Document) bool { return d.Kind == kind } }
func byID(id string) func(Document) bool     { return func(d Document) bool { return d.ID == id } }

func (f *fixture) jobs(s Scope) []Job {
	f.t.Helper()
	jobs, err := listJobs(f.conn, s)
	if err != nil {
		f.t.Fatal(err)
	}
	return jobs
}

func (f *fixture) insertJob(s Scope, kind string, total int64) Job {
	f.t.Helper()
	job, err := insertJob(f.conn, s, kind, "{}", total)
	if err != nil {
		f.t.Fatal(err)
	}
	return job
}

func (f *fixture) freeze(s Scope, stage, source string) generation {
	f.t.Helper()
	g, err := freezeGeneration(f.conn, GenerateInput{Scope: s, Stage: stage, ExpectedSourceRevisionID: source})
	if err != nil {
		f.t.Fatal(err)
	}
	return g
}

func (f *fixture) workspace(s Scope) Workspace {
	f.t.Helper()
	w, err := loadWorkspace(f.conn, s)
	if err != nil {
		f.t.Fatal(err)
	}
	return w
}

func (f *fixture) book(s Scope) *book {
	f.t.Helper()
	b, err := loadBook(f.conn, s)
	if err != nil {
		f.t.Fatal(err)
	}
	return b
}

func (f *fixture) optimizeFixture(s Scope, docs []Document, instruction string) (optimization, Job) {
	f.t.Helper()
	targets := make([]PageInput, len(docs))
	for i, d := range docs {
		targets[i] = PageInput{DocumentID: d.ID, Revision: d.Revision}
	}
	o, err := freezeOptimization(f.conn, OptimizeInput{Scope: s, Targets: targets, Instruction: instruction, AllPages: len(docs) > 1})
	if err != nil {
		f.t.Fatal(err)
	}
	return o, f.insertJob(s, "optimize", int64(len(o.Targets)))
}

// insertImage records a rendered page directly.
func (f *fixture) insertImage(id string, job Job, d Document, path string, extra map[string]any) {
	f.t.Helper()
	cols := []string{"id", "job_id", "document_id", "document_revision", "page_no", "path", "created_at"}
	args := []any{id, job.ID, d.ID, d.Revision, orZero(d.PageNo), path, nowMillis()}
	for _, k := range slices.Sorted(func(yield func(string) bool) {
		for k := range extra {
			if !yield(k) {
				return
			}
		}
	}) {
		cols = append(cols, k)
		args = append(args, extra[k])
	}
	f.exec("INSERT INTO comic_md_images("+strings.Join(cols, ",")+") VALUES(?"+strings.Repeat(",?", len(cols)-1)+")", args...)
}

func (f *fixture) writeFile(name string, data []byte) string {
	f.t.Helper()
	path := filepath.Join(f.paths.Root, name)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		f.t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		f.t.Fatal(err)
	}
	return path
}

func completed(md string) Completion {
	return Completion{Content: md, FinishReason: "stop", Completed: true}
}

func mustContain(t *testing.T, s, sub string) {
	t.Helper()
	if !strings.Contains(s, sub) {
		t.Fatalf("expected %q in:\n%s", sub, s)
	}
}

func mustErr(t *testing.T, err error, sub string) {
	t.Helper()
	if err == nil {
		t.Fatalf("expected error containing %q", sub)
	}
	if !strings.Contains(err.Error(), sub) {
		t.Fatalf("error %q does not contain %q", err.Error(), sub)
	}
}

// --- service fakes ---------------------------------------------------------------

type fakeText struct {
	mu       sync.Mutex
	requests []TextRequest
	reply    func(ctx context.Context, req TextRequest) (Completion, error)
}

func (f *fakeText) Complete(ctx context.Context, req TextRequest) (Completion, error) {
	f.mu.Lock()
	f.requests = append(f.requests, req)
	reply := f.reply
	f.mu.Unlock()
	return reply(ctx, req)
}

func (f *fakeText) seen() []TextRequest {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.requests)
}

type fakeImages struct {
	mu       sync.Mutex
	requests []ImageRequest
	fail     error
}

func (f *fakeImages) Generate(_ context.Context, req ImageRequest) ([]store.AssetRef, error) {
	f.mu.Lock()
	f.requests = append(f.requests, req)
	f.mu.Unlock()
	if f.fail != nil {
		return nil, f.fail
	}
	for _, r := range req.References {
		if _, err := os.Stat(r.Path); err != nil {
			return nil, err
		}
	}
	data := []byte("\x89PNG\r\n\x1a\nrendered page")
	ref, err := saveTestAsset(req.OutputDir, data)
	if err != nil {
		return nil, err
	}
	return []store.AssetRef{ref}, nil
}

func (f *fakeImages) seen() []ImageRequest {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.requests)
}

func saveTestAsset(dir string, data []byte) (store.AssetRef, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return store.AssetRef{}, err
	}
	id := newID()
	path := filepath.Join(dir, id+".png")
	return store.AssetRef{ID: id, Kind: "image", Path: path, Format: "png"}, os.WriteFile(path, data, 0o644)
}

// newService wires a service with fake providers and configured endpoints.
func (f *fixture) newService(text TextModel, images ImageGenerator) *Service {
	f.t.Helper()
	// A snapshot keeps config.Load from falling back to a developer .env.
	if err := os.WriteFile(f.paths.BackendConfig(), []byte(`{"source":"none"}`), 0o600); err != nil {
		f.t.Fatal(err)
	}
	cfg := config.Load(f.paths)
	if err := cfg.Replace(config.Settings{
		ImageAPIURL: "https://image.test/v1/images", ImageAPIKey: "image-secret-key", ImageModel: "image-model",
		LLMAPIURL: "https://llm.test/v1/", LLMAPIKey: "llm-secret-key", LLMModel: "text-model", Source: "db",
	}); err != nil {
		f.t.Fatal(err)
	}
	s := New(Deps{Paths: f.paths, Config: cfg, DB: f.db, Text: text, Images: images})
	f.t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		s.Close(ctx)
	})
	return s
}

// waitJob polls until the job leaves the running state.
func (f *fixture) waitJob(id string) Job {
	f.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		var job Job
		found, err := queryOne(f.conn, `SELECT `+jobColumns+` FROM comic_md_jobs WHERE id=?`, func(stmt *sqlite.Stmt) { job = scanJob(stmt) }, id)
		if err != nil || !found {
			f.t.Fatalf("job %s: found=%v err=%v", id, found, err)
		}
		if job.Status != jobRunning {
			return job
		}
		time.Sleep(5 * time.Millisecond)
	}
	f.t.Fatalf("job %s still running", id)
	return Job{}
}
