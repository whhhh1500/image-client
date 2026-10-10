package comic

import (
	"cmp"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"maps"
	"slices"
	"strconv"
	"strings"

	"zombiezen.com/go/sqlite"

	"imageclient/internal/config"
	"imageclient/internal/platform"
)

// Every background job follows the same rules: admission freezes the inputs
// and fails fast; the paid call runs outside the database; the result is
// applied only if nothing it was based on changed meanwhile (otherwise the
// raw output stays on the job for copying); and a failure never triggers an
// automatic, billable retry.

// --- generation ----------------------------------------------------------------

// genTarget identifies a document version a generation may overwrite.
type genTarget struct {
	ID       string
	PageNo   *int64
	Revision int64
}

func (t genTarget) MarshalJSON() ([]byte, error) {
	return json.Marshal([]any{t.ID, nullable(t.PageNo), t.Revision})
}

func (t genTarget) equal(o genTarget) bool {
	return t.ID == o.ID && optEqual(t.PageNo, o.PageNo) && t.Revision == o.Revision
}

// generation is a frozen generation request (stored as the job snapshot).
type generation struct {
	Scope              Scope       `json:"scope"`
	Stage              string      `json:"stage"`
	SourceRevision     string      `json:"source_revision"`
	Dependencies       string      `json:"dependencies"`
	WorkVisualRevision int64       `json:"work_visual_revision"`
	Targets            []genTarget `json:"targets"`
	Prompt             string      `json:"prompt"`
}

func stageKind(stage string) string {
	if stage == "page_prompts" {
		return kindPagePrompt
	}
	return stage
}

func generationTargets(docs []Document, kind string) []genTarget {
	out := []genTarget{}
	for _, d := range docs {
		if d.Kind == kind {
			out = append(out, genTarget{d.ID, d.PageNo, d.Revision})
		}
	}
	return out
}

// readyDocument finds a complete, current document of kind.
func readyDocument(docs []Document, kind string) (Document, error) {
	for _, d := range docs {
		if d.Kind == kind && !d.Stale && len(d.Issues) == 0 {
			return d, nil
		}
	}
	return Document{}, fmt.Errorf("请先保存完整且最新的%s", documentLabel(kind, nil))
}

func freezeGeneration(conn *sqlite.Conn, in GenerateInput) (generation, error) {
	switch in.Stage {
	case "settings", "script", "storyboard", "page_prompts":
	default:
		return generation{}, errors.New("未知生成步骤")
	}
	revision, content, err := chapterSource(conn, in.Scope)
	if err != nil {
		return generation{}, err
	}
	if revision == nil || *revision != in.ExpectedSourceRevisionID || blank(content) {
		return generation{}, errors.New("章节正文已变化或为空，请先保存并刷新正文")
	}
	b, err := loadBook(conn, in.Scope)
	if err != nil {
		return generation{}, err
	}
	docs := b.documents()
	visual, err := workVisualProfile(conn, in.ProjectID, in.NovelWorkID)
	if err != nil {
		return generation{}, err
	}
	kind := stageKind(in.Stage)
	var prompt strings.Builder
	prompt.WriteString("## 本章原著\n" + content + "\n")
	prompt.WriteString("\n" + visualConstitutionContext(visual) + "\n")
	if in.Stage == kindSettings {
		if i := slices.IndexFunc(docs, func(d Document) bool { return d.Kind == kindSettings }); i >= 0 {
			prompt.WriteString("\n## 已有作品设定（更新基础）\n" + docs[i].Markdown + "\n" + settingsRefreshNote + "\n")
		}
	} else {
		for _, k := range []string{kindSettings, kindScript, kindStoryboard} {
			if k == kind {
				break
			}
			d, err := readyDocument(docs, k)
			if err != nil {
				return generation{}, err
			}
			prompt.WriteString("\n## 已保存的" + k + "\n" + d.Markdown + "\n")
		}
	}
	if in.Stage == kindScript {
		type previous struct{ markdown, chapter string }
		var scripts []previous
		err := querySQL(conn, `SELECT d.markdown,ch.id FROM novel_chapters ch JOIN comic_md_documents d ON d.chapter_id=ch.id AND d.kind='script' WHERE ch.novel_work_id=? AND ch.sequence_no<(SELECT sequence_no FROM novel_chapters WHERE id=?) ORDER BY ch.sequence_no`,
			func(stmt *sqlite.Stmt) { scripts = append(scripts, previous{stmt.ColumnText(0), stmt.ColumnText(1)}) },
			in.NovelWorkID, in.ChapterID)
		if err != nil {
			return generation{}, err
		}
		for _, p := range scripts {
			// Stale state is computed for the whole work, so the earlier
			// chapter's documents come from the same snapshot.
			if _, err := readyDocument(b.documentsFor(p.chapter), kindScript); err != nil {
				return generation{}, errors.New("前章人物状态对应的剧本需要更新，请先更新前章剧本")
			}
			if delta, ok := section(p.markdown, "人物锚点补充"); ok {
				prompt.WriteString("\n## 前章人物状态补充（只继承持续状态）\n" + delta + "\n")
			}
		}
	}
	prompt.WriteString(generationContinuity)
	prompt.WriteString("\n## 本次任务\n" + generationRule(in.Stage) + "\n只输出 Markdown 正文，不输出 JSON，不套代码围栏，不输出解释。")
	return generation{
		Scope:              in.Scope,
		Stage:              in.Stage,
		SourceRevision:     in.ExpectedSourceRevisionID,
		Dependencies:       b.dependencies(kind, nil),
		WorkVisualRevision: visual.Revision,
		Targets:            generationTargets(docs, kind),
		Prompt:             withCompliance(prompt.String()),
	}, nil
}

// applyGeneration saves generated Markdown if the job is still running and
// neither the source, the upstream documents, the visual profile nor the
// target documents changed since admission.
func applyGeneration(conn *sqlite.Conn, jobID string, g generation, output string) error {
	running, err := isJobRunning(conn, jobID)
	if err != nil {
		return err
	}
	if !running {
		return errors.New("任务已中断，不能覆盖已保存文档")
	}
	if err := keepOutput(conn, jobID, output); err != nil {
		return err
	}
	if err := rejectComplianceBlock(output); err != nil {
		return err
	}
	kind := stageKind(g.Stage)
	return immediate(conn, func() error {
		changed := errors.New("生成期间正文或上游资料已变化，结果已保留，请对照后手动保存或重新生成")
		revision, _, err := chapterSource(conn, g.Scope)
		if err != nil {
			return err
		}
		if revision == nil || *revision != g.SourceRevision {
			return changed
		}
		b, err := loadBook(conn, g.Scope)
		if err != nil {
			return err
		}
		if b.dependencies(kind, nil) != g.Dependencies {
			return changed
		}
		visual, err := workVisualProfile(conn, g.Scope.ProjectID, g.Scope.NovelWorkID)
		if err != nil {
			return err
		}
		if visual.Revision != g.WorkVisualRevision {
			return changed
		}
		current := b.documents()
		if !slices.EqualFunc(generationTargets(current, kind), g.Targets, genTarget.equal) {
			return errors.New("生成期间你已编辑文档，结果已保留，未覆盖你的修改")
		}
		type pageOutput struct {
			pageNo   *int64
			markdown string
		}
		var outputs []pageOutput
		if g.Stage == "page_prompts" {
			board, err := readyDocument(current, kindStoryboard)
			if err != nil {
				return err
			}
			ps := pages(output)
			if len(ps) == 0 || !slices.Equal(pageNumbers(output), pageNumbers(board.Markdown)) {
				return errors.New("生成页号与分镜不一致，原始 Markdown 已保留")
			}
			for _, p := range ps {
				outputs = append(outputs, pageOutput{ptr(p.no), p.body})
			}
		} else {
			outputs = []pageOutput{{nil, strings.TrimSpace(output)}}
		}
		for _, o := range outputs {
			if issues := validate(kind, o.pageNo, o.markdown); len(issues) > 0 {
				return fmt.Errorf("生成内容需要补全：%s。原始 Markdown 已保留", strings.Join(issues, "；"))
			}
		}
		for _, o := range outputs {
			in := SaveInput{Scope: g.Scope, Kind: kind, PageNo: o.pageNo, Markdown: o.markdown, AcknowledgeUpdates: true}
			if i := slices.IndexFunc(current, func(d Document) bool { return d.Kind == kind && optEqual(d.PageNo, o.pageNo) }); i >= 0 {
				in.ExpectedRevision = ptr(current[i].Revision)
				in.OptimizationInstruction = current[i].OptimizationInstruction
			}
			if _, err := saveDocument(conn, in); err != nil {
				return err
			}
		}
		return finishJob(conn, jobID, jobSucceeded, "Markdown 已生成并保存，可以编辑、复制或导出")
	})
}

// applyGenerationCompletion refuses a truncated completion: complete
// headings must not hide a cut-off document.
func applyGenerationCompletion(conn *sqlite.Conn, jobID string, g generation, c Completion) error {
	if !c.Completed {
		if err := keepOutput(conn, jobID, c.Content); err != nil {
			return err
		}
		return incompleteError(c.FinishReason)
	}
	return applyGeneration(conn, jobID, g, c.Content)
}

func incompleteError(finishReason string) error {
	reason := "文本响应未正常结束"
	switch finishReason {
	case "length":
		reason = "文本达到服务输出上限"
	case "content_filter":
		reason = "文本被服务中止"
	}
	return fmt.Errorf("%s，未覆盖已有文档。未完成的 Markdown 已保留，可复制后手动修正；不会自动重发请求。", reason)
}

func (s *Service) generate(ctx context.Context, in GenerateInput) (Job, error) {
	cfg := s.config.Get()
	if !cfg.LLMReady() || s.text == nil {
		return Job{}, errors.New("请先在设置中配置文本模型服务")
	}
	var g generation
	var job Job
	err := s.db.Tx(ctx, func(conn *sqlite.Conn) error {
		var err error
		if g, err = freezeGeneration(conn, in); err != nil {
			return err
		}
		job, err = insertJob(conn, in.Scope, in.Stage, mustJSON(g), 0)
		return err
	})
	if err != nil {
		return Job{}, err
	}
	s.start(job.ID, func(ctx context.Context) { s.runGeneration(ctx, cfg, job.ID, g) })
	return job, nil
}

func (s *Service) runGeneration(ctx context.Context, cfg config.Settings, jobID string, g generation) {
	completion, callErr := s.complete(ctx, cfg, "comic_markdown", generateSystem, g.Prompt)
	if callErr != nil && ctx.Err() != nil {
		s.interrupt(jobID)
		return
	}
	s.withJob(ctx, jobID, func(conn *sqlite.Conn) error {
		if callErr != nil {
			providerFailed(cfg, jobID, g.Stage, callErr)
			return errors.New("文本服务请求失败，请检查服务配置后重试；不会自动重新计费请求")
		}
		return applyGenerationCompletion(conn, jobID, g, completion)
	})
}

// --- optimization ----------------------------------------------------------------

type optimizationTarget struct {
	Document       Document `json:"document"`
	Dependencies   string   `json:"dependencies"`
	SourceRevision *string  `json:"source_revision"`
	Prompt         string   `json:"prompt"`
}

// optimization is a frozen optimization request (stored as the job snapshot).
type optimization struct {
	Scope              Scope                `json:"scope"`
	Instruction        string               `json:"instruction"`
	Targets            []optimizationTarget `json:"targets"`
	Guard              string               `json:"guard"`
	WorkVisualRevision int64                `json:"work_visual_revision"`
}

func compareStage(a, b Document) int {
	return cmp.Or(cmp.Compare(stageRank(a.Kind), stageRank(b.Kind)), cmp.Compare(orZero(a.PageNo), orZero(b.PageNo)))
}

// optimizationWorkspaceContext attaches every saved product of the chapter
// for consistency checks; only the target may be rewritten.
func optimizationWorkspaceContext(conn *sqlite.Conn, s Scope, docs []Document, targetID string) (string, error) {
	_, content, err := chapterSource(conn, s)
	if err != nil {
		return "", err
	}
	options, err := renderOptions(conn, s)
	if err != nil {
		return "", err
	}
	visual, err := workVisualProfile(conn, s.ProjectID, s.NovelWorkID)
	if err != nil {
		return "", err
	}
	injection := options.PromptInjection
	if blank(injection) {
		injection = "未设置"
	}
	var out strings.Builder
	fmt.Fprintf(&out, "# 同一小说漫画工作区的全部已保存产物（只用于一致性校验）\n## 当前章节正文\n%s\n\n## 本章生图注入规则 · 第%d版\n%s", content, options.Revision, injection)
	out.WriteString("\n\n" + visualConstitutionContext(visual))
	related := slices.DeleteFunc(slices.Clone(docs), func(d Document) bool { return d.ID == targetID })
	slices.SortStableFunc(related, compareStage)
	for _, d := range related {
		state := "当前已保存版本"
		switch {
		case d.OutOfPlan:
			state = "不在当前分镜，只作历史参考"
		case d.Stale:
			state = "需更新，不能覆盖上游事实"
		case len(d.Issues) > 0:
			state = "结构未完成，只作问题参考"
		}
		fmt.Fprintf(&out, "\n\n## %s · 第%d版 · %s\n%s", documentLabel(d.Kind, d.PageNo), d.Revision, state, d.Markdown)
	}
	type imageGroup struct{ pageNo, count, revision, createdAt int64 }
	var groups []imageGroup
	err = querySQL(conn, `SELECT i.page_no,COUNT(*),MAX(i.document_revision),MAX(i.created_at) FROM comic_md_images i JOIN comic_md_documents d ON d.id=i.document_id WHERE d.project_id=? AND d.novel_work_id=? AND d.chapter_id=? GROUP BY i.page_no ORDER BY i.page_no`,
		func(stmt *sqlite.Stmt) {
			groups = append(groups, imageGroup{stmt.ColumnInt64(0), stmt.ColumnInt64(1), stmt.ColumnInt64(2), stmt.ColumnInt64(3)})
		}, s.ProjectID, s.NovelWorkID, s.ChapterID)
	if err != nil {
		return "", err
	}
	out.WriteString("\n\n## 已生成漫画图片（只提供关联元数据，文本模型不能直接修改图片）")
	if len(groups) == 0 {
		out.WriteString("\n无")
	}
	for _, g := range groups {
		fmt.Fprintf(&out, "\n- 第%d页：%d 个图片版本；最新记录关联页 Prompt 第%d版；记录时间 %d", g.pageNo, g.count, g.revision, g.createdAt)
	}
	return out.String(), nil
}

// cascadeTargets is a single optimization's root plus its existing in-plan
// downstream documents (later pages for a page prompt), in pipeline order.
func cascadeTargets(docs []Document, root Document) []Document {
	var out []Document
	for _, d := range docs {
		if d.OutOfPlan {
			continue
		}
		if root.Kind == kindPagePrompt {
			if d.Kind == kindPagePrompt && orZero(d.PageNo) >= orZero(root.PageNo) {
				out = append(out, d)
			}
		} else if stageRank(d.Kind) >= stageRank(root.Kind) {
			out = append(out, d)
		}
	}
	slices.SortStableFunc(out, compareStage)
	return out
}

func freezeOptimization(conn *sqlite.Conn, in OptimizeInput) (optimization, error) {
	sourceRevision, _, err := chapterSource(conn, in.Scope)
	if err != nil {
		return optimization{}, err
	}
	if len(in.Targets) == 0 {
		return optimization{}, errors.New("请选择要优化的文档")
	}
	if blank(in.Instruction) {
		return optimization{}, errors.New("请填写本次优化要求")
	}
	if len(in.Instruction) > maxInstructionBytes {
		return optimization{}, errors.New("优化要求过长，请精简后提交")
	}
	b, err := loadBook(conn, in.Scope)
	if err != nil {
		return optimization{}, err
	}
	docs := b.documents()
	visual, err := workVisualProfile(conn, in.ProjectID, in.NovelWorkID)
	if err != nil {
		return optimization{}, err
	}
	var selected []Document
	if in.AllPages {
		if missing := b.plan().MissingPageNos; len(missing) > 0 {
			numbers := make([]string, len(missing))
			for i, p := range missing {
				numbers[i] = strconv.FormatInt(p, 10)
			}
			return optimization{}, fmt.Errorf("分镜仍缺少第%s页 Prompt，请先补齐后再优化全部页", strings.Join(numbers, "、"))
		}
		expected, submitted := map[string]bool{}, map[string]bool{}
		for _, d := range docs {
			if d.Kind == kindPagePrompt && !d.OutOfPlan {
				expected[d.ID] = true
			}
		}
		for _, t := range in.Targets {
			submitted[t.DocumentID] = true
		}
		if len(expected) == 0 || !maps.Equal(expected, submitted) || len(submitted) != len(in.Targets) {
			return optimization{}, errors.New("优化全部页必须提交当前分镜中的每一份页 Prompt，请刷新后重试")
		}
		for _, t := range in.Targets {
			if d, ok := findDocument(docs, t.DocumentID); ok {
				selected = append(selected, d)
			}
		}
	} else {
		if len(in.Targets) > 1 {
			return optimization{}, errors.New("多页优化必须明确选择“优化全部页”")
		}
		root, ok := findDocument(docs, in.Targets[0].DocumentID)
		if !ok {
			return optimization{}, errors.New("优化文档不属于当前小说章节")
		}
		selected = cascadeTargets(docs, root)
	}
	seen := map[string]bool{}
	targets := []optimizationTarget{}
	for _, d := range selected {
		if seen[d.ID] {
			return optimization{}, errors.New("同一文档不能重复提交优化")
		}
		seen[d.ID] = true
		if i := slices.IndexFunc(in.Targets, func(t PageInput) bool { return t.DocumentID == d.ID }); i >= 0 && in.Targets[i].Revision != d.Revision {
			return optimization{}, errors.New("待优化文档已有新版本，请先保存并刷新")
		}
		if blank(d.Markdown) {
			return optimization{}, errors.New("请先保存要优化的 Markdown 内容")
		}
		if d.OutOfPlan {
			return optimization{}, errors.New("该页已不在当前分镜计划内，请调整分镜或选择有效页")
		}
		upstream, err := b.context(d.Kind, d.PageNo)
		if err != nil {
			return optimization{}, err
		}
		workspace, err := optimizationWorkspaceContext(conn, in.Scope, docs, d.ID)
		if err != nil {
			return optimization{}, err
		}
		targets = append(targets, optimizationTarget{
			Document:       d,
			Dependencies:   b.dependencies(d.Kind, d.PageNo),
			SourceRevision: sourceRevision,
			Prompt:         upstream + "\n\n" + optimizationPrompt(d, in.Instruction, workspace),
		})
	}
	slices.SortStableFunc(targets, func(a, b optimizationTarget) int { return compareStage(a.Document, b.Document) })
	return optimization{Scope: in.Scope, Instruction: in.Instruction, Targets: targets, Guard: b.guard(), WorkVisualRevision: visual.Revision}, nil
}

// refreshOptimization rebuilds a cascade step's prompt from the latest
// upstream (which earlier steps of the same job may have rewritten).
func refreshOptimization(conn *sqlite.Conn, o optimization, initial optimizationTarget) (optimizationTarget, error) {
	b, err := loadBook(conn, o.Scope)
	if err != nil {
		return optimizationTarget{}, err
	}
	if b.guard() != o.Guard {
		return optimizationTarget{}, errors.New("关联资料已有外部编辑，后续页没有提交；已完成内容保留")
	}
	visual, err := workVisualProfile(conn, o.Scope.ProjectID, o.Scope.NovelWorkID)
	if err != nil {
		return optimizationTarget{}, err
	}
	if visual.Revision != o.WorkVisualRevision {
		return optimizationTarget{}, errors.New("优化期间作品视觉宪法或参考图已有变化，未覆盖你的修改；请刷新后重新提交")
	}
	target := initial
	docs := b.documents()
	// An earlier step may have shortened the storyboard; a page that left the
	// plan must not be paid for and saved as a new version.
	if slices.ContainsFunc(docs, func(d Document) bool { return d.ID == target.Document.ID && d.OutOfPlan }) {
		return optimizationTarget{}, fmt.Errorf("分镜调整后%s已不在当前分镜计划内，未再优化；此前完成的内容已保存", documentLabel(target.Document.Kind, target.Document.PageNo))
	}
	target.Dependencies = b.dependencies(target.Document.Kind, target.Document.PageNo)
	workspace, err := optimizationWorkspaceContext(conn, o.Scope, docs, target.Document.ID)
	if err != nil {
		return optimizationTarget{}, err
	}
	upstream, err := b.context(target.Document.Kind, target.Document.PageNo)
	if err != nil {
		return optimizationTarget{}, err
	}
	target.Prompt = upstream + "\n\n" + optimizationPrompt(target.Document, o.Instruction, workspace)
	return target, nil
}

func optimizationPreflight(conn *sqlite.Conn, jobID string, o optimization, target optimizationTarget) error {
	running, err := isJobRunning(conn, jobID)
	if err != nil {
		return err
	}
	if !running {
		return errors.New("优化任务已中断，后续文档没有提交")
	}
	b, err := loadBook(conn, o.Scope)
	if err != nil {
		return err
	}
	if b.guard() != o.Guard {
		return errors.New("优化期间正文或上游关联资料已有外部编辑，未覆盖你的修改；已完成内容与返回文本保留")
	}
	revision, _, err := chapterSource(conn, o.Scope)
	if err != nil {
		return err
	}
	if !strEqual(revision, target.SourceRevision) || b.dependencies(target.Document.Kind, target.Document.PageNo) != target.Dependencies {
		return errors.New("优化期间正文或上游资料已变化，已完成内容保留；请对照后重新提交")
	}
	docs, err := listDocuments(conn, o.Scope)
	if err != nil {
		return err
	}
	current, ok := findDocument(docs, target.Document.ID)
	if !ok {
		return errors.New("待优化文档已不可用")
	}
	if current.Revision != target.Document.Revision {
		return errors.New("优化期间你已保存新的文档版本，未覆盖你的修改；返回文本已保留供复制")
	}
	return nil
}

func applyOptimization(conn *sqlite.Conn, jobID string, o optimization, target optimizationTarget, c Completion) error {
	if err := keepOutput(conn, jobID, c.Content); err != nil {
		return err
	}
	if err := rejectComplianceBlock(c.Content); err != nil {
		return err
	}
	if !c.Completed {
		return errors.New("优化文本未正常结束，未覆盖文档。未完成的 Markdown 已保留，可复制后手动修正；不会自动重发请求。")
	}
	return immediate(conn, func() error {
		if err := optimizationPreflight(conn, jobID, o, target); err != nil {
			return err
		}
		d := target.Document
		md := strings.TrimSpace(c.Content)
		if issues := validate(d.Kind, d.PageNo, md); len(issues) > 0 {
			return fmt.Errorf("优化内容需要补全：%s。原始 Markdown 已保留", strings.Join(issues, "；"))
		}
		if _, err := saveDocument(conn, SaveInput{
			Scope: o.Scope, Kind: d.Kind, PageNo: d.PageNo, Markdown: md, OptimizationInstruction: o.Instruction,
			ExpectedRevision: ptr(d.Revision), AcknowledgeUpdates: true,
		}); err != nil {
			return err
		}
		return execSQL(conn, `UPDATE comic_md_jobs SET completed_pages=completed_pages+1,message=? WHERE id=? AND status='running'`,
			documentLabel(d.Kind, d.PageNo)+"已优化并保存", jobID)
	})
}

func (s *Service) optimize(ctx context.Context, in OptimizeInput) (Job, error) {
	cfg := s.config.Get()
	if !cfg.LLMReady() || s.text == nil {
		return Job{}, errors.New("请先在设置中配置文本模型服务")
	}
	var o optimization
	var job Job
	err := s.db.Tx(ctx, func(conn *sqlite.Conn) error {
		var err error
		if o, err = freezeOptimization(conn, in); err != nil {
			return err
		}
		job, err = insertJob(conn, in.Scope, "optimize", mustJSON(o), int64(len(o.Targets)))
		return err
	})
	if err != nil {
		return Job{}, err
	}
	s.start(job.ID, func(ctx context.Context) { s.runOptimization(ctx, cfg, job.ID, o) })
	return job, nil
}

func (s *Service) runOptimization(ctx context.Context, cfg config.Settings, jobID string, o optimization) {
	for _, initial := range slices.Clone(o.Targets) {
		if ctx.Err() != nil {
			s.interrupt(jobID)
			return
		}
		var target optimizationTarget
		ok := s.withJob(ctx, jobID, func(conn *sqlite.Conn) error {
			t, err := refreshOptimization(conn, o, initial)
			if err != nil {
				return err
			}
			if err := optimizationPreflight(conn, jobID, o, t); err != nil {
				return err
			}
			target = t
			return jobProgress(conn, jobID, "正在优化"+documentLabel(t.Document.Kind, t.Document.PageNo))
		})
		if !ok {
			return
		}
		completion, callErr := s.complete(ctx, cfg, "comic_markdown.optimize", optimizeSystem, target.Prompt)
		if callErr != nil && ctx.Err() != nil {
			s.interrupt(jobID)
			return
		}
		ok = s.withJob(ctx, jobID, func(conn *sqlite.Conn) error {
			if callErr != nil {
				providerFailed(cfg, jobID, "optimize", callErr)
				return errors.New("优化服务请求失败，已完成内容保留；请检查服务后手动重试")
			}
			if err := applyOptimization(conn, jobID, o, target, completion); err != nil {
				return err
			}
			b, err := loadBook(conn, o.Scope)
			if err != nil {
				return err
			}
			o.Guard = b.guard()
			return nil
		})
		if !ok {
			return
		}
	}
	s.finish(ctx, jobID, jobSucceeded, "所选文档已优化并保存，优化要求已随版本记录")
}

// --- shared worker plumbing ------------------------------------------------------

// complete sends one system+user completion; an empty answer is an error.
func (s *Service) complete(ctx context.Context, cfg config.Settings, operation, system, user string) (Completion, error) {
	c, err := s.text.Complete(ctx, TextRequest{
		Endpoint: completionEndpoint(cfg.LLMAPIURL), APIKey: cfg.LLMAPIKey, Model: cfg.LLMModel, Operation: operation,
		Messages: []ChatMessage{{Role: "system", Content: system}, {Role: "user", Content: user}},
	})
	if err == nil && c.Content == "" {
		err = errors.New("文本接口未返回内容")
	}
	return c, err
}

// withJob runs fn for a job; when fn fails the job is marked failed with
// fn's message. It reports whether the job may continue.
func (s *Service) withJob(ctx context.Context, jobID string, fn func(*sqlite.Conn) error) bool {
	var failure error
	err := s.db.With(context.WithoutCancel(ctx), func(conn *sqlite.Conn) error {
		if failure = fn(conn); failure != nil {
			return finishJob(conn, jobID, jobFailed, failure.Error())
		}
		return nil
	})
	if err != nil {
		slog.Warn("comic_markdown.job_update_failed", "jobId", jobID, "error", platform.ErrorText(err))
	}
	return failure == nil && err == nil
}

func (s *Service) finish(ctx context.Context, jobID, status, message string) {
	err := s.db.With(context.WithoutCancel(ctx), func(conn *sqlite.Conn) error {
		return finishJob(conn, jobID, status, message)
	})
	if err != nil {
		slog.Warn("comic_markdown.job_update_failed", "jobId", jobID, "error", platform.ErrorText(err))
	}
}

// interrupt ends a job stopped by shutdown the way startup recovery would.
func (s *Service) interrupt(jobID string) {
	s.finish(context.Background(), jobID, jobInterrupted, interruptedMessage)
}

// providerFailed logs a provider error with every configured key removed;
// the user sees a fixed message instead.
func providerFailed(cfg config.Settings, jobID, stage string, err error) {
	text := err.Error()
	for _, key := range []string{cfg.LLMAPIKey, cfg.ImageAPIKey, cfg.VideoAPIKey} {
		if key != "" {
			text = strings.ReplaceAll(text, key, "[REDACTED]")
		}
	}
	slog.Warn("comic_markdown.provider_failed", "jobId", jobID, "stage", stage, "error", platform.ErrorText(text))
}
