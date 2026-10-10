package comic

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"

	"zombiezen.com/go/sqlite"

	"imageclient/internal/config"
)

// Sync rewrites a chapter's stale documents and fills missing pages one at
// a time, in pipeline order, only within the plan the user confirmed. It
// never schedules images.

type syncSnapshot struct {
	Scope Scope    `json:"scope"`
	Plan  SyncPlan `json:"plan"`
	Guard string   `json:"guard"`
}

// syncStep is the next document a sync job rewrites (old is nil for a
// missing page).
type syncStep struct {
	kind   string
	pageNo *int64
	old    *Document
	prompt string
	guard  string
}

// prepareSync picks the next authorized update in pipeline order; a missing
// page comes before existing later pages. nil means the job is done.
func prepareSync(conn *sqlite.Conn, s Scope, guard string, allowed map[string]bool, pageSet map[int64]bool) (*syncStep, error) {
	b, err := loadBook(conn, s)
	if err != nil {
		return nil, err
	}
	if b.guard() != guard {
		return nil, errors.New("同步期间关联资料已有外部编辑，后续内容没有提交；已完成内容保留")
	}
	plan := b.plan()
	if plan.BlockedReason != nil {
		return nil, errors.New(*plan.BlockedReason)
	}
	docs := b.documents()
	var target *SyncTarget
	for i := range plan.Targets {
		if allowed[plan.Targets[i].DocumentID] {
			target = &plan.Targets[i]
			break
		}
	}
	var missing *int64
	for _, p := range plan.MissingPageNos {
		if pageSet[p] {
			missing = ptr(p)
			break
		}
	}
	step := &syncStep{guard: guard}
	switch {
	case missing != nil && (target == nil || (target.Kind == kindPagePrompt && optCompare(missing, target.PageNo) < 0)):
		step.kind, step.pageNo = kindPagePrompt, missing
	case target != nil:
		step.kind, step.pageNo = target.Kind, target.PageNo
		if d, ok := findDocument(docs, target.DocumentID); ok {
			step.old = &d
		}
	default:
		if len(plan.Targets) > 0 || len(plan.MissingPageNos) > 0 {
			return nil, errors.New("仍有超出本次更新范围的内容需要核对，请刷新计划后另行提交；已完成内容保留")
		}
		return nil, nil
	}
	template := Document{
		Kind: step.kind, PageNo: step.pageNo, Markdown: fmt.Sprintf("# 第%d页\n", orZero(step.pageNo)),
		StaleReasons: []string{}, Issues: []string{},
	}
	if step.old != nil {
		template = *step.old
	}
	workspace, err := optimizationWorkspaceContext(conn, s, docs, template.ID)
	if err != nil {
		return nil, err
	}
	upstream, err := b.context(step.kind, step.pageNo)
	if err != nil {
		return nil, err
	}
	step.prompt = upstream + "\n\n" + optimizationPrompt(template, syncInstruction, workspace)
	return step, nil
}

// applySync saves one synced document and returns the new guard. A sync
// never changes the storyboard's page count by itself.
func applySync(conn *sqlite.Conn, s Scope, jobID string, step syncStep, c Completion) (string, error) {
	if err := keepOutput(conn, jobID, c.Content); err != nil {
		return "", err
	}
	if err := rejectComplianceBlock(c.Content); err != nil {
		return "", err
	}
	if !c.Completed {
		return "", errors.New("同步文本未正常结束，未覆盖文档；返回 Markdown 已保留，可复制核对，不会自动重发")
	}
	var guard string
	err := immediate(conn, func() error {
		external := errors.New("同步期间关联资料已有外部编辑或任务已中断，未覆盖你的修改；返回文本已保留")
		running, err := isJobRunning(conn, jobID)
		if err != nil {
			return err
		}
		if !running {
			return external
		}
		b, err := loadBook(conn, s)
		if err != nil {
			return err
		}
		if b.guard() != step.guard {
			return external
		}
		md := strings.TrimSpace(c.Content)
		if issues := validate(step.kind, step.pageNo, md); len(issues) > 0 {
			return fmt.Errorf("同步内容需要补全：%s。原始 Markdown 已保留", strings.Join(issues, "；"))
		}
		if step.kind == kindStoryboard {
			var before []int64
			if step.old != nil {
				before = pageNumbers(step.old.Markdown)
			}
			if !slices.Equal(before, pageNumbers(md)) {
				return errors.New("联动更新不会自动改变分页数量；请先单独修改或优化分页分镜，核对后再补齐页 Prompt。原始 Markdown 已保留")
			}
		}
		in := SaveInput{Scope: s, Kind: step.kind, PageNo: step.pageNo, Markdown: md, AcknowledgeUpdates: true}
		if step.old != nil {
			in.OptimizationInstruction = step.old.OptimizationInstruction
			in.ExpectedRevision = ptr(step.old.Revision)
		}
		if _, err := saveDocument(conn, in); err != nil {
			return err
		}
		if err := execSQL(conn, `UPDATE comic_md_jobs SET completed_pages=completed_pages+1,message='当前文档已同步并保存' WHERE id=?`, jobID); err != nil {
			return err
		}
		after, err := loadBook(conn, s)
		if err != nil {
			return err
		}
		guard = after.guard()
		return nil
	})
	return guard, err
}

func (s *Service) sync(ctx context.Context, in SyncInput) (Job, error) {
	cfg := s.config.Get()
	if !cfg.LLMReady() || s.text == nil {
		return Job{}, errors.New("请先在设置中配置文本模型服务")
	}
	var snapshot syncSnapshot
	var job Job
	err := s.db.Tx(ctx, func(conn *sqlite.Conn) error {
		b, err := loadBook(conn, in.Scope)
		if err != nil {
			return err
		}
		plan := b.plan()
		if plan.Fingerprint != in.ExpectedPlanFingerprint {
			return errors.New("关联更新计划已变化，请刷新后重新核对提交")
		}
		if plan.BlockedReason != nil {
			return errors.New(*plan.BlockedReason)
		}
		if len(plan.Targets) == 0 && len(plan.MissingPageNos) == 0 {
			return errors.New("本章没有待同步的文字或缺页")
		}
		snapshot = syncSnapshot{Scope: in.Scope, Plan: plan, Guard: b.guard()}
		job, err = insertJob(conn, in.Scope, "sync", mustJSON(snapshot), int64(len(plan.Targets)+len(plan.MissingPageNos)))
		return err
	})
	if err != nil {
		return Job{}, err
	}
	s.start(job.ID, func(ctx context.Context) { s.runSync(ctx, cfg, job.ID, snapshot) })
	return job, nil
}

func (s *Service) runSync(ctx context.Context, cfg config.Settings, jobID string, snapshot syncSnapshot) {
	scope, guard := snapshot.Scope, snapshot.Guard
	allowed, pageSet := map[string]bool{}, map[int64]bool{}
	var first *int64
	for _, t := range snapshot.Plan.Targets {
		allowed[t.DocumentID] = true
		if t.PageNo != nil && (first == nil || *t.PageNo < *first) {
			first = ptr(*t.PageNo)
		}
	}
	for _, p := range snapshot.Plan.MissingPageNos {
		pageSet[p] = true
		if first == nil || p < *first {
			first = ptr(p)
		}
	}
	// A submitted page edit or missing page also authorizes updating the
	// chapter's later pages that inherit its character state.
	if first != nil {
		_ = s.db.With(context.WithoutCancel(ctx), func(conn *sqlite.Conn) error {
			b, err := loadBook(conn, scope)
			if err != nil {
				return err
			}
			for _, d := range b.documents() {
				if d.Kind == kindPagePrompt && !d.OutOfPlan && d.PageNo != nil && *d.PageNo >= *first {
					allowed[d.ID] = true
				}
			}
			return nil
		})
	}
	for {
		if ctx.Err() != nil {
			s.interrupt(jobID)
			return
		}
		var step *syncStep
		ok := s.withJob(ctx, jobID, func(conn *sqlite.Conn) error {
			running, err := isJobRunning(conn, jobID)
			if err != nil {
				return err
			}
			if !running {
				return errors.New("同步任务已中断，后续文档没有提交")
			}
			if step, err = prepareSync(conn, scope, guard, allowed, pageSet); err != nil || step == nil {
				return err
			}
			return jobProgress(conn, jobID, "正在同步"+documentLabel(step.kind, step.pageNo))
		})
		if !ok {
			return
		}
		if step == nil {
			s.finish(ctx, jobID, jobSucceeded, "本章关联文字已同步，历史版本已保留；漫画需要你另行生成")
			return
		}
		completion, callErr := s.complete(ctx, cfg, "comic_markdown.sync", syncSystem, step.prompt)
		if callErr != nil && ctx.Err() != nil {
			s.interrupt(jobID)
			return
		}
		ok = s.withJob(ctx, jobID, func(conn *sqlite.Conn) error {
			if callErr != nil {
				providerFailed(cfg, jobID, "sync", callErr)
				return errors.New("同步服务请求失败，已完成内容保留；请检查后手动重试")
			}
			next, err := applySync(conn, scope, jobID, *step, completion)
			if err != nil {
				return err
			}
			guard = next
			// Only this job's own storyboard revision may extend its
			// authorized page set.
			if step.kind == kindStoryboard {
				b, err := loadBook(conn, scope)
				if err != nil {
					return err
				}
				plan := b.plan()
				for _, p := range plan.MissingPageNos {
					pageSet[p] = true
				}
				for _, t := range plan.Targets {
					if t.Kind == kindPagePrompt {
						allowed[t.DocumentID] = true
					}
				}
				if err := setSyncTotal(conn, jobID, int64(len(plan.Targets)+len(plan.MissingPageNos))); err != nil {
					return err
				}
			}
			b, err := loadBook(conn, scope)
			if err != nil {
				return err
			}
			remaining := b.plan()
			pending := 0
			for _, t := range remaining.Targets {
				if allowed[t.DocumentID] {
					pending++
				}
			}
			for _, p := range remaining.MissingPageNos {
				if pageSet[p] {
					pending++
				}
			}
			return setSyncTotal(conn, jobID, int64(pending))
		})
		if !ok {
			return
		}
	}
}

// setSyncTotal sets the job total to the completed count plus what remains.
func setSyncTotal(conn *sqlite.Conn, jobID string, remaining int64) error {
	var completed int64
	if _, err := queryOne(conn, `SELECT completed_pages FROM comic_md_jobs WHERE id=?`,
		func(stmt *sqlite.Stmt) { completed = stmt.ColumnInt64(0) }, jobID); err != nil {
		return err
	}
	return execSQL(conn, `UPDATE comic_md_jobs SET total_pages=? WHERE id=?`, completed+remaining, jobID)
}
