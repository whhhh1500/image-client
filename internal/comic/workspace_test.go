package comic

import (
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"zombiezen.com/go/sqlite"
)

func TestWorkVisualProfileIsWorkScopedCASVersionedAndEntersRenderContract(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	stylePath := f.writeFile("style.png", pngBytes)
	f.exec(`INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('style-ref','image',?,1,?)`, stylePath, `{"projectId":"project","params":{"novelWorkId":"work"}}`)
	input := WorkVisualSaveInput{
		ProjectID: "project", NovelWorkID: "work", ConstitutionMarkdown: "## 画面总则\n保持水墨线条与低饱和配色。",
		References: []WorkVisualReferenceInput{{AssetID: "style-ref", Role: "style", Weight: 0.8, SortOrder: 0, Note: " 只参考线条 "}},
	}
	saved, err := saveWorkVisualProfile(f.conn, input)
	if err != nil {
		t.Fatal(err)
	}
	if saved.Revision != 1 || len(saved.References) != 1 || saved.References[0].Note != "只参考线条" || !saved.References[0].FileAvailable {
		t.Fatalf("saved = %+v", saved)
	}
	_, err = saveWorkVisualProfile(f.conn, input)
	mustErr(t, err, "作品视觉宪法已有新版本")
	frozen, err := freezeVisual(f.conn, "project", "work")
	if err != nil {
		t.Fatal(err)
	}
	if frozen.profile.Revision != saved.Revision || len(frozen.files) != 1 {
		t.Fatalf("frozen = %+v", frozen)
	}
	root, refs, err := materializeReferences(f.paths.Assets(), frozen, "test-job")
	if err != nil {
		t.Fatal(err)
	}
	if len(refs) != 1 || refs[0].Path != filepath.Join(root, "00.png") || refs[0].Role != "style" || refs[0].Weight != 0.8 {
		t.Fatalf("refs = %+v", refs)
	}
	if root != filepath.Join(f.paths.Assets(), ".漫画画风任务快照", "test-job") {
		t.Fatal(root)
	}
	os.RemoveAll(root)

	g := f.freeze(s, "settings", "src1")
	if g.WorkVisualRevision != saved.Revision {
		t.Fatal(g.WorkVisualRevision)
	}
	mustContain(t, g.Prompt, "作品级视觉宪法")
	mustContain(t, g.Prompt, "水墨线条")
	mustContain(t, g.Prompt, "style-ref · style · 只参考线条")
	ctx, err := optimizationWorkspaceContext(f.conn, s, nil, "none")
	if err != nil {
		t.Fatal(err)
	}
	mustContain(t, ctx, "作品级视觉宪法")
	mustContain(t, ctx, "水墨线条")

	f.pipeline(s)
	page := f.put(s, kindPagePrompt, promptMD, nil)
	imagePath := f.writeFile("render.png", []byte("rendered image"))
	job := f.insertJob(s, "images", 1)
	f.insertImage("work-style-image", job, page, imagePath, map[string]any{
		"prompt_injection": "", "rerun_prompt_injection": "", "visual_profile_revision": frozen.profile.Revision, "visual_reference_snapshot": frozen.referencesJSON,
	})
	if f.workspace(s).Images[0].Stale {
		t.Fatal("fresh image must not be stale")
	}
	if err := os.WriteFile(stylePath, []byte("\x89PNG\r\n\x1a\nchanged style reference"), 0o644); err != nil {
		t.Fatal(err)
	}
	if !f.workspace(s).Images[0].Stale {
		t.Fatal("changed reference must make the image stale")
	}
	_, err = freezeVisual(f.conn, "project", "work")
	mustErr(t, err, "作品视觉参考图内容已变化")
	input.ExpectedRevision = 1
	input.ConstitutionMarkdown = "## 画面总则\n改为浓墨高反差。"
	updated, err := saveWorkVisualProfile(f.conn, input)
	if err != nil {
		t.Fatal(err)
	}
	if updated.Revision != 2 || !f.workspace(s).Images[0].Stale {
		t.Fatal("new profile revision must keep the image stale")
	}
}

func TestWorkVisualSaveValidatesReferences(t *testing.T) {
	f := newFixture(t)
	a := f.writeFile("a.png", pngBytes)
	b := f.writeFile("b.png", pngBytes) // same content as a
	c := f.writeFile("c.png", []byte("not an image"))
	big := f.writeFile("big.png", append(slices.Clone(pngBytes), make([]byte, maxReferenceBytes)...))
	meta := `{"projectId":"project","params":{"novelWorkId":"work"}}`
	f.exec(`INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('a','image',?,1,?),('b','image',?,1,?),('c','image',?,1,?),('big','image',?,1,?)`, a, meta, b, meta, c, meta, big, meta)
	f.exec(`INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('video','video',?,1,?),('foreign','image',?,1,'{"projectId":"other"}'),('other-work','image',?,1,'{"projectId":"project","params":{"comicGeneration":{"novelWorkId":"elsewhere"}}}')`, a, meta, a, a)
	ref := func(id string, order int64) WorkVisualReferenceInput {
		return WorkVisualReferenceInput{AssetID: id, Role: "style", Weight: 1, SortOrder: order}
	}
	for _, tc := range []struct {
		name string
		in   WorkVisualSaveInput
		want string
	}{
		{"foreign work", WorkVisualSaveInput{ProjectID: "other", NovelWorkID: "work"}, "未找到当前小说作品"},
		{"too long", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", ConstitutionMarkdown: strings.Repeat("a", maxInstructionBytes+1)}, "视觉宪法过长"},
		{"too many", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: make([]WorkVisualReferenceInput, 9)}, "最多支持 8 张"},
		{"stale revision", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", ExpectedRevision: 3}, "已有新版本"},
		{"duplicate asset", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("a", 0), ref("a", 1)}}, "不能重复引用同一图片"},
		{"bad role", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{{AssetID: "a", Role: "x", Weight: 1}}}, "role 无效"},
		{"zero weight", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{{AssetID: "a", Role: "style"}}}, "weight 必须在 0 到 1 之间"},
		{"duplicate order", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("a", 0), ref("c", 0)}}, "排序必须是互不重复的非负整数"},
		{"same content", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("a", 0), ref("b", 1)}}, "内容相同的图片"},
		{"not an image", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("c", 0)}}, "格式不受支持或文件已损坏"},
		{"too big", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("big", 0)}}, "不能超过 20 MiB"},
		{"missing", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("nope", 0)}}, "作品视觉参考图片不存在"},
		{"video", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("video", 0)}}, "必须是图片资产"},
		{"other project", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("foreign", 0)}}, "必须属于当前项目"},
		{"other work", WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{ref("other-work", 0)}}, "明确属于另一部小说"},
	} {
		_, err := saveWorkVisualProfile(f.conn, tc.in)
		if err == nil || !strings.Contains(err.Error(), tc.want) {
			t.Errorf("%s: err = %v, want %q", tc.name, err, tc.want)
		}
	}
}

func TestComplianceBlockFailsClosedWithoutOverwritingSavedDocument(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	saved := f.put(s, kindSettings, settingsMD, nil)
	g := f.freeze(s, "settings", "src1")
	job := f.insertJob(s, "settings", 0)
	blocked := "COMIC_COMPLIANCE_BLOCKED: 无法安全改写"
	mustErr(t, applyGeneration(f.conn, job.ID, g, blocked), "本次结果未保存")
	current := f.doc(s, byKind(kindSettings))
	if current.Revision != saved.Revision || current.Markdown != settingsMD {
		t.Fatal("blocked output overwrote the document")
	}
	if got := f.queryText(`SELECT output_markdown FROM comic_md_jobs WHERE id=?`, job.ID); got != blocked {
		t.Fatal(got)
	}
}

func TestStandalonePromptNeedsNoUpstreamDocuments(t *testing.T) {
	f := newFixture(t)
	d := f.put(f.scope, kindPagePrompt, promptMD, nil)
	docs, err := renderPages(f.conn, RenderInput{Scope: f.scope, Pages: []PageInput{{d.ID, d.Revision}}})
	if err != nil || docs[0].Markdown != promptMD {
		t.Fatal(err)
	}
}

func TestSaveCASPreservesAllRevisionsAndScope(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	d := f.put(s, kindSettings, settingsMD, nil)
	_, err := saveDocument(f.conn, SaveInput{Scope: s, Kind: kindSettings, Markdown: "bad", AcknowledgeUpdates: true})
	mustErr(t, err, "文档已有新版本，请刷新后对照保存；你的编辑尚未覆盖已有内容")
	f.put(s, kindSettings, settingsMD+"\n新增设定", rev(d.Revision))
	if n := f.queryInt(`SELECT count(*) FROM comic_md_revisions`); n != 2 {
		t.Fatal(n)
	}
	wrong := s
	wrong.ProjectID = "foreign"
	_, err = loadWorkspace(f.conn, wrong)
	mustErr(t, err, "未找到当前小说章节")
	for _, tc := range []struct {
		in   SaveInput
		want string
	}{
		{SaveInput{Scope: s, Kind: "poster"}, "未知文档类型"},
		{SaveInput{Scope: s, Kind: kindPagePrompt}, "请提供正整数页号"},
		{SaveInput{Scope: s, Kind: kindPagePrompt, PageNo: rev(0)}, "请提供正整数页号"},
		{SaveInput{Scope: s, Kind: kindScript, Markdown: strings.Repeat("a", maxMarkdownBytes+1)}, "Markdown 文档过大"},
		{SaveInput{Scope: s, Kind: kindScript, OptimizationInstruction: strings.Repeat("a", maxInstructionBytes+1)}, "优化要求过长，请精简后保存"},
	} {
		_, err := saveDocument(f.conn, tc.in)
		mustErr(t, err, tc.want)
	}
}

func TestEditsInvalidateDependentsAndLaterChaptersOnly(t *testing.T) {
	f := newFixture(t)
	s, s2, s3 := f.scope, f.chapter("ch2"), f.chapter("ch3")
	f.pipeline(s)
	p := f.put(s, kindPagePrompt, promptMD, nil)
	f.put(s2, kindScript, scriptMD, nil)
	f.put(s3, kindScript, scriptMD, nil)
	if slices.ContainsFunc(f.docs(s), func(d Document) bool { return d.Stale }) {
		t.Fatal("fresh pipeline is stale")
	}
	f.put(s, kindScript, scriptMD+"\n新增道具", rev(1))
	if !f.doc(s2, byKind(kindScript)).Stale {
		t.Fatal("later chapter script must be stale")
	}
	_, err := renderPages(f.conn, RenderInput{Scope: s, Pages: []PageInput{{p.ID, 1}}})
	mustErr(t, err, "第1页 Prompt 已变化、需要更新或缺少必要内容")
	if f.doc(s2, byKind(kindSettings)).Stale {
		t.Fatal("settings must not be stale")
	}
}

func TestSourceChangeMarksChapterDocsStaleButNotSharedSettings(t *testing.T) {
	f := newFixture(t)
	f.pipeline(f.scope)
	f.exec(`INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,source_kind,created_at) VALUES('src1new','ch1',2,'更新正文','h','paste',2)`)
	f.exec(`UPDATE novel_chapters SET current_revision_id='src1new' WHERE id='ch1'`)
	docs := f.docs(f.scope)
	if docs[0].Stale || !docs[1].Stale || !docs[2].Stale {
		t.Fatalf("stale = %v %v %v", docs[0].Stale, docs[1].Stale, docs[2].Stale)
	}
	if !slices.Contains(docs[1].StaleReasons, "章节正文已变化，可能受影响，请同步或核对") {
		t.Fatal(docs[1].StaleReasons)
	}
}

func TestLateModelOutputCannotOverwriteManualEdit(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	f.put(s, kindSettings, settingsMD, nil)
	g := f.freeze(s, "settings", "src1")
	job := f.insertJob(s, "settings", 0)
	f.put(s, kindSettings, settingsMD+"\n手工新增", rev(1))
	mustErr(t, applyGeneration(f.conn, job.ID, g, settingsMD), "已编辑")
	if !strings.Contains(f.docs(s)[0].Markdown, "手工新增") {
		t.Fatal("manual edit lost")
	}
	if out := f.jobs(s)[0].OutputMarkdown; out == nil || *out != settingsMD {
		t.Fatal("model output must be kept")
	}
}

func TestCompleteHeadingsCannotHideATruncatedCompletion(t *testing.T) {
	for _, reason := range []string{"length", "", "content_filter"} {
		f := newFixture(t)
		s := f.scope
		original := f.put(s, kindSettings, settingsMD, nil)
		g := f.freeze(s, "settings", "src1")
		job := f.insertJob(s, "settings", 0)
		partial := settingsMD + "\n本应继续输出更多人物，但内容中途终止"
		if len(validate(kindSettings, nil, partial)) != 0 {
			t.Fatal("partial looks valid by headings")
		}
		err := applyGenerationCompletion(f.conn, job.ID, g, Completion{Content: partial, FinishReason: reason})
		if err == nil {
			t.Fatal("truncated completion accepted")
		}
		if err := finishJob(f.conn, job.ID, jobFailed, err.Error()); err != nil {
			t.Fatal(err)
		}
		doc := f.docs(s)[0]
		if doc.Revision != original.Revision || doc.Markdown != settingsMD {
			t.Fatal("document overwritten")
		}
		j := f.jobs(s)[0]
		if j.Status != jobFailed || j.OutputMarkdown == nil || *j.OutputMarkdown != partial || !strings.Contains(*j.Message, "可复制后手动修正") {
			t.Fatalf("job = %+v", j)
		}
		want := map[string]string{"length": "文本达到服务输出上限", "": "文本响应未正常结束", "content_filter": "文本被服务中止"}[reason]
		if !strings.HasPrefix(*j.Message, want+"，未覆盖已有文档") {
			t.Fatal(*j.Message)
		}
	}
}

func TestInvalidOutputIsRetainedAndRunningJobDeduplicated(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	g := f.freeze(s, "settings", "src1")
	job := f.insertJob(s, "settings", 0)
	_, err := insertJob(f.conn, f.chapter("ch2"), "settings", "{}", 0)
	mustErr(t, err, "这部小说已有制作任务正在执行")
	mustErr(t, applyGeneration(f.conn, job.ID, g, "不完整输出"), "生成内容需要补全")
	if len(f.docs(s)) != 0 {
		t.Fatal("invalid output saved")
	}
	if out := f.jobs(s)[0].OutputMarkdown; out == nil || *out != "不完整输出" {
		t.Fatal("output not kept")
	}
}

func TestGenerationFreezeChecksStageSourceAndUpstream(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	for _, tc := range []struct{ stage, source, want string }{
		{"poster", "src1", "未知生成步骤"},
		{"settings", "src2", "章节正文已变化或为空"},
		{"script", "src1", "请先保存完整且最新的作品设定"},
		{"page_prompts", "src1", "请先保存完整且最新的作品设定"},
	} {
		_, err := freezeGeneration(f.conn, GenerateInput{Scope: s, Stage: tc.stage, ExpectedSourceRevisionID: tc.source})
		mustErr(t, err, tc.want)
	}
	f.put(s, kindSettings, settingsMD, nil)
	_, err := freezeGeneration(f.conn, GenerateInput{Scope: s, Stage: "storyboard", ExpectedSourceRevisionID: "src1"})
	mustErr(t, err, "请先保存完整且最新的本章剧本")
	g := f.freeze(s, "script", "src1")
	mustContain(t, g.Prompt, "## 本章原著\n第一章正文\n")
	mustContain(t, g.Prompt, "\n## 已保存的settings\n"+settingsMD+"\n")
	mustContain(t, g.Prompt, "\n## 本次任务\n输出本章完整可独立使用的剧本")
	if !strings.HasSuffix(g.Prompt, "只输出 Markdown 正文，不输出 JSON，不套代码围栏，不输出解释。\n\n---\n\n"+complianceRules) {
		t.Fatal("generation prompt tail")
	}
	encoded := mustJSON(g)
	for _, key := range []string{`"scope":{"projectId":"project"`, `"source_revision":"src1"`, `"work_visual_revision":0`, `"targets":[]`} {
		mustContain(t, encoded, key)
	}
}

func TestPageGenerationAndExportPreserveActualMarkdown(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	f.pipeline(s)
	g := f.freeze(s, "page_prompts", "src1")
	if strings.Contains(g.Prompt, "body_json") {
		t.Fatal("prompt mentions JSON body")
	}
	job := f.insertJob(s, "page_prompts", 0)
	if err := applyGeneration(f.conn, job.ID, g, promptMD); err != nil {
		t.Fatal(err)
	}
	d := f.doc(s, byKind(kindPagePrompt))
	if d.Markdown != promptMD || f.jobs(s)[0].Status != jobSucceeded {
		t.Fatal("page prompt not saved")
	}
	root := t.TempDir()
	out, err := exportDocuments(f.conn, ExportInput{Scope: s, DocumentIDs: &[]string{d.ID}}, root)
	if err != nil {
		t.Fatal(err)
	}
	data, _ := os.ReadFile(out.Files[0])
	if string(data) != promptMD || filepath.Base(out.Files[0]) != "第001页-Prompt.md" || !strings.HasPrefix(filepath.Base(out.Path), "漫画文字-") {
		t.Fatalf("export = %+v", out)
	}
	all, err := exportDocuments(f.conn, ExportInput{Scope: s}, root)
	if err != nil {
		t.Fatal(err)
	}
	names := make([]string, len(all.Files))
	for i, p := range all.Files {
		names[i] = filepath.Base(p)
	}
	if !slices.Equal(names, []string{"作品设定.md", "本章剧本.md", "本章分镜.md", "第001页-Prompt.md"}) {
		t.Fatal(names)
	}
	_, err = exportDocuments(f.conn, ExportInput{Scope: s, DocumentIDs: &[]string{"foreign"}}, root)
	mustErr(t, err, "所选文档不属于当前小说章节")
	_, err = exportDocuments(f.conn, ExportInput{Scope: s, DocumentIDs: &[]string{}}, root)
	mustErr(t, err, "还没有可以导出的 Markdown 文档")
}

func TestPageGenerationRejectsPageNumbersThatDifferFromTheStoryboard(t *testing.T) {
	f := newFixture(t)
	f.pipeline(f.scope)
	g := f.freeze(f.scope, "page_prompts", "src1")
	job := f.insertJob(f.scope, "page_prompts", 0)
	mustErr(t, applyGeneration(f.conn, job.ID, g, strings.ReplaceAll(promptMD, "第1页", "第2页")), "生成页号与分镜不一致")
}

func TestCatalogKeepsPersistedEffectivePromptAndLabelsImagesInChinese(t *testing.T) {
	f := newFixture(t)
	page := f.put(f.scope, kindPagePrompt, promptMD, nil)
	job := f.insertJob(f.scope, "images", 1)
	f.insertImage("catalog-image", job, page, f.writeFile("c.png", []byte("x")), map[string]any{"effective_prompt": "实际提交的完整 Prompt"})
	entries, err := catalogList(f.conn, "project")
	if err != nil {
		t.Fatal(err)
	}
	image := entries[slices.IndexFunc(entries, func(e CatalogEntry) bool { return e.Kind == "image" })]
	if image.Title != "第1章 · 漫画第1页" || *image.SourcePrompt != promptMD || *image.EffectivePrompt != "实际提交的完整 Prompt" || !image.PromptSnapshotComplete {
		t.Fatalf("image = %+v", image)
	}
	if image.SourceURI != "comic-md://project/work/ch1/image/catalog-image" {
		t.Fatal(image.SourceURI)
	}
	text := entries[slices.IndexFunc(entries, func(e CatalogEntry) bool { return e.Kind == "text" })]
	if text.Title != "第1章 · 漫画第1页 Prompt · 第1页" || text.SourceURI != "comic-md://project/work/ch1/document/"+page.ID+"@1" || text.Path != nil {
		t.Fatalf("text = %+v", text)
	}
}

func TestCatalogListsDocumentsAndImagesOfEveryChapterInAWork(t *testing.T) {
	f := newFixture(t)
	s2 := f.chapter("ch2")
	f.exec(`UPDATE novel_chapters SET title='雨夜' WHERE id='ch2'`)
	f.put(f.scope, kindSettings, settingsMD, nil)
	f.put(f.scope, kindPagePrompt, promptMD, nil)
	page2 := f.put(s2, kindPagePrompt, promptMD, nil)
	job := f.insertJob(s2, "images", 1)
	f.insertImage("catalog-ch2-image", job, page2, f.writeFile("c2.png", []byte("x")), map[string]any{"effective_prompt": "第二章实际 Prompt"})
	entries, err := catalogList(f.conn, "project")
	if err != nil {
		t.Fatal(err)
	}
	text := entries[slices.IndexFunc(entries, func(e CatalogEntry) bool { return e.Kind == "text" && e.DocumentID != nil && *e.DocumentID == page2.ID })]
	if *text.NovelChapterID != "ch2" || *text.ChapterTitle != "雨夜" || text.Title != "第2章《雨夜》 · 漫画第1页 Prompt · 第1页" {
		t.Fatalf("text = %+v", text)
	}
	image := entries[slices.IndexFunc(entries, func(e CatalogEntry) bool { return e.Kind == "image" })]
	if *image.NovelChapterID != "ch2" || *image.DocumentKind != kindPagePrompt || image.Stale != text.Stale {
		t.Fatalf("image = %+v", image)
	}
	settings := slices.IndexFunc(entries, func(e CatalogEntry) bool { return e.DocumentKind != nil && *e.DocumentKind == kindSettings })
	if settings < 0 || entries[settings].NovelChapterID != nil || !strings.Contains(entries[settings].SourceURI, "/work/document/") {
		t.Fatal("settings must be listed once as a work document")
	}
	count := 0
	for _, e := range entries {
		if e.DocumentKind != nil && *e.DocumentKind == kindSettings {
			count++
		}
	}
	if count != 1 || len(entries) != 4 {
		t.Fatalf("entries = %d settings = %d", len(entries), count)
	}
	for i := 1; i < len(entries); i++ {
		if entries[i-1].CreatedAt < entries[i].CreatedAt {
			t.Fatal("catalog must be newest first")
		}
	}
	if entries, err := catalogList(f.conn, "nobody"); err != nil || entries == nil || len(entries) != 0 {
		t.Fatal("unknown project must list nothing, as an empty array")
	}
}

func TestCatalogDoesNotFabricateAnEffectivePromptForIncompleteLegacySnapshot(t *testing.T) {
	f := newFixture(t)
	page := f.put(f.scope, kindPagePrompt, promptMD, nil)
	job := f.insertJob(f.scope, "images", 1)
	f.insertImage("legacy-image", job, page, f.writeFile("l.png", []byte("x")), nil)
	entries, err := catalogList(f.conn, "project")
	if err != nil {
		t.Fatal(err)
	}
	image := entries[slices.IndexFunc(entries, func(e CatalogEntry) bool { return e.Kind == "image" })]
	if *image.SourcePrompt != promptMD || image.EffectivePrompt != nil || image.PromptSnapshotComplete {
		t.Fatalf("image = %+v", image)
	}
}

func TestPreviousCharacterDeltaNeverContainsFutureChapter(t *testing.T) {
	f := newFixture(t)
	f.put(f.scope, kindSettings, settingsMD, nil)
	f.put(f.scope, kindScript, scriptMD, nil)
	f.put(f.chapter("ch3"), kindScript, strings.Replace(scriptMD, "左臂包扎，持续到康复。", "未来秘密", 1), nil)
	g := f.freeze(f.chapter("ch2"), "script", "src2")
	mustContain(t, g.Prompt, "\n## 前章人物状态补充（只继承持续状态）\n左臂包扎，持续到康复。\n")
	if strings.Contains(g.Prompt, "未来秘密") {
		t.Fatal("future chapter leaked")
	}
}

func TestSettingsRefreshKeepsExistingCharactersEvenWithIncompleteDraft(t *testing.T) {
	f := newFixture(t)
	f.put(f.scope, kindSettings, "## 人物锚点\n此前人物柳白，白发。", nil)
	g := f.freeze(f.chapter("ch2"), "settings", "src2")
	mustContain(t, g.Prompt, "此前人物柳白，白发。")
	mustContain(t, g.Prompt, "不要删掉本章未出场人物")
}

func TestLateImageKeepsRevisionAndIsStaleAfterEdit(t *testing.T) {
	f := newFixture(t)
	d := f.put(f.scope, kindPagePrompt, promptMD, nil)
	job := f.insertJob(f.scope, "images", 1)
	f.put(f.scope, kindPagePrompt, promptMD+"\n新增画面要求", rev(1))
	f.insertImage("img", job, d, "test.png", nil)
	w := f.workspace(f.scope)
	if w.Images[0].DocumentRevision != 1 || !w.Images[0].Stale || w.Documents[0].Revision != 2 || w.Images[0].FileAvailable {
		t.Fatalf("workspace = %+v", w.Images[0])
	}
}

func TestOptimizationInstructionIsVersionedAndGenerationPreservesIt(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	d, err := saveDocument(f.conn, SaveInput{Scope: s, Kind: kindSettings, Markdown: settingsMD, OptimizationInstruction: "保留人物，增加雨夜细节", AcknowledgeUpdates: true})
	if err != nil || d.OptimizationInstruction != "保留人物，增加雨夜细节" {
		t.Fatal(err)
	}
	g := f.freeze(s, "settings", "src1")
	if strings.Contains(g.Prompt, "保留人物，增加雨夜细节") {
		t.Fatal("generation must not replay the optimization instruction")
	}
	job := f.insertJob(s, "settings", 0)
	if err := applyGeneration(f.conn, job.ID, g, settingsMD); err != nil {
		t.Fatal(err)
	}
	current := f.docs(s)[0]
	if current.OptimizationInstruction != d.OptimizationInstruction {
		t.Fatal("generation dropped the instruction")
	}
	o, j := f.optimizeFixture(s, []Document{current}, "人物服装统一为蓝色")
	mustContain(t, o.Targets[0].Prompt, settingsMD)
	mustContain(t, o.Targets[0].Prompt, "## 用户修订要求\n人物服装统一为蓝色")
	if !strings.HasSuffix(o.Targets[0].Prompt, complianceRules) {
		t.Fatal("optimization prompt tail")
	}
	if err := applyOptimization(f.conn, j.ID, o, o.Targets[0], completed(settingsMD)); err != nil {
		t.Fatal(err)
	}
	type row struct {
		revision    int64
		instruction string
	}
	var history []row
	querySQL(f.conn, `SELECT revision,optimization_instruction FROM comic_md_revisions WHERE document_id=? ORDER BY revision`, func(stmt *sqlite.Stmt) {
		history = append(history, row{stmt.ColumnInt64(0), stmt.ColumnText(1)})
	}, d.ID)
	if !slices.Equal(history, []row{{1, d.OptimizationInstruction}, {2, d.OptimizationInstruction}, {3, "人物服装统一为蓝色"}}) {
		t.Fatal(history)
	}
	revisions, err := documentHistory(f.conn, HistoryInput{Scope: s, DocumentID: d.ID})
	if err != nil || len(revisions) != 3 || revisions[0].Revision != 3 || revisions[0].OptimizationInstruction != "人物服装统一为蓝色" {
		t.Fatal(err, revisions)
	}
	_, err = documentHistory(f.conn, HistoryInput{Scope: s, DocumentID: "foreign"})
	mustErr(t, err, "文档不属于当前章节")
}

func TestOptimizationReceivesAllSavedWorkspaceProductsButOnlyRewritesTarget(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	f.pipeline(s)
	page := f.pagePut(s, 1, promptMD, nil)
	job := f.insertJob(s, "images", 1)
	f.insertImage("workspace-image", job, page, "workspace.png", nil)
	if err := finishJob(f.conn, job.ID, jobSucceeded, "图片夹具完成"); err != nil {
		t.Fatal(err)
	}
	o, _ := f.optimizeFixture(s, []Document{f.doc(s, byKind(kindScript))}, "加强动作因果")
	prompt := o.Targets[0].Prompt
	for _, want := range []string{
		"# 同一小说漫画工作区的全部已保存产物", "## 当前章节正文\n第一章正文", "## 本章生图注入规则 · 第0版\n未设置",
		"## 作品设定 · 第1版", settingsMD, "## 分页分镜 · 第1版", boardMD, "## 第1页 Prompt · 第1版", promptMD,
		"第1页：1 个图片版本", "# 当前目标 Markdown（唯一允许改写）", "只改写“当前目标 Markdown”", "文档：本章剧本",
	} {
		mustContain(t, prompt, want)
	}
}

func TestOptimizationUpdatesDownstreamProductsInDependencyOrder(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	f.pipeline(s)
	f.pagePut(s, 1, promptMD, nil)
	o, job := f.optimizeFixture(s, []Document{f.doc(s, byKind(kindScript))}, "统一动作与人物状态")
	kinds := make([]string, len(o.Targets))
	for i, t := range o.Targets {
		kinds[i] = t.Document.Kind
	}
	if !slices.Equal(kinds, []string{kindScript, kindStoryboard, kindPagePrompt}) {
		t.Fatal(kinds)
	}
	for _, initial := range slices.Clone(o.Targets) {
		target, err := refreshOptimization(f.conn, o, initial)
		if err != nil {
			t.Fatal(err)
		}
		md := map[string]string{kindScript: scriptMD, kindStoryboard: boardMD, kindPagePrompt: promptMD}[target.Document.Kind]
		if err := applyOptimization(f.conn, job.ID, o, target, completed(md)); err != nil {
			t.Fatal(err)
		}
		o.Guard = f.book(s).guard()
	}
	for _, kind := range []string{kindScript, kindStoryboard, kindPagePrompt} {
		d := f.doc(s, byKind(kind))
		if d.Revision != 2 || d.Stale || d.OptimizationInstruction != "统一动作与人物状态" {
			t.Fatalf("%s = rev %d stale %v", kind, d.Revision, d.Stale)
		}
	}
}

func TestOptimizationChecksAllTargetsBeforeAcceptingAJob(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	d := f.put(s, kindPagePrompt, promptMD, nil)
	settings := f.put(s, kindSettings, settingsMD, nil)
	for _, tc := range []struct {
		targets []PageInput
		want    string
	}{
		{[]PageInput{{d.ID, 99}}, "待优化文档已有新版本"},
		{[]PageInput{{d.ID, 1}, {d.ID, 1}}, "多页优化必须明确选择"},
		{[]PageInput{{d.ID, 1}, {settings.ID, 1}}, "多页优化必须明确选择"},
		{[]PageInput{{"foreign-doc", 1}}, "优化文档不属于当前小说章节"},
		{nil, "请选择要优化的文档"},
	} {
		_, err := freezeOptimization(f.conn, OptimizeInput{Scope: s, Targets: tc.targets, Instruction: "优化对白"})
		mustErr(t, err, tc.want)
	}
	_, err := freezeOptimization(f.conn, OptimizeInput{Scope: s, Targets: []PageInput{{d.ID, 1}}, Instruction: "  "})
	mustErr(t, err, "请填写本次优化要求")
	_, err = freezeOptimization(f.conn, OptimizeInput{Scope: s, Targets: []PageInput{{d.ID, 1}}, Instruction: strings.Repeat("a", maxInstructionBytes+1)})
	mustErr(t, err, "优化要求过长，请精简后提交")
	if len(f.jobs(s)) != 0 {
		t.Fatal("no job may be created")
	}
	_, err = freezeOptimization(f.conn, OptimizeInput{Scope: f.chapter("ch2"), Targets: []PageInput{{d.ID, 1}}, Instruction: "优化对白"})
	mustErr(t, err, "优化文档不属于当前小说章节")
}

func TestOptimizeAllRequiresEveryPlannedPageAndRejectsMissingPrompts(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	board := boardMD + "\n\n" + strings.ReplaceAll(boardMD, "第1页", "第2页") + "\n\n" + strings.ReplaceAll(boardMD, "第1页", "第3页")
	f.put(s, kindStoryboard, board, nil)
	first := f.pagePut(s, 1, promptMD, nil)
	third := f.pagePut(s, 3, promptMD, nil)
	targets := []PageInput{{first.ID, first.Revision}, {third.ID, third.Revision}}
	_, err := freezeOptimization(f.conn, OptimizeInput{Scope: s, Targets: targets, Instruction: "统一人物状态", AllPages: true})
	mustErr(t, err, "分镜仍缺少第2页 Prompt")
	second := f.pagePut(s, 2, promptMD, nil)
	_, err = freezeOptimization(f.conn, OptimizeInput{Scope: s, Targets: targets, Instruction: "统一人物状态", AllPages: true})
	mustErr(t, err, "每一份页 Prompt")
	all := []PageInput{{first.ID, first.Revision}, {second.ID, second.Revision}, {third.ID, third.Revision}}
	if _, err := freezeOptimization(f.conn, OptimizeInput{Scope: s, Targets: all, Instruction: "统一人物状态", AllPages: true}); err != nil {
		t.Fatal(err)
	}
}

func TestBatchOptimizationPreservesFirstPageWhenSecondOutputFails(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	d1 := f.put(s, kindPagePrompt, promptMD, nil)
	d2 := f.pagePut(s, 2, promptMD, nil)
	o, job := f.optimizeFixture(s, []Document{d2, d1}, "优化雨夜光线")
	if *o.Targets[0].Document.PageNo != 1 {
		t.Fatal("targets must be page ordered")
	}
	if err := applyOptimization(f.conn, job.ID, o, o.Targets[0], completed(promptMD)); err != nil {
		t.Fatal(err)
	}
	raw := "# 第2页\n不完整的优化文本"
	// The snapshot guard was not advanced after the first page, so the second
	// step is refused before validation; its output is still kept.
	err := applyOptimization(f.conn, job.ID, o, o.Targets[1], completed(raw))
	mustErr(t, err, "已有外部编辑")
	finishJob(f.conn, job.ID, jobFailed, err.Error())
	docs := f.docs(s)
	if docs[0].Revision != 2 || docs[1].Revision != 1 || docs[0].OptimizationInstruction != "优化雨夜光线" {
		t.Fatal("first page must be kept")
	}
	j := f.jobs(s)[0]
	if j.Status != jobFailed || j.CompletedPages != 1 || j.TotalPages != 2 || *j.OutputMarkdown != raw {
		t.Fatalf("job = %+v", j)
	}
	if *j.Message != err.Error() {
		t.Fatal(*j.Message)
	}
}

func TestOptimizationKeepsPageIdentityButAllowsStoryboardPageCountChanges(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	d := f.put(s, kindPagePrompt, promptMD, nil)
	o, job := f.optimizeFixture(s, []Document{d}, "细化动作")
	if applyOptimization(f.conn, job.ID, o, o.Targets[0], completed(strings.ReplaceAll(promptMD, "第1页", "第2页"))) == nil {
		t.Fatal("page number change accepted")
	}
	finishJob(f.conn, job.ID, jobFailed, "wrong page")
	b := f.put(s, kindStoryboard, boardMD, nil)
	o, job = f.optimizeFixture(s, []Document{b}, "扩展为2页")
	two := boardMD + "\n\n" + strings.ReplaceAll(boardMD, "第1页", "第2页")
	if err := applyOptimization(f.conn, job.ID, o, o.Targets[0], completed(two)); err != nil {
		t.Fatal(err)
	}
	if got := f.queryText(`SELECT markdown FROM comic_md_revisions WHERE document_id=? AND revision=1`, b.ID); got != boardMD {
		t.Fatal("first revision lost")
	}
	if n := len(pages(f.doc(s, byKind(kindStoryboard)).Markdown)); n != 2 {
		t.Fatal(n)
	}
}

func TestTruncatedOrChangedDependencyOptimizationKeepsTheSavedDocument(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	d := f.put(s, kindPagePrompt, promptMD, nil)
	o, job := f.optimizeFixture(s, []Document{d}, "增加细节")
	mustErr(t, applyOptimization(f.conn, job.ID, o, o.Targets[0], Completion{Content: promptMD, FinishReason: "length"}), "未正常结束")
	f.exec(`INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,source_kind,created_at) VALUES('src1b','ch1',2,'改写正文','h','paste',2)`)
	f.exec(`UPDATE novel_chapters SET current_revision_id='src1b' WHERE id='ch1'`)
	mustErr(t, applyOptimization(f.conn, job.ID, o, o.Targets[0], completed(promptMD)), "正文或上游")
	if f.docs(s)[0].Revision != d.Revision {
		t.Fatal("document overwritten")
	}
}

func TestRenderRulesAreChapterScopedCASAndMarkOldInjectionImagesStale(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	d := f.put(s, kindPagePrompt, promptMD, nil)
	if got, _ := renderOptions(f.conn, s); got != (RenderOptions{}) {
		t.Fatal(got)
	}
	options, err := saveRenderOptions(f.conn, RenderOptionsSaveInput{Scope: s, PromptInjection: "服装改为红色"})
	if err != nil || options.Revision != 1 {
		t.Fatal(err)
	}
	_, err = saveRenderOptions(f.conn, RenderOptionsSaveInput{Scope: s, PromptInjection: "旧版覆盖"})
	mustErr(t, err, "漫画注入规则已有新版本，请刷新后对照保存")
	_, err = saveRenderOptions(f.conn, RenderOptionsSaveInput{Scope: s, PromptInjection: strings.Repeat("a", maxInstructionBytes+1), ExpectedRevision: 1})
	mustErr(t, err, "漫画注入规则过长")
	if got, _ := renderOptions(f.conn, f.chapter("ch2")); got.Revision != 0 {
		t.Fatal("render options must be chapter scoped")
	}
	job := f.insertJob(s, "images", 1)
	f.insertImage("injected", job, d, f.writeFile("i.png", []byte("image")), map[string]any{"prompt_injection": options.PromptInjection, "rerun_prompt_injection": "只调整这一页的雨伞颜色"})
	if f.workspace(s).Images[0].Stale {
		t.Fatal("image is current")
	}
	if _, err := saveRenderOptions(f.conn, RenderOptionsSaveInput{Scope: s, PromptInjection: "改为黑白", ExpectedRevision: 1}); err != nil {
		t.Fatal(err)
	}
	image := f.workspace(s).Images[0]
	if !image.Stale || image.PromptInjection != "服装改为红色" || image.RerunPromptInjection != "只调整这一页的雨伞颜色" {
		t.Fatalf("image = %+v", image)
	}
	actual := renderPrompt(promptMD, "", options.PromptInjection, "")
	if !strings.HasPrefix(actual, promptMD) || !strings.Contains(actual, "以本节为准") || !strings.Contains(actual, "服装改为红色") || !strings.HasSuffix(actual, complianceRules) {
		t.Fatal("render prompt layers")
	}
}

func TestRenderFreezesOptionsAtAdmissionButDoesNotRejectAuthorizedLaterPages(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	d := f.put(s, kindPagePrompt, promptMD, nil)
	options, err := saveRenderOptions(f.conn, RenderOptionsSaveInput{Scope: s, PromptInjection: "黑白线稿"})
	if err != nil {
		t.Fatal(err)
	}
	input := RenderInput{Scope: s, Pages: []PageInput{{d.ID, d.Revision}}, ExpectedRenderOptionsRevision: rev(0), RerunPromptInjection: "只把披风改为蓝色"}
	_, _, err = freezeRender(f.conn, input)
	mustErr(t, err, "漫画注入规则已有新版本，请刷新后再提交")
	input.ExpectedRenderOptionsRevision = rev(options.Revision)
	_, frozen, err := freezeRender(f.conn, input)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := saveRenderOptions(f.conn, RenderOptionsSaveInput{Scope: s, PromptInjection: "彩色油画", ExpectedRevision: 1}); err != nil {
		t.Fatal(err)
	}
	if _, err := renderPages(f.conn, input); err != nil {
		t.Fatal(err)
	}
	actual := renderPrompt(promptMD, "", frozen.PromptInjection, input.RerunPromptInjection)
	if !strings.Contains(actual, "黑白线稿") || !strings.Contains(actual, "只把披风改为蓝色") || strings.Contains(actual, "彩色油画") || !strings.HasSuffix(actual, complianceRules) {
		t.Fatal("frozen injection")
	}
	_, _, err = freezeRender(f.conn, input)
	mustErr(t, err, "已有新版本")
	input.ExpectedRenderOptionsRevision = nil
	if _, frozen, _ := freezeRender(f.conn, input); frozen.PromptInjection != "彩色油画" {
		t.Fatal(frozen)
	}
	second := f.pagePut(s, 2, promptMD, nil)
	input.Pages = append(input.Pages, PageInput{second.ID, second.Revision})
	_, _, err = freezeRender(f.conn, input)
	mustErr(t, err, "只能用于单页重画")
	for _, tc := range []struct {
		pages []PageInput
		want  string
	}{
		{nil, "请选择需要生成的页 Prompt"},
		{[]PageInput{{d.ID, 1}, {d.ID, 1}}, "同一页不能重复提交"},
		{[]PageInput{{"nope", 1}}, "页 Prompt 不属于当前章节"},
	} {
		_, err := renderPages(f.conn, RenderInput{Scope: s, Pages: tc.pages})
		mustErr(t, err, tc.want)
	}
	_, _, err = freezeRender(f.conn, RenderInput{Scope: s, Pages: []PageInput{{d.ID, 1}}, RerunPromptInjection: strings.Repeat("a", maxInstructionBytes+1)})
	mustErr(t, err, "本次重画 Prompt 注入过长")
}

func TestWorkspaceJSONKeepsArraysAndNulls(t *testing.T) {
	f := newFixture(t)
	w := f.workspace(f.scope)
	raw, err := json.Marshal(w)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{
		`"sourceRevisionId":"src1"`, `"sourceContent":"第一章正文"`, `"documents":[]`, `"jobs":[]`, `"images":[]`,
		`"textReady":false`, `"imageReady":false`, `"renderOptions":{"promptInjection":"","revision":0}`,
		`"workVisualProfile":{"constitutionMarkdown":"","revision":0,"references":[]}`,
		`"targets":[]`, `"missingPageNos":[]`, `"obsoletePageNos":[]`, `"blockedReason":null`, `"affectedChapters":[]`,
	} {
		mustContain(t, string(raw), want)
	}
	f.pipeline(f.scope)
	f.put(f.scope, kindPagePrompt, promptMD, nil)
	w = f.workspace(f.scope)
	if !w.TextReady || !w.ImageReady {
		t.Fatal("ready flags")
	}
	raw, _ = json.Marshal(w.Documents[0])
	for _, want := range []string{`"pageNo":null`, `"staleReasons":[]`, `"issues":[]`, `"outOfPlan":false`, `"optimizationInstruction":""`} {
		mustContain(t, string(raw), want)
	}
	raw, _ = json.Marshal(f.jobs(f.scope))
	if string(raw) != "[]" {
		t.Fatal(string(raw))
	}
}
