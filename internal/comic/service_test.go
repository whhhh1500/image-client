package comic

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/config"
	"imageclient/internal/ipc"
)

func TestRegisterExposesTheFrontendCommands(t *testing.T) {
	f := newFixture(t)
	s := f.newService(nil, nil)
	r := ipc.NewRouter()
	s.Register(r)
	want := []string{
		"comic_md_catalog_list", "comic_md_document_history", "comic_md_document_save", "comic_md_export", "comic_md_generate",
		"comic_md_optimize", "comic_md_render", "comic_md_render_options_save", "comic_md_sync", "comic_md_work_visual_extract",
		"comic_md_work_visual_import", "comic_md_work_visual_save", "comic_md_workspace_get",
	}
	if got := r.Names(); !slices.Equal(got, want) {
		t.Fatal(got)
	}
	ctx := context.Background()
	saved, err := r.Invoke(ctx, "comic_md_document_save", json.RawMessage(`{"input":{"projectId":"project","novelWorkId":"work","chapterId":"ch1","kind":"settings","markdown":"`+strings.ReplaceAll(settingsMD, "\n", `\n`)+`","expectedRevision":null}}`))
	if err != nil || saved.(Document).Revision != 1 {
		t.Fatal(err)
	}
	result, err := r.Invoke(ctx, "comic_md_workspace_get", json.RawMessage(`{"input":{"projectId":"project","novelWorkId":"work","chapterId":"ch1"}}`))
	if err != nil || len(result.(Workspace).Documents) != 1 {
		t.Fatal(err)
	}
	_, err = r.Invoke(ctx, "comic_md_render_options_save", json.RawMessage(`{"input":{"projectId":"project","novelWorkId":"work","chapterId":"ch1","promptInjection":"x","expectedRevision":0}}`))
	if err != nil {
		t.Fatal(err)
	}
	exported, err := r.Invoke(ctx, "comic_md_export", json.RawMessage(`{"input":{"projectId":"project","novelWorkId":"work","chapterId":"ch1"}}`))
	if err != nil || !strings.HasPrefix(exported.(ExportResult).Path, filepath.Join(f.paths.Root, "exports", "漫画文字-")) {
		t.Fatal(err, exported)
	}
	catalog, err := r.Invoke(ctx, "comic_md_catalog_list", json.RawMessage(`{"input":{"projectId":"project"}}`))
	if err != nil || len(catalog.([]CatalogEntry)) != 1 {
		t.Fatal(err)
	}
}

func TestJobsRequireConfiguredProviders(t *testing.T) {
	f := newFixture(t)
	s := f.newService(&fakeText{}, &fakeImages{})
	if err := s.config.Replace(config.Settings{}); err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	_, err := s.generate(ctx, GenerateInput{Scope: f.scope, Stage: "settings", ExpectedSourceRevisionID: "src1"})
	mustErr(t, err, "请先在设置中配置文本模型服务")
	_, err = s.optimize(ctx, OptimizeInput{Scope: f.scope})
	mustErr(t, err, "请先在设置中配置文本模型服务")
	_, err = s.sync(ctx, SyncInput{Scope: f.scope})
	mustErr(t, err, "请先在设置中配置文本模型服务")
	_, err = s.render(ctx, RenderInput{Scope: f.scope})
	mustErr(t, err, "请先在设置中配置图像服务")
	_, err = s.extractConstitution(ctx, WorkVisualExtractInput{ProjectID: "project", NovelWorkID: "work"})
	mustErr(t, err, "请先在设置中配置支持图片输入的文本模型服务")
	_, err = s.extractConstitution(ctx, WorkVisualExtractInput{Instruction: strings.Repeat("a", maxInstructionBytes+1)})
	mustErr(t, err, "视觉宪法提取要求过长")
}

func TestGenerateRunsInTheBackgroundAndSavesTheDocument(t *testing.T) {
	f := newFixture(t)
	text := &fakeText{reply: func(context.Context, TextRequest) (Completion, error) {
		return completed("\n" + settingsMD + "\n"), nil
	}}
	s := f.newService(text, nil)
	job, err := s.generate(context.Background(), GenerateInput{Scope: f.scope, Stage: "settings", ExpectedSourceRevisionID: "src1"})
	if err != nil {
		t.Fatal(err)
	}
	if job.Status != jobRunning || *job.Message != "任务已提交，可以离开页面后再回来查看" || job.Kind != "settings" {
		t.Fatalf("job = %+v", job)
	}
	done := f.waitJob(job.ID)
	if done.Status != jobSucceeded || *done.Message != "Markdown 已生成并保存，可以编辑、复制或导出" {
		t.Fatalf("job = %+v", done)
	}
	if d := f.doc(f.scope, byKind(kindSettings)); d.Markdown != settingsMD {
		t.Fatal(d.Markdown)
	}
	req := text.seen()[0]
	if req.Endpoint != "https://llm.test/v1/chat/completions" || req.APIKey != "llm-secret-key" || req.Model != "text-model" || req.Operation != "comic_markdown" {
		t.Fatalf("request = %+v", req)
	}
	if len(req.Messages) != 2 || req.Messages[0].Content != generateSystem || !strings.HasSuffix(req.Messages[1].Content.(string), complianceRules) {
		t.Fatal("messages")
	}
	var snapshot generation
	if err := json.Unmarshal([]byte(f.queryText(`SELECT input_snapshot FROM comic_md_jobs WHERE id=?`, job.ID)), &snapshot); err != nil || snapshot.Prompt != req.Messages[1].Content {
		t.Fatal("job snapshot must hold the frozen prompt")
	}
}

func TestProviderFailureFailsTheJobWithoutLeakingDetails(t *testing.T) {
	f := newFixture(t)
	text := &fakeText{reply: func(context.Context, TextRequest) (Completion, error) {
		return Completion{}, errors.New("401 bad key llm-secret-key")
	}}
	s := f.newService(text, nil)
	job, err := s.generate(context.Background(), GenerateInput{Scope: f.scope, Stage: "settings", ExpectedSourceRevisionID: "src1"})
	if err != nil {
		t.Fatal(err)
	}
	if done := f.waitJob(job.ID); done.Status != jobFailed || *done.Message != "文本服务请求失败，请检查服务配置后重试；不会自动重新计费请求" {
		t.Fatalf("job = %+v", done)
	}
	text.reply = func(context.Context, TextRequest) (Completion, error) { return Completion{Completed: true}, nil }
	job, err = s.generate(context.Background(), GenerateInput{Scope: f.scope, Stage: "settings", ExpectedSourceRevisionID: "src1"})
	if err != nil {
		t.Fatal(err)
	}
	if done := f.waitJob(job.ID); done.Status != jobFailed {
		t.Fatal("an empty completion is a provider failure")
	}
}

func TestOptimizeJobCascadesThroughDownstreamDocuments(t *testing.T) {
	f := newFixture(t)
	f.pipeline(f.scope)
	f.pagePut(f.scope, 1, promptMD, nil)
	text := &fakeText{reply: func(_ context.Context, req TextRequest) (Completion, error) {
		prompt := req.Messages[1].Content.(string)
		switch {
		case strings.Contains(prompt, "文档：本章剧本"):
			return completed(scriptMD), nil
		case strings.Contains(prompt, "文档：分页分镜"):
			return completed(boardMD), nil
		}
		return completed(promptMD), nil
	}}
	s := f.newService(text, nil)
	script := f.doc(f.scope, byKind(kindScript))
	job, err := s.optimize(context.Background(), OptimizeInput{Scope: f.scope, Targets: []PageInput{{script.ID, script.Revision}}, Instruction: "统一动作"})
	if err != nil || job.TotalPages != 3 {
		t.Fatal(err, job)
	}
	done := f.waitJob(job.ID)
	if done.Status != jobSucceeded || done.CompletedPages != 3 || *done.Message != "所选文档已优化并保存，优化要求已随版本记录" {
		t.Fatalf("job = %+v", done)
	}
	for _, d := range f.docs(f.scope)[1:] {
		if d.Revision != 2 || d.Stale || d.OptimizationInstruction != "统一动作" {
			t.Fatalf("%s rev %d", d.Kind, d.Revision)
		}
	}
	for _, req := range text.seen() {
		if req.Operation != "comic_markdown.optimize" || req.Messages[0].Content != optimizeSystem {
			t.Fatal("optimize request")
		}
	}
}

func TestDelayedOptimizationCannotOverwriteAnInFlightEdit(t *testing.T) {
	f := newFixture(t)
	d := f.put(f.scope, kindPagePrompt, promptMD, nil)
	received, release := make(chan struct{}), make(chan struct{})
	text := &fakeText{reply: func(context.Context, TextRequest) (Completion, error) {
		close(received)
		<-release
		return completed(promptMD), nil
	}}
	s := f.newService(text, nil)
	job, err := s.optimize(context.Background(), OptimizeInput{Scope: f.scope, Targets: []PageInput{{d.ID, d.Revision}}, Instruction: "增加雨夜细节"})
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-received:
	case <-time.After(5 * time.Second):
		t.Fatal("provider not called")
	}
	edited := promptMD + "\n用户在等待时修改了内容"
	f.put(f.scope, kindPagePrompt, edited, rev(1))
	close(release)
	done := f.waitJob(job.ID)
	if done.Status != jobFailed || !strings.Contains(*done.Message, "未覆盖") || *done.OutputMarkdown != promptMD {
		t.Fatalf("job = %+v", done)
	}
	if current := f.docs(f.scope)[0]; current.Markdown != edited || current.Revision != 2 {
		t.Fatal("edit lost")
	}
}

func TestSyncJobFillsMissingPagesAndFinishes(t *testing.T) {
	f := newFixture(t)
	f.put(f.scope, kindSettings, settingsMD, nil)
	f.put(f.scope, kindScript, scriptMD, nil)
	f.put(f.scope, kindStoryboard, boardMD+"\n"+strings.ReplaceAll(boardMD, "第1页", "第2页"), nil)
	f.pagePut(f.scope, 1, promptMD, nil)
	text := &fakeText{reply: func(context.Context, TextRequest) (Completion, error) {
		return completed(strings.ReplaceAll(promptMD, "第1页", "第2页")), nil
	}}
	s := f.newService(text, nil)
	plan := f.workspace(f.scope).SyncPlan
	if !slices.Equal(plan.MissingPageNos, []int64{2}) {
		t.Fatal(plan)
	}
	_, err := s.sync(context.Background(), SyncInput{Scope: f.scope, ExpectedPlanFingerprint: "stale"})
	mustErr(t, err, "关联更新计划已变化")
	job, err := s.sync(context.Background(), SyncInput{Scope: f.scope, ExpectedPlanFingerprint: plan.Fingerprint})
	if err != nil || job.TotalPages != 1 {
		t.Fatal(err, job)
	}
	done := f.waitJob(job.ID)
	if done.Status != jobSucceeded || done.CompletedPages != 1 || done.TotalPages != 1 || *done.Message != "本章关联文字已同步，历史版本已保留；漫画需要你另行生成" {
		t.Fatalf("job = %+v", done)
	}
	if d := f.doc(f.scope, func(d Document) bool { return d.PageNo != nil && *d.PageNo == 2 }); d.Stale || len(d.Issues) != 0 {
		t.Fatal("page 2 must be saved and current")
	}
	if req := text.seen()[0]; req.Operation != "comic_markdown.sync" || req.Messages[0].Content != syncSystem {
		t.Fatal("sync request")
	}
	plan = f.workspace(f.scope).SyncPlan
	_, err = s.sync(context.Background(), SyncInput{Scope: f.scope, ExpectedPlanFingerprint: plan.Fingerprint})
	mustErr(t, err, "本章没有待同步的文字或缺页")
}

func (f *fixture) saveStyleReference(name string) WorkVisualProfile {
	f.t.Helper()
	path := filepath.Join(f.paths.Assets(), name)
	if err := os.WriteFile(path, append(slices.Clone(pngBytes), name...), 0o644); err != nil {
		f.t.Fatal(err)
	}
	f.exec(`INSERT INTO assets(id,kind,path,created_at,metadata) VALUES(?,'image',?,1,'{"projectId":"project"}')`, name, path)
	p, err := saveWorkVisualProfile(f.conn, WorkVisualSaveInput{
		ProjectID: "project", NovelWorkID: "work", ConstitutionMarkdown: "## 总体风格\n水墨",
		References: []WorkVisualReferenceInput{{AssetID: name, Role: "style", Weight: 1, SortOrder: 0}},
	})
	if err != nil {
		f.t.Fatal(err)
	}
	return p
}

func TestRenderJobRecordsPagesWithTheirFrozenPrompt(t *testing.T) {
	f := newFixture(t)
	profile := f.saveStyleReference("style.png")
	page := f.put(f.scope, kindPagePrompt, promptMD, nil)
	options, err := saveRenderOptions(f.conn, RenderOptionsSaveInput{Scope: f.scope, PromptInjection: "黑白线稿"})
	if err != nil {
		t.Fatal(err)
	}
	images := &fakeImages{}
	s := f.newService(nil, images)
	job, err := s.render(context.Background(), RenderInput{Scope: f.scope, Pages: []PageInput{{page.ID, page.Revision}}, ExpectedRenderOptionsRevision: rev(options.Revision), RerunPromptInjection: "披风改蓝"})
	if err != nil {
		t.Fatal(err)
	}
	done := f.waitJob(job.ID)
	if done.Status != jobSucceeded || done.CompletedPages != 1 || *done.Message != "所选漫画页面已生成" {
		t.Fatalf("job = %+v", done)
	}
	req := images.seen()[0]
	wantPrompt := renderPrompt(promptMD, "## 总体风格\n水墨", "黑白线稿", "披风改蓝")
	if req.Prompt != wantPrompt || req.Size != "1024x1536" || req.Quality != "high" || req.OutputDir != f.paths.Assets() || req.Settings.ImageAPIKey != "image-secret-key" {
		t.Fatalf("request = %+v", req)
	}
	snapshotRoot := filepath.Join(f.paths.Assets(), ".漫画画风任务快照", job.ID)
	if len(req.References) != 1 || req.References[0].Path != filepath.Join(snapshotRoot, "00.png") || req.References[0].Weight != 1 {
		t.Fatalf("references = %+v", req.References)
	}
	if _, err := os.Stat(snapshotRoot); !os.IsNotExist(err) {
		t.Fatal("reference snapshot must be removed after the job")
	}
	w := f.workspace(f.scope)
	if len(w.Images) != 1 || w.Images[0].Stale || *w.Images[0].EffectivePrompt != wantPrompt || w.Images[0].RerunPromptInjection != "披风改蓝" ||
		w.Images[0].VisualProfileRevision != profile.Revision || w.Images[0].VisualReferenceSnapshot != referencesSnapshot(profile.References) {
		t.Fatalf("image = %+v", w.Images)
	}
	var snapshot map[string]json.RawMessage
	json.Unmarshal([]byte(f.queryText(`SELECT input_snapshot FROM comic_md_jobs WHERE id=?`, job.ID)), &snapshot)
	for _, key := range []string{"scope", "pages", "renderOptions", "workVisualProfile", "rerunPromptInjection"} {
		if _, ok := snapshot[key]; !ok {
			t.Fatalf("snapshot lacks %s", key)
		}
	}
}

func TestRenderProviderFailureFailsTheJob(t *testing.T) {
	f := newFixture(t)
	page := f.put(f.scope, kindPagePrompt, promptMD, nil)
	s := f.newService(nil, &fakeImages{fail: errors.New("upstream said image-secret-key is invalid")})
	job, err := s.render(context.Background(), RenderInput{Scope: f.scope, Pages: []PageInput{{page.ID, page.Revision}}})
	if err != nil {
		t.Fatal(err)
	}
	done := f.waitJob(job.ID)
	if done.Status != jobFailed || *done.Message != "图像服务请求失败，已生成页面已保留；请检查服务后重新选择未完成页面" {
		t.Fatalf("job = %+v", done)
	}
	if n := f.queryInt(`SELECT count(*) FROM comic_md_images`); n != 0 {
		t.Fatal(n)
	}
}

func TestCloseInterruptsRunningJobs(t *testing.T) {
	f := newFixture(t)
	started := make(chan struct{})
	text := &fakeText{reply: func(ctx context.Context, _ TextRequest) (Completion, error) {
		close(started)
		<-ctx.Done()
		return Completion{}, ctx.Err()
	}}
	s := f.newService(text, nil)
	job, err := s.generate(context.Background(), GenerateInput{Scope: f.scope, Stage: "settings", ExpectedSourceRevisionID: "src1"})
	if err != nil {
		t.Fatal(err)
	}
	<-started
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := s.Close(ctx); err != nil {
		t.Fatal(err)
	}
	done := f.waitJob(job.ID)
	if done.Status != jobInterrupted || *done.Message != interruptedMessage {
		t.Fatalf("job = %+v", done)
	}
}

func TestExtractConstitutionSendsReferencesAndReturnsADraft(t *testing.T) {
	f := newFixture(t)
	profile := f.saveStyleReference("style.png")
	text := &fakeText{reply: func(context.Context, TextRequest) (Completion, error) {
		return completed("\n## 总体风格\n浓墨\n"), nil
	}}
	s := f.newService(text, nil)
	out, err := s.extractConstitution(context.Background(), WorkVisualExtractInput{ProjectID: "project", NovelWorkID: "work", ExpectedRevision: profile.Revision, Instruction: "更重的墨色"})
	if err != nil {
		t.Fatal(err)
	}
	if out.ConstitutionMarkdown != "## 总体风格\n浓墨" || out.ProfileRevision != 1 || !slices.Equal(out.ReferenceAssetIDs, []string{"style.png"}) {
		t.Fatalf("out = %+v", out)
	}
	req := text.seen()[0]
	parts := req.Messages[1].Content.([]ContentPart)
	if req.Operation != "comic_markdown.work_visual_extract" || req.Messages[0].Content != extractSystem || len(parts) != 2 ||
		!strings.HasPrefix(parts[1].ImageURL.URL, "data:image/png;base64,") || !strings.Contains(parts[0].Text, "用户补充要求：更重的墨色") {
		t.Fatalf("request = %+v", req)
	}
	_, err = s.extractConstitution(context.Background(), WorkVisualExtractInput{ProjectID: "project", NovelWorkID: "work", ExpectedRevision: 0})
	mustErr(t, err, "作品视觉宪法已有新版本，请刷新后再提取")
	text.reply = func(context.Context, TextRequest) (Completion, error) {
		return Completion{Content: "## 总体", FinishReason: "length"}, nil
	}
	_, err = s.extractConstitution(context.Background(), WorkVisualExtractInput{ProjectID: "project", NovelWorkID: "work", ExpectedRevision: 1})
	mustErr(t, err, "视觉宪法提取文本未正常结束")
	text.reply = func(context.Context, TextRequest) (Completion, error) {
		return Completion{}, errors.New("请求文本接口失败: boom")
	}
	_, err = s.extractConstitution(context.Background(), WorkVisualExtractInput{ProjectID: "project", NovelWorkID: "work", ExpectedRevision: 1})
	mustErr(t, err, "请求文本接口失败: boom")
}

func TestExtractionOnlySendsReferencesFromAppDirectories(t *testing.T) {
	f := newFixture(t)
	outside := filepath.Join(t.TempDir(), "outside.png")
	os.WriteFile(outside, pngBytes, 0o644)
	f.exec(`INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('outside','image',?,1,'{"projectId":"project"}')`, outside)
	if _, err := saveWorkVisualProfile(f.conn, WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{{AssetID: "outside", Role: "style", Weight: 1}}}); err != nil {
		t.Fatal(err)
	}
	s := f.newService(&fakeText{}, nil)
	_, err := s.extractConstitution(context.Background(), WorkVisualExtractInput{ProjectID: "project", NovelWorkID: "work", ExpectedRevision: 1})
	mustErr(t, err, "作品视觉参考图必须位于应用资产目录或输出目录")
	_, err = s.extractConstitution(context.Background(), WorkVisualExtractInput{ProjectID: "project", NovelWorkID: "work", ExpectedRevision: 1, Instruction: ""})
	if err == nil {
		t.Fatal("expected refusal")
	}
	f2 := newFixture(t)
	s2 := f2.newService(&fakeText{}, nil)
	_, err = s2.extractConstitution(context.Background(), WorkVisualExtractInput{ProjectID: "project", NovelWorkID: "work"})
	mustErr(t, err, "请先至少保存一张作品级视觉参考图")
}

func TestImportReferencesRecordsUploadsOfTheWork(t *testing.T) {
	f := newFixture(t)
	s := f.newService(nil, nil)
	dir := t.TempDir()
	a, b := filepath.Join(dir, "a.png"), filepath.Join(dir, "b.jpg")
	os.WriteFile(a, pngBytes, 0o644)
	os.WriteFile(b, []byte{0xFF, 0xD8, 0xFF, 0xE0, 1, 2}, 0o644)
	refs, err := s.importReferences(context.Background(), WorkVisualImportInput{ProjectID: "project", NovelWorkID: "work", Paths: []string{a, b, a}})
	if err != nil {
		t.Fatal(err)
	}
	if len(refs) != 2 || refs[0].Format != "png" || refs[1].Format != "jpg" || filepath.Dir(refs[0].Path) != filepath.Join(f.paths.Assets(), "漫画画风参考") {
		t.Fatalf("refs = %+v", refs)
	}
	raw, _ := json.Marshal(refs[0])
	if string(raw) != `{"id":"`+refs[0].ID+`","kind":"image","path":`+mustJSON(refs[0].Path)+`,"format":"png"}` {
		t.Fatal(string(raw))
	}
	meta := f.queryText(`SELECT metadata FROM assets WHERE id=?`, refs[0].ID)
	if meta != `{"params":{"catalog":{"category":"upload","group":{"id":"work","type":"comic_work"},"origin":"local_upload","version":1},"comicStyleReference":true,"novelWorkId":"work"},"projectId":"project","source":"a.png"}` {
		t.Fatal(meta)
	}
	// The imported asset is a valid work reference.
	if _, err := saveWorkVisualProfile(f.conn, WorkVisualSaveInput{ProjectID: "project", NovelWorkID: "work", References: []WorkVisualReferenceInput{{AssetID: refs[0].ID, Role: "style", Weight: 0.5}}}); err != nil {
		t.Fatal(err)
	}
	bad := filepath.Join(dir, "bad.png")
	os.WriteFile(bad, []byte("text"), 0o644)
	for _, tc := range []struct {
		in   WorkVisualImportInput
		want string
	}{
		{WorkVisualImportInput{ProjectID: "project", NovelWorkID: "work"}, "一次请选择 1 到 8 张画风参考图"},
		{WorkVisualImportInput{ProjectID: "project", NovelWorkID: "work", Paths: make([]string, 9)}, "一次请选择 1 到 8 张画风参考图"},
		{WorkVisualImportInput{ProjectID: "project", NovelWorkID: "work", Paths: []string{filepath.Join(dir, "missing.png")}}, "读取画风参考图失败："},
		{WorkVisualImportInput{ProjectID: "project", NovelWorkID: "work", Paths: []string{bad}}, "画风参考图格式不受支持或文件已损坏"},
		{WorkVisualImportInput{ProjectID: "project", NovelWorkID: "other", Paths: []string{a}}, "未找到当前小说作品"},
	} {
		_, err := s.importReferences(context.Background(), tc.in)
		mustErr(t, err, tc.want)
	}
	entries, _ := os.ReadDir(filepath.Join(f.paths.Assets(), "漫画画风参考"))
	if len(entries) != 2 {
		t.Fatalf("failed imports must not leave files: %d", len(entries))
	}
}

func TestRecoverInterruptsEarlierJobsAndRemovesSnapshots(t *testing.T) {
	f := newFixture(t)
	job := f.insertJob(f.scope, "images", 1)
	snapshot := filepath.Join(f.paths.Assets(), ".漫画画风任务快照", "old-job")
	os.MkdirAll(snapshot, 0o755)
	s := f.newService(nil, nil)
	got, err := s.Recover(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got.InterruptedJobs != 1 || got.LegacyRetired != 0 {
		t.Fatalf("recovery = %+v", got)
	}
	if j := f.jobs(f.scope)[0]; j.ID != job.ID || j.Status != jobInterrupted || *j.Message != interruptedMessage {
		t.Fatalf("job = %+v", j)
	}
	if _, err := os.Stat(filepath.Dir(snapshot)); !os.IsNotExist(err) {
		t.Fatal("snapshots must be removed")
	}
}

const legacyFixtureSQL = `
INSERT INTO novel_works(id,project_id,title,status,created_at,updated_at)
VALUES('legacy-test-work','legacy-test-project','旧漫画兼容验收','active',1,1);
INSERT INTO novel_chapters(id,novel_work_id,sequence_no,chapter_no,current_revision_id,created_at,updated_at)
VALUES('legacy-test-chapter','legacy-test-work',1,1,'legacy-test-source',1,1),('legacy-test-finished-chapter','legacy-test-work',2,2,'legacy-test-finished-source',1,1);
INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,source_kind,created_at)
VALUES('legacy-test-source','legacy-test-chapter',1,'旧小说原著正文，必须保留。','legacy-test-hash','paste',1),('legacy-test-finished-source','legacy-test-finished-chapter',1,'旧完成章节正文','legacy-test-finished-hash','paste',1);
INSERT INTO novel_analysis_lineages(id,novel_work_id,name,status,created_at,updated_at)
VALUES('legacy-test-lineage','legacy-test-work','旧分析主线','active',1,1);
INSERT INTO source_analysis_runs(id,novel_chapter_revision_id,novel_analysis_lineage_id,frozen_input_fingerprint,provider_id,model_id,status,prompt_version,schema_version,idempotency_key,created_at,updated_at)
VALUES('legacy-test-analysis','legacy-test-source','legacy-test-lineage','legacy-fingerprint','local-mock','local-model','running','legacy-prompt','legacy-schema','legacy-analysis-key',1,1);
INSERT INTO source_analysis_run_attempts(id,source_analysis_run_id,attempt_no,status,lease_owner,lease_expires_at,created_at)
VALUES('legacy-test-attempt','legacy-test-analysis',1,'running','old-session',9999999999999,1);
INSERT INTO novel_production_jobs(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,request_hash,idempotency_key,status,stage,stage_index,stage_total,created_at,updated_at)
VALUES('legacy-test-job','legacy-test-project','legacy-test-work','legacy-test-chapter','legacy-test-source','legacy-job-hash','legacy-job-key','queued','saving_source',0,8,1,1);
INSERT INTO novel_production_jobs(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,request_hash,idempotency_key,status,stage,stage_index,stage_total,safe_user_message,created_at,updated_at,finished_at)
VALUES('legacy-test-finished-job','legacy-test-project','legacy-test-work','legacy-test-finished-chapter','legacy-test-finished-source','legacy-finished-hash','legacy-finished-key','succeeded','succeeded',8,8,'原完成记录',1,1,1);
INSERT INTO comic_visual_batches(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,production_job_id,output_target,authorization_json,authorization_fingerprint,request_hash,idempotency_key,status,created_at,updated_at)
VALUES('legacy-test-batch','legacy-test-project','legacy-test-work','legacy-test-chapter','legacy-test-source','legacy-test-job','comic_pages','{"model":"local-model","providerId":"gateway","size":"1024x1536","target":"comic_pages"}','sha256:2d318375bf94ebb3d49b566e661b31cdf4debe6fc8d3d6f1d49490459407f1fd','legacy-batch-hash','legacy-batch-key','authorized',1,1);
`

func TestRetiresValidPendingHistoryOnceAndPreservesCompletedResults(t *testing.T) {
	f := newFixture(t)
	if err := sqlitex.ExecuteScript(f.conn, legacyFixtureSQL, nil); err != nil {
		t.Fatal(err)
	}
	if f.queryInt(`SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)`) != 0 {
		t.Fatal("fixture violates foreign keys")
	}
	s := f.newService(nil, nil)
	for _, want := range []int{4, 0} {
		got, err := s.Recover(context.Background())
		if err != nil || got.LegacyRetired != want {
			t.Fatalf("retired = %d, want %d (%v)", got.LegacyRetired, want, err)
		}
	}
	for _, tc := range []struct{ table, id, status string }{
		{"novel_production_jobs", "legacy-test-job", "error"}, {"comic_visual_batches", "legacy-test-batch", "failed"},
		{"source_analysis_runs", "legacy-test-analysis", "cancelled"}, {"source_analysis_run_attempts", "legacy-test-attempt", "cancelled"},
	} {
		var status, code string
		queryOne(f.conn, `SELECT status,safe_error_code FROM `+tc.table+` WHERE id=?`, func(stmt *sqlite.Stmt) { status, code = stmt.ColumnText(0), stmt.ColumnText(1) }, tc.id)
		if status != tc.status || code != "LEGACY_COMIC_RETIRED" {
			t.Fatalf("%s: %s %s", tc.table, status, code)
		}
	}
	if got := f.queryText(`SELECT status||'|'||safe_user_message FROM novel_production_jobs WHERE id='legacy-test-finished-job'`); got != "succeeded|原完成记录" {
		t.Fatal(got)
	}
	if got := f.queryText(`SELECT content FROM novel_chapter_revisions WHERE id='legacy-test-source'`); got != "旧小说原著正文，必须保留。" {
		t.Fatal(got)
	}
	if got := f.queryText(`SELECT safe_user_message FROM novel_production_jobs WHERE id='legacy-test-job'`); got != legacyRetiredMessage {
		t.Fatal(got)
	}
	if n := f.queryInt(`SELECT count(*) FROM novel_production_jobs`); n != 2 {
		t.Fatal(n)
	}
}

func TestRetirementRollsBackAsOneTransaction(t *testing.T) {
	f := newFixture(t)
	if err := sqlitex.ExecuteScript(f.conn, legacyFixtureSQL+"CREATE TRIGGER reject_retirement BEFORE UPDATE ON comic_visual_batches BEGIN SELECT RAISE(ABORT,'fixture failure'); END;", nil); err != nil {
		t.Fatal(err)
	}
	s := f.newService(nil, nil)
	_, err := s.Recover(context.Background())
	mustErr(t, err, "停用旧小说漫画任务失败：")
	if got := f.queryText(`SELECT status FROM novel_production_jobs WHERE id='legacy-test-job'`); got != "queued" {
		t.Fatal(got)
	}
}
