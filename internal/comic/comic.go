// Package comic is the Markdown comic workspace: per-chapter settings,
// script, storyboard and page-prompt documents with full revision history,
// content lineage that marks dependents stale, LLM generation, optimization
// and sync jobs, page rendering, and the work-level visual profile.
//
// Portable Markdown is the content contract; JSON is only IPC and job
// metadata.
//
// Lifecycle: New creates the service and its background context. Run
// Recover once at startup, before the window opens, to stop retired legacy
// executors and mark jobs of an earlier run interrupted. Generate, optimize,
// sync and render jobs run on goroutines tied to the service; Close cancels
// them and waits (bounded by its context). A job stopped by Close is marked
// interrupted exactly as startup recovery would, so nothing paid is retried.
package comic

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sync"

	"zombiezen.com/go/sqlite"

	"imageclient/internal/config"
	"imageclient/internal/ipc"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

// ChatMessage is one OpenAI-style chat message.
type ChatMessage struct {
	Role    string `json:"role"`
	Content any    `json:"content"` // string, or []ContentPart for multimodal input
}

// ContentPart is one part of a multimodal user message.
type ContentPart struct {
	Type     string    `json:"type"` // "text" | "image_url"
	Text     string    `json:"text,omitempty"`
	ImageURL *ImageURL `json:"image_url,omitempty"`
}

// ImageURL carries an inline data: URL image.
type ImageURL struct {
	URL string `json:"url"`
}

// TextRequest is one chat completion against the configured text model,
// captured when the job was accepted.
type TextRequest struct {
	Endpoint  string // full ".../chat/completions" URL
	APIKey    string
	Model     string
	Operation string // log label, e.g. "comic_markdown.optimize"
	Messages  []ChatMessage
}

// Completion is a model answer. Documents are saved only from a completion
// that ended normally: complete headings must not hide a truncated answer.
type Completion struct {
	Content      string
	FinishReason string // provider finish_reason ("stop", "length", "content_filter"); "" if absent
	Completed    bool   // the response ended normally
}

// TextModel sends chat completions. Errors are provider failures; their text
// is logged (keys removed) and, for interactive calls, shown to the user.
type TextModel interface {
	Complete(ctx context.Context, req TextRequest) (Completion, error)
}

// ImageReference is a reference image for image-to-image rendering.
type ImageReference struct {
	Path      string  `json:"path"`
	Role      string  `json:"role"`
	Weight    float64 `json:"weight"`
	SortOrder int64   `json:"sortOrder"`
}

// ImageRequest renders one comic page.
type ImageRequest struct {
	Settings   config.Settings // configuration captured when the job was accepted
	OutputDir  string          // where the produced files go
	Prompt     string
	References []ImageReference
	Size       string // "1024x1536"
	Quality    string // "high"
}

// ImageGenerator produces image files in req.OutputDir and describes them.
// It does not record them in the assets table.
type ImageGenerator interface {
	Generate(ctx context.Context, req ImageRequest) ([]store.AssetRef, error)
}

// Deps are the service's collaborators.
type Deps struct {
	Paths  platform.Paths
	Config *config.Store
	DB     *store.DB
	Events ipc.Emitter // unused: the workspace polls for job state
	Text   TextModel
	Images ImageGenerator
}

// Service implements the comic_md_* commands.
type Service struct {
	paths  platform.Paths
	config *config.Store
	db     *store.DB
	text   TextModel
	images ImageGenerator

	ctx    context.Context
	cancel context.CancelFunc
	mu     sync.Mutex
	closed bool
	jobs   sync.WaitGroup
}

func New(d Deps) *Service {
	ctx, cancel := context.WithCancel(context.Background())
	return &Service{
		paths: d.Paths, config: d.Config, db: d.DB, text: d.Text, images: d.Images,
		ctx: ctx, cancel: cancel,
	}
}

// Register adds the comic_md_* commands. Every command takes its arguments
// under "input".
func (s *Service) Register(r *ipc.Router) {
	r.Handle("comic_md_workspace_get", ipc.Typed(func(ctx context.Context, in ipc.Input[Scope]) (Workspace, error) {
		return withConn(ctx, s.db, func(conn *sqlite.Conn) (Workspace, error) { return loadWorkspace(conn, in.Input) })
	}))
	r.Handle("comic_md_catalog_list", ipc.Typed(func(ctx context.Context, in ipc.Input[CatalogListInput]) ([]CatalogEntry, error) {
		return CatalogList(ctx, s.db, in.Input.ProjectID)
	}))
	r.Handle("comic_md_work_visual_save", ipc.Typed(func(ctx context.Context, in ipc.Input[WorkVisualSaveInput]) (WorkVisualProfile, error) {
		return withTx(ctx, s.db, func(conn *sqlite.Conn) (WorkVisualProfile, error) { return saveWorkVisualProfile(conn, in.Input) })
	}))
	r.Handle("comic_md_work_visual_extract", ipc.Typed(func(ctx context.Context, in ipc.Input[WorkVisualExtractInput]) (WorkVisualExtraction, error) {
		return s.extractConstitution(ctx, in.Input)
	}))
	r.Handle("comic_md_work_visual_import", ipc.Typed(func(ctx context.Context, in ipc.Input[WorkVisualImportInput]) ([]store.AssetRef, error) {
		return s.importReferences(ctx, in.Input)
	}))
	r.Handle("comic_md_document_save", ipc.Typed(func(ctx context.Context, in ipc.Input[SaveInput]) (Document, error) {
		return withTx(ctx, s.db, func(conn *sqlite.Conn) (Document, error) { return saveDocument(conn, in.Input) })
	}))
	r.Handle("comic_md_document_history", ipc.Typed(func(ctx context.Context, in ipc.Input[HistoryInput]) ([]Revision, error) {
		return withConn(ctx, s.db, func(conn *sqlite.Conn) ([]Revision, error) { return documentHistory(conn, in.Input) })
	}))
	r.Handle("comic_md_generate", ipc.Typed(func(ctx context.Context, in ipc.Input[GenerateInput]) (Job, error) {
		return s.generate(ctx, in.Input)
	}))
	r.Handle("comic_md_optimize", ipc.Typed(func(ctx context.Context, in ipc.Input[OptimizeInput]) (Job, error) {
		return s.optimize(ctx, in.Input)
	}))
	r.Handle("comic_md_sync", ipc.Typed(func(ctx context.Context, in ipc.Input[SyncInput]) (Job, error) {
		return s.sync(ctx, in.Input)
	}))
	r.Handle("comic_md_render_options_save", ipc.Typed(func(ctx context.Context, in ipc.Input[RenderOptionsSaveInput]) (RenderOptions, error) {
		return withTx(ctx, s.db, func(conn *sqlite.Conn) (RenderOptions, error) { return saveRenderOptions(conn, in.Input) })
	}))
	r.Handle("comic_md_render", ipc.Typed(func(ctx context.Context, in ipc.Input[RenderInput]) (Job, error) {
		return s.render(ctx, in.Input)
	}))
	r.Handle("comic_md_export", ipc.Typed(func(ctx context.Context, in ipc.Input[ExportInput]) (ExportResult, error) {
		return withConn(ctx, s.db, func(conn *sqlite.Conn) (ExportResult, error) {
			return exportDocuments(conn, in.Input, filepath.Join(s.paths.Root, "exports"))
		})
	}))
}

// CatalogList projects the project's comic documents and rendered pages for
// the shared asset catalog (also used to resolve comic-md:// source URIs).
func CatalogList(ctx context.Context, db *store.DB, projectID string) ([]CatalogEntry, error) {
	return withConn(ctx, db, func(conn *sqlite.Conn) ([]CatalogEntry, error) { return catalogList(conn, projectID) })
}

func withConn[T any](ctx context.Context, db *store.DB, fn func(*sqlite.Conn) (T, error)) (T, error) {
	var out T
	err := db.With(ctx, func(conn *sqlite.Conn) (err error) {
		out, err = fn(conn)
		return err
	})
	return out, err
}

func withTx[T any](ctx context.Context, db *store.DB, fn func(*sqlite.Conn) (T, error)) (T, error) {
	var out T
	err := db.Tx(ctx, func(conn *sqlite.Conn) (err error) {
		out, err = fn(conn)
		return err
	})
	return out, err
}

// start runs a job on a service goroutine.
func (s *Service) start(jobID string, run func(ctx context.Context)) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		s.interrupt(jobID)
		return
	}
	s.jobs.Add(1)
	s.mu.Unlock()
	go func() {
		defer s.jobs.Done()
		defer func() {
			if p := recover(); p != nil {
				slog.Error("comic_markdown.job_panic", "jobId", jobID, "panic", fmt.Sprint(p))
				s.interrupt(jobID)
			}
		}()
		run(s.ctx)
	}()
}

// Close stops the background jobs and waits for them until ctx ends. Jobs
// still running are marked interrupted.
func (s *Service) Close(ctx context.Context) error {
	s.mu.Lock()
	s.closed = true
	s.mu.Unlock()
	s.cancel()
	done := make(chan struct{})
	go func() {
		s.jobs.Wait()
		close(done)
	}()
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		return fmt.Errorf("等待漫画后台任务结束超时：%w", ctx.Err())
	}
}

// Recovery reports what startup recovery changed.
type Recovery struct {
	LegacyRetired   int // pending records of the retired novel/comic executors
	InterruptedJobs int // comic_md jobs left running by an earlier run
}

// Recover runs the startup steps: retire pending legacy executor records,
// then mark this workspace's jobs of an earlier run interrupted and remove
// leftover render reference snapshots. Saved results are never deleted.
func (s *Service) Recover(ctx context.Context) (Recovery, error) {
	var out Recovery
	retired, err := withTx(ctx, s.db, retirePending)
	if err != nil {
		return out, err
	}
	out.LegacyRetired = retired
	slog.Info("legacy_comic.retired", "interruptedRecords", retired)
	out.InterruptedJobs, err = withConn(ctx, s.db, func(conn *sqlite.Conn) (int, error) {
		err := execSQL(conn, `UPDATE comic_md_jobs SET status='interrupted',message=? WHERE status='running'`, interruptedMessage)
		return conn.Changes(), err
	})
	os.RemoveAll(filepath.Join(s.paths.Assets(), renderSnapshotDir))
	return out, err
}

const legacyRetiredMessage = "旧小说漫画流程已停用；已有正文、分析记录和图片保留，请使用 Markdown 漫画工作区。"

// retirePending stops the retired novel/comic executors' pending records in
// one transaction, keeping their saved history.
func retirePending(conn *sqlite.Conn) (int, error) {
	now := nowMillis()
	changed := 0
	for _, statement := range []string{
		"UPDATE novel_production_jobs SET status='error',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,next_action=NULL,lease_owner=NULL,lease_expires_at=NULL,updated_at=?2,finished_at=?2 WHERE status IN ('queued','running','waiting_for_predecessor','blocked_config','blocked_conflict')",
		"UPDATE source_analysis_runs SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,updated_at=?2,completed_at=?2 WHERE status IN ('queued','running')",
		"UPDATE source_analysis_run_attempts SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running')",
		"UPDATE adaptation_analysis_runs SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,updated_at=?2,finished_at=?2 WHERE status IN ('draft','queued','running')",
		"UPDATE adaptation_analysis_run_attempts SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running')",
		"UPDATE novel_artifact_optimization_runs SET status='error',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,updated_at=?2,finished_at=?2 WHERE status IN ('queued','running','cancel_requested')",
		"UPDATE novel_artifact_optimization_attempts SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running')",
		"UPDATE comic_visual_batches SET status='failed',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,updated_at=?2,finished_at=?2 WHERE status IN ('authorized','waiting_text','preparing','running','blocked_config','needs_reconcile')",
		"UPDATE comic_visual_page_runs SET status='failed',failure_json=json_object('code','LEGACY_COMIC_RETIRED','message',?1),owner_app_session_id=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running','needs_reconcile')",
		"UPDATE comic_page_runs SET status='error',error=?1,failure_json=json_object('code','LEGACY_COMIC_RETIRED','message',?1),owner_app_session_id=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running','submitted','needs_reconcile')",
		"UPDATE comic_panel_runs SET status='error',error=?1,failure_json=json_object('code','LEGACY_COMIC_RETIRED','message',?1),owner_app_session_id=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running','submitted','needs_reconcile')",
	} {
		if err := rawExec(conn, statement, legacyRetiredMessage, now); err != nil {
			return 0, fmt.Errorf("停用旧小说漫画任务失败：%v", err)
		}
		changed += conn.Changes()
	}
	// blocked_config visual attempts have attempt_no=0 and cannot become
	// failed under the v17 CHECK; they never dispatched and no executor is
	// left to revive them, so their historical state stays.
	if err := rawExec(conn, "UPDATE comic_visual_batch_members SET status='failed',updated_at=?1,finished_at=?1 WHERE status IN ('pending','queued','running','blocked_config','needs_reconcile')", now); err != nil {
		return 0, fmt.Errorf("停用旧漫画页任务失败：%v", err)
	}
	changed += conn.Changes()
	return changed, nil
}
