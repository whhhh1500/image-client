package comic

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"

	"zombiezen.com/go/sqlite"
)

// chapterSource is the chapter's current source revision and text. It also
// proves the scope: the chapter belongs to an active work of the project.
func chapterSource(conn *sqlite.Conn, s Scope) (revisionID *string, content string, err error) {
	found, err := queryOne(conn, `SELECT ch.current_revision_id,COALESCE(r.content,'') FROM novel_chapters ch JOIN novel_works w ON w.id=ch.novel_work_id LEFT JOIN novel_chapter_revisions r ON r.id=ch.current_revision_id AND r.novel_chapter_id=ch.id WHERE ch.id=? AND w.id=? AND w.project_id=? AND w.status='active'`,
		func(stmt *sqlite.Stmt) { revisionID, content = columnOptText(stmt, 0), stmt.ColumnText(1) },
		s.ChapterID, s.NovelWorkID, s.ProjectID)
	if err != nil {
		return nil, "", err
	}
	if !found {
		return nil, "", errors.New("未找到当前小说章节，请重新选择小说和章节")
	}
	return revisionID, content, nil
}

// chapterKey: settings belong to the work, everything else to a chapter.
func chapterKey(s Scope, kind string) string {
	if kind == kindSettings {
		return ""
	}
	return s.ChapterID
}

func listDocuments(conn *sqlite.Conn, s Scope) ([]Document, error) {
	b, err := loadBook(conn, s)
	if err != nil {
		return nil, err
	}
	return b.documents(), nil
}

func findDocument(docs []Document, id string) (Document, bool) {
	i := slices.IndexFunc(docs, func(d Document) bool { return d.ID == id })
	if i < 0 {
		return Document{}, false
	}
	return docs[i], true
}

func isDocumentKind(kind string) bool {
	return kind == kindSettings || kind == kindScript || kind == kindStoryboard || kind == kindPagePrompt
}

// saveDocument writes a new revision under optimistic concurrency: the caller
// names the revision it edited (nil for a new document). A plain save keeps
// the stored upstream receipt, so editing the body never silently confirms
// changed upstream content; acknowledgeUpdates records today's upstream.
func saveDocument(conn *sqlite.Conn, in SaveInput) (Document, error) {
	if _, _, err := chapterSource(conn, in.Scope); err != nil {
		return Document{}, err
	}
	if !isDocumentKind(in.Kind) {
		return Document{}, errors.New("未知文档类型")
	}
	if in.Kind == kindPagePrompt && (in.PageNo == nil || *in.PageNo < 1) {
		return Document{}, errors.New("请提供正整数页号")
	}
	if len(in.Markdown) > maxMarkdownBytes {
		return Document{}, errors.New("Markdown 文档过大，请拆分章节")
	}
	if len(in.OptimizationInstruction) > maxInstructionBytes {
		return Document{}, errors.New("优化要求过长，请精简后保存")
	}
	s := in.Scope
	var pageNo int64
	if in.Kind == kindPagePrompt {
		pageNo = *in.PageNo
	}
	var docID string
	var current *int64
	_, err := queryOne(conn, `SELECT id,revision FROM comic_md_documents WHERE novel_work_id=? AND chapter_id=? AND kind=? AND page_no=?`,
		func(stmt *sqlite.Stmt) { docID, current = stmt.ColumnText(0), ptr(stmt.ColumnInt64(1)) },
		s.NovelWorkID, chapterKey(s, in.Kind), in.Kind, pageNo)
	if err != nil {
		return Document{}, err
	}
	if !optEqual(current, in.ExpectedRevision) {
		return Document{}, errors.New("文档已有新版本，请刷新后对照保存；你的编辑尚未覆盖已有内容")
	}
	revision := int64(1)
	if current != nil {
		revision = *current + 1
	} else {
		docID = newID()
	}
	var storedDeps string
	hasStored, err := queryOne(conn, `SELECT markdown,dependencies FROM comic_md_documents WHERE id=?`,
		func(stmt *sqlite.Stmt) { storedDeps = stmt.ColumnText(1) }, docID)
	if err != nil {
		return Document{}, err
	}
	deps := storedDeps
	if !hasStored || in.AcknowledgeUpdates {
		b, err := loadBook(conn, s)
		if err != nil {
			return Document{}, err
		}
		deps = b.dependencies(in.Kind, in.PageNo)
	}
	now := nowMillis()
	if err := execSQL(conn, `INSERT INTO comic_md_documents(id,project_id,novel_work_id,chapter_id,kind,page_no,markdown,revision,dependencies,updated_at,optimization_instruction) VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET markdown=excluded.markdown,revision=excluded.revision,dependencies=excluded.dependencies,updated_at=excluded.updated_at,optimization_instruction=excluded.optimization_instruction`,
		docID, s.ProjectID, s.NovelWorkID, chapterKey(s, in.Kind), in.Kind, pageNo, in.Markdown, revision, deps, now, in.OptimizationInstruction); err != nil {
		return Document{}, err
	}
	if err := execSQL(conn, `INSERT INTO comic_md_revisions(document_id,revision,markdown,dependencies,created_at,optimization_instruction) VALUES(?,?,?,?,?,?)`,
		docID, revision, in.Markdown, deps, now, in.OptimizationInstruction); err != nil {
		return Document{}, err
	}
	docs, err := listDocuments(conn, s)
	if err != nil {
		return Document{}, err
	}
	saved, ok := findDocument(docs, docID)
	if !ok {
		return Document{}, errors.New("保存的文档不可用")
	}
	return saved, nil
}

func documentHistory(conn *sqlite.Conn, in HistoryInput) ([]Revision, error) {
	if _, _, err := chapterSource(conn, in.Scope); err != nil {
		return nil, err
	}
	docs, err := listDocuments(conn, in.Scope)
	if err != nil {
		return nil, err
	}
	if _, ok := findDocument(docs, in.DocumentID); !ok {
		return nil, errors.New("文档不属于当前章节")
	}
	out := []Revision{}
	err = querySQL(conn, `SELECT revision,markdown,created_at,optimization_instruction FROM comic_md_revisions WHERE document_id=? ORDER BY revision DESC`,
		func(stmt *sqlite.Stmt) {
			out = append(out, Revision{Revision: stmt.ColumnInt64(0), Markdown: stmt.ColumnText(1), CreatedAt: stmt.ColumnInt64(2), OptimizationInstruction: stmt.ColumnText(3)})
		}, in.DocumentID)
	return out, err
}

const jobColumns = `id,kind,status,message,output_markdown,completed_pages,total_pages,created_at`

func scanJob(stmt *sqlite.Stmt) Job {
	return Job{
		ID: stmt.ColumnText(0), Kind: stmt.ColumnText(1), Status: stmt.ColumnText(2),
		Message: columnOptText(stmt, 3), OutputMarkdown: columnOptText(stmt, 4),
		CompletedPages: stmt.ColumnInt64(5), TotalPages: stmt.ColumnInt64(6), CreatedAt: stmt.ColumnInt64(7),
	}
}

// listJobs shows the chapter's jobs, work settings jobs and anything running
// in the work (one running job blocks the whole work).
func listJobs(conn *sqlite.Conn, s Scope) ([]Job, error) {
	out := []Job{}
	err := querySQL(conn, `SELECT `+jobColumns+` FROM comic_md_jobs WHERE novel_work_id=? AND project_id=? AND (chapter_id=? OR kind='settings' OR status='running') ORDER BY created_at DESC LIMIT 30`,
		func(stmt *sqlite.Stmt) { out = append(out, scanJob(stmt)) }, s.NovelWorkID, s.ProjectID, s.ChapterID)
	return out, err
}

func loadWorkspace(conn *sqlite.Conn, s Scope) (Workspace, error) {
	revisionID, content, err := chapterSource(conn, s)
	if err != nil {
		return Workspace{}, err
	}
	b, err := loadBook(conn, s)
	if err != nil {
		return Workspace{}, err
	}
	docs := b.documents()
	options, err := renderOptions(conn, s)
	if err != nil {
		return Workspace{}, err
	}
	profile, err := workVisualProfile(conn, s.ProjectID, s.NovelWorkID)
	if err != nil {
		return Workspace{}, err
	}
	images, err := chapterImages(conn, s, docs, options, profile, referencesSnapshot(profile.References))
	if err != nil {
		return Workspace{}, err
	}
	usable := func(kind string) bool {
		return slices.ContainsFunc(docs, func(d Document) bool { return d.Kind == kind && !d.Stale && len(d.Issues) == 0 })
	}
	jobs, err := listJobs(conn, s)
	if err != nil {
		return Workspace{}, err
	}
	return Workspace{
		SourceRevisionID:  revisionID,
		SourceContent:     content,
		Documents:         docs,
		Jobs:              jobs,
		Images:            images,
		TextReady:         usable(kindSettings) && usable(kindScript) && usable(kindStoryboard),
		ImageReady:        usable(kindPagePrompt),
		RenderOptions:     options,
		WorkVisualProfile: profile,
		SyncPlan:          b.plan(),
		AffectedChapters:  b.affected(),
	}, nil
}

// chapterImages lists a chapter's rendered pages. A page is stale when its
// document, the chapter injection, the work visual profile or the file
// changed since it was rendered. Work-level inputs are passed in so callers
// walking many chapters load them once.
func chapterImages(conn *sqlite.Conn, s Scope, docs []Document, options RenderOptions, profile WorkVisualProfile, profileSnapshot string) ([]PageImage, error) {
	type row struct {
		image       PageImage
		jobSnapshot string
		persisted   *string
	}
	var rows []row
	err := querySQL(conn, `SELECT i.id,i.document_id,i.document_revision,i.page_no,i.path,i.created_at,i.prompt_injection,i.rerun_prompt_injection,i.visual_profile_revision,i.visual_reference_snapshot,r.markdown,j.input_snapshot,i.effective_prompt FROM comic_md_images i JOIN comic_md_documents d ON d.id=i.document_id JOIN comic_md_jobs j ON j.id=i.job_id LEFT JOIN comic_md_revisions r ON r.document_id=i.document_id AND r.revision=i.document_revision WHERE d.project_id=? AND d.novel_work_id=? AND d.chapter_id=? ORDER BY i.page_no,i.created_at DESC`,
		func(stmt *sqlite.Stmt) {
			rows = append(rows, row{
				image: PageImage{
					ID: stmt.ColumnText(0), DocumentID: stmt.ColumnText(1), DocumentRevision: stmt.ColumnInt64(2),
					PageNo: stmt.ColumnInt64(3), Path: stmt.ColumnText(4), CreatedAt: stmt.ColumnInt64(5),
					PromptInjection: stmt.ColumnText(6), RerunPromptInjection: stmt.ColumnText(7),
					VisualProfileRevision: stmt.ColumnInt64(8), VisualReferenceSnapshot: stmt.ColumnText(9),
					SourcePrompt: columnOptText(stmt, 10),
				},
				jobSnapshot: stmt.ColumnText(11),
				persisted:   columnOptText(stmt, 12),
			})
		}, s.ProjectID, s.NovelWorkID, s.ChapterID)
	if err != nil {
		return nil, err
	}
	out := make([]PageImage, 0, len(rows))
	for _, r := range rows {
		img := r.image
		img.FileAvailable = isRegularFile(img.Path)
		if img.SourcePrompt != nil {
			img.ContentHash = contentHash(*img.SourcePrompt)
		}
		img.EffectivePrompt = r.persisted
		if img.EffectivePrompt == nil {
			img.EffectivePrompt = historicalEffectivePrompt(img, r.jobSnapshot)
		}
		doc, found := findDocument(docs, img.DocumentID)
		img.Stale = !found || doc.Stale || doc.OutOfPlan || doc.ContentHash != img.ContentHash ||
			img.PromptInjection != options.PromptInjection ||
			img.VisualProfileRevision != profile.Revision ||
			img.VisualReferenceSnapshot != profileSnapshot ||
			!img.FileAvailable
		out = append(out, img)
	}
	return out, nil
}

func isRegularFile(path string) bool {
	info, err := os.Stat(path)
	return err == nil && info.Mode().IsRegular()
}

// historicalEffectivePrompt rebuilds the prompt of a page rendered before the
// effective prompt was persisted, but only when the job snapshot holds every
// component the renderer used. A page revision alone only proves the source
// prompt, not the provider request.
func historicalEffectivePrompt(img PageImage, jobSnapshot string) *string {
	var snapshot map[string]json.RawMessage
	if json.Unmarshal([]byte(jobSnapshot), &snapshot) != nil {
		return nil
	}
	var pagesSnapshot []map[string]json.RawMessage
	if json.Unmarshal(snapshot["pages"], &pagesSnapshot) != nil {
		return nil
	}
	markdown, found := "", false
	for _, p := range pagesSnapshot {
		var id string
		var revision int64
		if json.Unmarshal(p["id"], &id) != nil || id != img.DocumentID || isJSONNull(p["revision"]) ||
			json.Unmarshal(p["revision"], &revision) != nil || revision != img.DocumentRevision {
			continue
		}
		if isJSONNull(p["markdown"]) || json.Unmarshal(p["markdown"], &markdown) != nil {
			return nil
		}
		found = true
		break
	}
	if !found || (img.SourcePrompt != nil && *img.SourcePrompt != markdown) {
		return nil
	}
	constitution := ""
	if raw, ok := snapshot["workVisualProfile"]; ok {
		var profile map[string]json.RawMessage
		if json.Unmarshal(raw, &profile) != nil || profile == nil {
			return nil
		}
		value, ok := profile["constitutionMarkdown"]
		if !ok || isJSONNull(value) || json.Unmarshal(value, &constitution) != nil {
			return nil
		}
	} else if img.VisualProfileRevision != 0 || img.VisualReferenceSnapshot != "[]" {
		return nil
	}
	return ptr(renderPrompt(markdown, constitution, img.PromptInjection, img.RerunPromptInjection))
}

func renderOptions(conn *sqlite.Conn, s Scope) (RenderOptions, error) {
	if _, _, err := chapterSource(conn, s); err != nil {
		return RenderOptions{}, err
	}
	var out RenderOptions
	_, err := queryOne(conn, `SELECT prompt_injection,revision FROM comic_md_render_options WHERE chapter_id=? AND novel_work_id=? AND project_id=?`,
		func(stmt *sqlite.Stmt) {
			out = RenderOptions{PromptInjection: stmt.ColumnText(0), Revision: stmt.ColumnInt64(1)}
		},
		s.ChapterID, s.NovelWorkID, s.ProjectID)
	return out, err
}

func saveRenderOptions(conn *sqlite.Conn, in RenderOptionsSaveInput) (RenderOptions, error) {
	current, err := renderOptions(conn, in.Scope)
	if err != nil {
		return RenderOptions{}, err
	}
	if in.ExpectedRevision != current.Revision {
		return RenderOptions{}, errors.New("漫画注入规则已有新版本，请刷新后对照保存")
	}
	if len(in.PromptInjection) > maxInstructionBytes {
		return RenderOptions{}, errors.New("漫画注入规则过长，请精简后保存")
	}
	next := RenderOptions{PromptInjection: in.PromptInjection, Revision: current.Revision + 1}
	err = execSQL(conn, `INSERT INTO comic_md_render_options(chapter_id,novel_work_id,project_id,prompt_injection,revision,updated_at) VALUES(?,?,?,?,?,?) ON CONFLICT(chapter_id) DO UPDATE SET prompt_injection=excluded.prompt_injection,revision=excluded.revision,updated_at=excluded.updated_at`,
		in.ChapterID, in.NovelWorkID, in.ProjectID, next.PromptInjection, next.Revision, nowMillis())
	return next, err
}

// catalogList projects every chapter's documents and rendered pages of the
// project's active works, newest first. It fails closed: a workspace error
// reaches the caller instead of making the project look empty.
func catalogList(conn *sqlite.Conn, projectID string) ([]CatalogEntry, error) {
	type chapterRow struct {
		work, id string
		no       int64
		title    *string
	}
	var chapters []chapterRow
	err := querySQL(conn, `SELECT w.id,ch.id,ch.chapter_no,ch.title FROM novel_works w JOIN novel_chapters ch ON ch.novel_work_id=w.id WHERE w.project_id=? AND w.status='active' ORDER BY w.id,ch.sequence_no,ch.id`,
		func(stmt *sqlite.Stmt) {
			chapters = append(chapters, chapterRow{stmt.ColumnText(0), stmt.ColumnText(1), stmt.ColumnInt64(2), columnOptText(stmt, 3)})
		}, projectID)
	if err != nil {
		return nil, err
	}
	entries := []CatalogEntry{}
	emitted := map[string]bool{}
	for start := 0; start < len(chapters); {
		// Documents and the visual profile are work-scoped: load them once per work.
		work := chapters[start].work
		end := start + 1
		for end < len(chapters) && chapters[end].work == work {
			end++
		}
		workScope := Scope{ProjectID: projectID, NovelWorkID: work, ChapterID: chapters[start].id}
		if _, _, err := chapterSource(conn, workScope); err != nil {
			return nil, err
		}
		b, err := loadBook(conn, workScope)
		if err != nil {
			return nil, err
		}
		profile, err := workVisualProfile(conn, projectID, work)
		if err != nil {
			return nil, err
		}
		profileSnapshot := referencesSnapshot(profile.References)
		for _, ch := range chapters[start:end] {
			scope := Scope{ProjectID: projectID, NovelWorkID: work, ChapterID: ch.id}
			options, err := renderOptions(conn, scope)
			if err != nil {
				return nil, err
			}
			docs := b.documentsFor(ch.id)
			images, err := chapterImages(conn, scope, docs, options, profile, profileSnapshot)
			if err != nil {
				return nil, err
			}
			titled := ""
			if ch.title != nil {
				titled = "《" + *ch.title + "》"
			}
			for _, d := range docs {
				if emitted[d.ID] {
					continue
				}
				emitted[d.ID] = true
				entry := CatalogEntry{
					ProjectID: projectID, Kind: "text", NovelWorkID: work,
					DocumentID: ptr(d.ID), DocumentRevision: ptr(d.Revision), DocumentKind: ptr(d.Kind), PageNo: d.PageNo,
					Text: ptr(d.Markdown), PromptSnapshotComplete: true, CreatedAt: d.UpdatedAt,
					Stale: d.Stale || d.OutOfPlan || len(d.Issues) > 0,
				}
				scopeURI := "work"
				if d.Kind != kindSettings {
					entry.NovelChapterID, entry.ChapterNo, entry.ChapterTitle = ptr(ch.id), ptr(ch.no), ch.title
					scopeURI = ch.id
				}
				entry.SourceURI = fmt.Sprintf("comic-md://%s/%s/%s/document/%s@%d", projectID, work, scopeURI, d.ID, d.Revision)
				pageSuffix := ""
				if d.PageNo != nil {
					pageSuffix = fmt.Sprintf(" · 第%d页", *d.PageNo)
				}
				entry.Title = fmt.Sprintf("第%d章%s · 漫画%s%s", ch.no, titled, documentLabel(d.Kind, d.PageNo), pageSuffix)
				entries = append(entries, entry)
			}
			for _, img := range images {
				var kind *string
				if d, ok := findDocument(docs, img.DocumentID); ok {
					kind = ptr(d.Kind)
				}
				entries = append(entries, CatalogEntry{
					SourceURI: fmt.Sprintf("comic-md://%s/%s/%s/image/%s", projectID, work, ch.id, img.ID),
					ProjectID: projectID, Kind: "image", NovelWorkID: work,
					NovelChapterID: ptr(ch.id), ChapterNo: ptr(ch.no), ChapterTitle: ch.title,
					DocumentID: ptr(img.DocumentID), DocumentRevision: ptr(img.DocumentRevision), DocumentKind: kind,
					PageNo: ptr(img.PageNo), Title: fmt.Sprintf("第%d章%s · 漫画第%d页", ch.no, titled, img.PageNo),
					SourcePrompt: img.SourcePrompt, EffectivePrompt: img.EffectivePrompt,
					PromptSnapshotComplete: img.EffectivePrompt != nil,
					Path:                   ptr(img.Path), CreatedAt: img.CreatedAt, Stale: img.Stale,
				})
			}
		}
		start = end
	}
	slices.SortStableFunc(entries, func(a, b CatalogEntry) int {
		switch {
		case a.CreatedAt > b.CreatedAt:
			return -1
		case a.CreatedAt < b.CreatedAt:
			return 1
		}
		return 0
	})
	return entries, nil
}

// exportDocuments writes the selected (or every in-plan) document to a new
// "漫画文字-<uuid>" folder under root.
func exportDocuments(conn *sqlite.Conn, in ExportInput, root string) (ExportResult, error) {
	if _, _, err := chapterSource(conn, in.Scope); err != nil {
		return ExportResult{}, err
	}
	docs, err := listDocuments(conn, in.Scope)
	if err != nil {
		return ExportResult{}, err
	}
	if in.DocumentIDs != nil {
		ids := *in.DocumentIDs
		for _, id := range ids {
			if _, ok := findDocument(docs, id); !ok {
				return ExportResult{}, errors.New("所选文档不属于当前小说章节")
			}
		}
		docs = slices.DeleteFunc(docs, func(d Document) bool { return !slices.Contains(ids, d.ID) })
	} else {
		docs = slices.DeleteFunc(docs, func(d Document) bool { return d.OutOfPlan })
	}
	if len(docs) == 0 {
		return ExportResult{}, errors.New("还没有可以导出的 Markdown 文档")
	}
	dir := filepath.Join(root, "漫画文字-"+newID())
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return ExportResult{}, errors.New("创建导出目录失败")
	}
	files := make([]string, 0, len(docs))
	for _, d := range docs {
		var name string
		switch d.Kind {
		case kindSettings:
			name = "作品设定.md"
		case kindScript:
			name = "本章剧本.md"
		case kindStoryboard:
			name = "本章分镜.md"
		default:
			name = fmt.Sprintf("第%03d页-Prompt.md", orZero(d.PageNo))
		}
		path := filepath.Join(dir, name)
		if err := os.WriteFile(path, []byte(d.Markdown), 0o644); err != nil {
			return ExportResult{}, errors.New("写入 Markdown 文件失败")
		}
		files = append(files, path)
	}
	return ExportResult{Path: dir, Files: files}, nil
}

// --- jobs --------------------------------------------------------------------

const (
	jobRunning     = "running"
	jobSucceeded   = "succeeded"
	jobFailed      = "failed"
	jobInterrupted = "interrupted"

	interruptedMessage = "应用已关闭，任务已中断。已有成果已保留；如需继续，请手动重新提交，可能计费。"
)

// insertJob admits one running job per novel work, so a paid request is
// never duplicated.
func insertJob(conn *sqlite.Conn, s Scope, kind, snapshot string, total int64) (Job, error) {
	running := false
	if _, err := queryOne(conn, `SELECT EXISTS(SELECT 1 FROM comic_md_jobs WHERE novel_work_id=? AND status='running')`,
		func(stmt *sqlite.Stmt) { running = stmt.ColumnBool(0) }, s.NovelWorkID); err != nil {
		return Job{}, err
	}
	if running {
		return Job{}, errors.New("这部小说已有制作任务正在执行，请等待当前任务完成，避免重复计费")
	}
	job := Job{
		ID: newID(), Kind: kind, Status: jobRunning, Message: ptr("任务已提交，可以离开页面后再回来查看"),
		TotalPages: total, CreatedAt: nowMillis(),
	}
	err := execSQL(conn, `INSERT INTO comic_md_jobs(id,project_id,novel_work_id,chapter_id,kind,status,message,input_snapshot,total_pages,created_at) VALUES(?,?,?,?,?,'running',?,?,?,?)`,
		job.ID, s.ProjectID, s.NovelWorkID, s.ChapterID, kind, *job.Message, snapshot, total, job.CreatedAt)
	return job, err
}

// finishJob moves a running job to its terminal state; a job that already
// ended keeps its state.
func finishJob(conn *sqlite.Conn, jobID, status, message string) error {
	return execSQL(conn, `UPDATE comic_md_jobs SET status=?,message=? WHERE id=? AND status='running'`, status, message, jobID)
}

func isJobRunning(conn *sqlite.Conn, jobID string) (bool, error) {
	running := false
	_, err := queryOne(conn, `SELECT status='running' FROM comic_md_jobs WHERE id=?`,
		func(stmt *sqlite.Stmt) { running = stmt.ColumnBool(0) }, jobID)
	return running, err
}

// keepOutput stores the raw model output before validation, so rejected
// work can still be copied and repaired.
func keepOutput(conn *sqlite.Conn, jobID, output string) error {
	return execSQL(conn, `UPDATE comic_md_jobs SET output_markdown=? WHERE id=? AND status='running'`, output, jobID)
}

func jobProgress(conn *sqlite.Conn, jobID, message string) error {
	return execSQL(conn, `UPDATE comic_md_jobs SET message=?,output_markdown=NULL WHERE id=? AND status='running'`, message, jobID)
}
