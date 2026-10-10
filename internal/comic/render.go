package comic

import (
	"context"
	"errors"
	"fmt"
	"os"
	"slices"

	"zombiezen.com/go/sqlite"

	"imageclient/internal/config"
)

// Rendering turns page prompts into comic pages with the chapter injection
// and work visual profile frozen at admission.

// renderPages resolves the submitted pages, each of which must be the
// current, complete, in-plan revision of a page prompt of the chapter.
func renderPages(conn *sqlite.Conn, in RenderInput) ([]Document, error) {
	if _, _, err := chapterSource(conn, in.Scope); err != nil {
		return nil, err
	}
	if len(in.Pages) == 0 {
		return nil, errors.New("请选择需要生成的页 Prompt")
	}
	docs, err := listDocuments(conn, in.Scope)
	if err != nil {
		return nil, err
	}
	seen := map[string]bool{}
	out := make([]Document, 0, len(in.Pages))
	for _, p := range in.Pages {
		if seen[p.DocumentID] {
			return nil, errors.New("同一页不能重复提交")
		}
		seen[p.DocumentID] = true
		i := slices.IndexFunc(docs, func(d Document) bool { return d.ID == p.DocumentID && d.Kind == kindPagePrompt })
		if i < 0 {
			return nil, errors.New("页 Prompt 不属于当前章节")
		}
		d := docs[i]
		if d.Revision != p.Revision || d.Stale || d.OutOfPlan || len(d.Issues) > 0 {
			return nil, fmt.Errorf("第%d页 Prompt 已变化、需要更新或缺少必要内容，请先保存完整的当前版本", orZero(d.PageNo))
		}
		out = append(out, d)
	}
	slices.SortStableFunc(out, func(a, b Document) int { return optCompare(a.PageNo, b.PageNo) })
	return out, nil
}

// freezeRender admits a render with the chapter injection current at
// admission; later edits to the injection do not change the job.
func freezeRender(conn *sqlite.Conn, in RenderInput) ([]Document, RenderOptions, error) {
	docs, err := renderPages(conn, in)
	if err != nil {
		return nil, RenderOptions{}, err
	}
	if len(in.RerunPromptInjection) > maxInstructionBytes {
		return nil, RenderOptions{}, errors.New("本次重画 Prompt 注入过长，请精简后重试")
	}
	if !blank(in.RerunPromptInjection) && len(docs) != 1 {
		return nil, RenderOptions{}, errors.New("本次重画 Prompt 注入只能用于单页重画")
	}
	options, err := renderOptions(conn, in.Scope)
	if err != nil {
		return nil, RenderOptions{}, err
	}
	if in.ExpectedRenderOptionsRevision != nil && *in.ExpectedRenderOptionsRevision != options.Revision {
		return nil, RenderOptions{}, errors.New("漫画注入规则已有新版本，请刷新后再提交")
	}
	return docs, options, nil
}

// renderJob is everything a render job needs after admission.
type renderJob struct {
	cfg        config.Settings
	outputDir  string
	jobID      string
	input      RenderInput
	pages      []Document
	options    RenderOptions
	visual     frozenVisual
	refsRoot   string
	references []ImageReference
}

func (s *Service) render(ctx context.Context, in RenderInput) (Job, error) {
	cfg := s.config.Get()
	if !cfg.ImageReady() || s.images == nil {
		return Job{}, errors.New("请先在设置中配置图像服务")
	}
	r := renderJob{cfg: cfg, outputDir: s.config.OutputPath(cfg), input: in}
	var job Job
	err := s.db.Tx(ctx, func(conn *sqlite.Conn) error {
		var err error
		if r.pages, r.options, err = freezeRender(conn, in); err != nil {
			return err
		}
		if r.visual, err = freezeVisual(conn, in.ProjectID, in.NovelWorkID); err != nil {
			return err
		}
		snapshot := mustJSON(map[string]any{
			"scope": in.Scope, "pages": r.pages, "renderOptions": r.options,
			"workVisualProfile": r.visual.profile, "rerunPromptInjection": in.RerunPromptInjection,
		})
		job, err = insertJob(conn, in.Scope, "images", snapshot, int64(len(r.pages)))
		return err
	})
	if err != nil {
		return Job{}, err
	}
	r.jobID = job.ID
	r.refsRoot, r.references, err = materializeReferences(s.paths.Assets(), r.visual, job.ID)
	if err != nil {
		s.finish(ctx, job.ID, jobFailed, err.Error())
		return Job{}, err
	}
	s.start(job.ID, func(ctx context.Context) { s.runRender(ctx, r) })
	return job, nil
}

func (s *Service) runRender(ctx context.Context, r renderJob) {
	defer os.RemoveAll(r.refsRoot)
	for _, d := range r.pages {
		if ctx.Err() != nil {
			s.interrupt(r.jobID)
			return
		}
		ok := s.withJob(ctx, r.jobID, func(conn *sqlite.Conn) error {
			running, err := isJobRunning(conn, r.jobID)
			if err != nil {
				return err
			}
			if !running {
				return errors.New("任务已中断，后续页面没有提交")
			}
			_, err = renderPages(conn, RenderInput{
				Scope: r.input.Scope, RerunPromptInjection: r.input.RerunPromptInjection,
				Pages: []PageInput{{DocumentID: d.ID, Revision: d.Revision}},
			})
			return err
		})
		if !ok {
			return
		}
		effective := renderPrompt(d.Markdown, r.visual.profile.ConstitutionMarkdown, r.options.PromptInjection, r.input.RerunPromptInjection)
		assets, genErr := s.images.Generate(ctx, ImageRequest{
			Settings: r.cfg, OutputDir: r.outputDir, Prompt: effective,
			References: slices.Clone(r.references), Size: "1024x1536", Quality: "high",
		})
		if genErr != nil && ctx.Err() != nil {
			s.interrupt(r.jobID)
			return
		}
		ok = s.withJob(ctx, r.jobID, func(conn *sqlite.Conn) error {
			if genErr != nil {
				providerFailed(r.cfg, r.jobID, "images", genErr)
				return errors.New("图像服务请求失败，已生成页面已保留；请检查服务后重新选择未完成页面")
			}
			if len(assets) == 0 {
				return errors.New("图像服务没有返回图片，已生成页面已保留")
			}
			return immediate(conn, func() error {
				for _, a := range assets {
					if err := execSQL(conn, `INSERT INTO comic_md_images(id,job_id,document_id,document_revision,page_no,path,created_at,prompt_injection,rerun_prompt_injection,visual_profile_revision,visual_reference_snapshot,effective_prompt) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)`,
						newID(), r.jobID, d.ID, d.Revision, orZero(d.PageNo), a.Path, nowMillis(), r.options.PromptInjection,
						r.input.RerunPromptInjection, r.visual.profile.Revision, r.visual.referencesJSON, effective); err != nil {
						return err
					}
				}
				return execSQL(conn, `UPDATE comic_md_jobs SET completed_pages=completed_pages+1,message=? WHERE id=?`,
					fmt.Sprintf("第%d页已生成", orZero(d.PageNo)), r.jobID)
			})
		})
		if !ok {
			// The provider already wrote these files; without the cleanup a
			// failed insert would leave untracked images on disk forever.
			for _, a := range assets {
				os.Remove(a.Path)
			}
			return
		}
	}
	os.RemoveAll(r.refsRoot) // before the job reads as finished
	s.finish(ctx, r.jobID, jobSucceeded, "所选漫画页面已生成")
}
