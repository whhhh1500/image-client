package comic

import (
	"encoding/json"
	"os"
	"slices"
	"strings"
	"testing"
)

func TestMetadataDoesNotStaleContentOrImagesAndStaleCannotBeWashed(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	f.pipeline(s)
	page := f.pagePut(s, 1, promptMD, nil)
	job := f.insertJob(s, "render", 1)
	imagePath := f.writeFile("hash-image.png", []byte("image"))
	f.insertImage("hash-image", job, page, imagePath, nil)
	next := f.metadataPut(s, page)
	if page.ContentHash != next.ContentHash || next.Revision != 2 {
		t.Fatal("metadata save changed the content hash")
	}
	if f.workspace(s).Images[0].Stale {
		t.Fatal("metadata-only save must not stale the image")
	}
	os.Remove(imagePath)
	if !f.workspace(s).Images[0].Stale {
		t.Fatal("a missing file makes the image stale")
	}
	f.metadataPut(s, f.doc(s, byKind(kindSettings)))
	if slices.ContainsFunc(f.docs(s), func(d Document) bool { return d.Stale }) {
		t.Fatal("settings metadata must not stale anything")
	}
	f.put(s, kindSettings, strings.Replace(settingsMD, "黑发", "银发", 1), rev(2))
	stale := f.doc(s, byID(page.ID))
	if !stale.Stale {
		t.Fatal("settings change must stale the page")
	}
	metadata := f.metadataPut(s, stale)
	if !metadata.Stale {
		t.Fatal("metadata save must not wash stale state")
	}
	edited := f.pagePut(s, 1, strings.Replace(promptMD, "远景", "近景", 1), rev(metadata.Revision))
	if !edited.Stale {
		t.Fatal("stale script/storyboard must propagate through an explicit save")
	}
}

func TestManualBodyEditKeepsStaleDependenciesUntilExplicitAcknowledgement(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	first := f.pagePut(s, 1, promptMD, nil)
	second := f.pagePut(s, 2, promptMD, nil)
	f.pagePut(s, 1, strings.Replace(first.Markdown, "左臂包扎", "右腿受伤", 1), rev(first.Revision))
	if !f.doc(s, byID(second.ID)).Stale {
		t.Fatal("later page must be stale after a state change")
	}
	edited, err := saveDocument(f.conn, SaveInput{
		Scope: s, Kind: kindPagePrompt, PageNo: rev(2), Markdown: strings.Replace(second.Markdown, "远景", "近景", 1),
		OptimizationInstruction: "手工核对中", ExpectedRevision: rev(second.Revision),
	})
	if err != nil || !edited.Stale {
		t.Fatal("普通保存不能暗中确认人物状态依赖")
	}
	acknowledged, err := saveDocument(f.conn, SaveInput{
		Scope: s, Kind: kindPagePrompt, PageNo: rev(2), Markdown: edited.Markdown,
		OptimizationInstruction: edited.OptimizationInstruction, ExpectedRevision: rev(edited.Revision), AcknowledgeUpdates: true,
	})
	if err != nil || acknowledged.Stale {
		t.Fatal("acknowledged save must be current")
	}
}

func TestLocalFramesDoNotAffectLaterPagesButStateAffectsFollowingChapterScript(t *testing.T) {
	f := newFixture(t)
	s, later := f.scope, f.chapter("ch2")
	first := f.pagePut(s, 1, promptMD, nil)
	second := f.pagePut(s, 2, promptMD, nil)
	f.put(later, kindScript, scriptMD, nil)
	first = f.pagePut(s, 1, strings.Replace(promptMD, "远景", "特写", 1), rev(first.Revision))
	if f.doc(s, byID(second.ID)).Stale || f.docs(later)[0].Stale {
		t.Fatal("a framing change must not stale anything")
	}
	f.pagePut(s, 1, strings.Replace(first.Markdown, "左臂包扎", "右腿受伤", 1), rev(first.Revision))
	if !f.doc(s, byID(second.ID)).Stale || !f.docs(later)[0].Stale {
		t.Fatal("a character state change must stale later pages and chapters")
	}
	affected := f.workspace(s).AffectedChapters
	if len(affected) != 1 || affected[0].ChapterID != "ch2" || affected[0].DocumentCount != 1 {
		t.Fatalf("affected = %+v", affected)
	}
	reasons := f.doc(s, byID(second.ID)).StaleReasons
	if !slices.Contains(reasons, "第1章第1页人物状态已变化，可能受影响，请同步或核对") {
		t.Fatal(reasons)
	}
}

func TestLegacyReceiptsResolveOriginalRevisionAndMetadataPreservesOldStateBaseline(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	first := f.pagePut(s, 1, promptMD, nil)
	f.exec(`UPDATE comic_md_revisions SET created_at=100 WHERE document_id=?`, first.ID)
	second := f.pagePut(s, 2, promptMD, nil)
	legacy := `[["source","src1"],["settings",null],["script",null],["storyboard",null]]`
	f.exec(`UPDATE comic_md_documents SET dependencies=? WHERE id=?`, legacy, second.ID)
	f.exec(`UPDATE comic_md_revisions SET dependencies=?,created_at=200 WHERE document_id=?`, legacy, second.ID)
	if f.doc(s, byID(second.ID)).Stale {
		t.Fatal("a verifiable legacy receipt is current")
	}
	f.pagePut(s, 1, strings.Replace(promptMD, "左臂包扎", "右腿受伤", 1), rev(1))
	f.exec(`UPDATE comic_md_revisions SET created_at=300 WHERE document_id=? AND revision=2`, first.ID)
	stale := f.doc(s, byID(second.ID))
	if !stale.Stale || !f.metadataPut(s, stale).Stale {
		t.Fatal("legacy baseline must survive a metadata save")
	}
	f.exec(`UPDATE comic_md_revisions SET created_at=100 WHERE document_id=? AND revision=2`, first.ID)
	if !slices.ContainsFunc(f.doc(s, byID(second.ID)).StaleReasons, func(r string) bool { return strings.Contains(r, "不可核验") }) {
		t.Fatal("ambiguous history must be unverifiable")
	}
}

func TestLegacyReferencedRevisionHashSurvivesMetadataAndMissingHistoryIsConservative(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	setting := f.put(s, kindSettings, settingsMD, nil)
	script := f.put(s, kindScript, scriptMD, nil)
	legacy := mustJSON([]any{[]any{"source", "src1"}, []any{"settings", []any{setting.ID, 1}}})
	f.exec(`UPDATE comic_md_documents SET dependencies=? WHERE id=?`, legacy, script.ID)
	f.exec(`UPDATE comic_md_revisions SET dependencies=? WHERE document_id=?`, legacy, script.ID)
	f.metadataPut(s, setting)
	if f.doc(s, byID(script.ID)).Stale {
		t.Fatal("referenced revision hash must match after a metadata save")
	}
	f.exec(`DELETE FROM comic_md_revisions WHERE document_id=? AND revision=1`, setting.ID)
	if !f.doc(s, byID(script.ID)).Stale {
		t.Fatal("missing history must be conservative")
	}
}

func TestBatchRefreshesStateContextAfterOwnWriteAndRejectsExternalEdits(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	first := f.pagePut(s, 1, promptMD, nil)
	second := f.pagePut(s, 2, promptMD, nil)
	o, err := freezeOptimization(f.conn, OptimizeInput{Scope: s, Targets: []PageInput{{first.ID, 1}, {second.ID, 1}}, Instruction: "右腿受伤", AllPages: true})
	if err != nil {
		t.Fatal(err)
	}
	job := f.insertJob(s, "optimize", 2)
	if err := applyOptimization(f.conn, job.ID, o, o.Targets[0], completed(strings.Replace(promptMD, "左臂包扎", "右腿受伤", -1))); err != nil {
		t.Fatal(err)
	}
	o.Guard = f.book(s).guard()
	target, err := refreshOptimization(f.conn, o, o.Targets[1])
	if err != nil {
		t.Fatal(err)
	}
	mustContain(t, target.Prompt, "前页人物状态（第1章第1页）")
	mustContain(t, target.Prompt, "右腿受伤")
	page2 := strings.Replace(strings.ReplaceAll(promptMD, "第1页", "第2页"), "左臂包扎", "右腿受伤", -1)
	if err := applyOptimization(f.conn, job.ID, o, target, completed(page2)); err != nil {
		t.Fatal(err)
	}
	if slices.ContainsFunc(f.docs(s), func(d Document) bool { return d.Stale }) {
		t.Fatal("batch must leave everything current")
	}
	o.Guard = f.book(s).guard()
	f.metadataPut(s, f.docs(s)[0])
	_, err = refreshOptimization(f.conn, o, o.Targets[1])
	mustErr(t, err, "外部编辑")
}

func TestSyncOrdersMissingPagesBeforeExistingLaterPagesAndRetainsPartialFailure(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	board := boardMD + "\n" + strings.ReplaceAll(boardMD, "第1页", "第2页") + "\n" + strings.ReplaceAll(boardMD, "第1页", "第3页")
	f.put(s, kindStoryboard, board, nil)
	f.pagePut(s, 1, promptMD, nil)
	third := f.pagePut(s, 3, promptMD, nil)
	b := f.book(s)
	if got := b.plan().MissingPageNos; !slices.Equal(got, []int64{2}) {
		t.Fatal(got)
	}
	allowed, missing := map[string]bool{third.ID: true}, map[int64]bool{2: true}
	step, err := prepareSync(f.conn, s, b.guard(), allowed, missing)
	if err != nil || step == nil || *step.pageNo != 2 || step.old != nil {
		t.Fatal(err, step)
	}
	mustContain(t, step.prompt, "# 当前目标 Markdown（唯一允许改写）\n# 第2页\n")
	job := f.insertJob(s, "sync", 1)
	page2 := strings.Replace(strings.ReplaceAll(promptMD, "第1页", "第2页"), "左臂包扎", "右腿受伤", -1)
	guard, err := applySync(f.conn, s, job.ID, *step, completed(page2))
	if err != nil {
		t.Fatal(err)
	}
	step, err = prepareSync(f.conn, s, guard, allowed, missing)
	if err != nil || step == nil || *step.pageNo != 3 {
		t.Fatal(err, step)
	}
	mustContain(t, step.prompt, "右腿受伤")
	_, err = applySync(f.conn, s, job.ID, *step, completed("# 第3页\n半成品"))
	mustErr(t, err, "同步内容需要补全")
	if f.doc(s, byID(third.ID)).Revision != 1 {
		t.Fatal("failed step overwrote the page")
	}
	j := f.jobs(s)[0]
	if j.CompletedPages != 1 || !strings.Contains(*j.OutputMarkdown, "半成品") {
		t.Fatalf("job = %+v", j)
	}
}

func TestPagePlanContractionKeepsHistoryAndDefaultExportOmitsObsoletePages(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	f.put(s, kindStoryboard, boardMD+"\n"+strings.ReplaceAll(boardMD, "第1页", "第2页"), nil)
	f.pagePut(s, 1, promptMD, nil)
	second := f.pagePut(s, 2, promptMD, nil)
	f.put(s, kindStoryboard, boardMD, rev(1))
	b := f.book(s)
	if got := b.plan().ObsoletePageNos; !slices.Equal(got, []int64{2}) {
		t.Fatal(got)
	}
	if !f.doc(s, byID(second.ID)).OutOfPlan {
		t.Fatal("page 2 left the plan")
	}
	_, err := renderPages(f.conn, RenderInput{Scope: s, Pages: []PageInput{{second.ID, 1}}})
	if err == nil {
		t.Fatal("out-of-plan page rendered")
	}
	dir := t.TempDir()
	result, err := exportDocuments(f.conn, ExportInput{Scope: s}, dir)
	if err != nil || len(result.Files) != 2 {
		t.Fatal(err, result)
	}
	explicit, err := exportDocuments(f.conn, ExportInput{Scope: s, DocumentIDs: &[]string{second.ID}}, dir)
	if err != nil || len(explicit.Files) != 1 {
		t.Fatal(err, explicit)
	}
}

func TestGuardIgnoresFutureAndUpstreamMetadataButDetectsCurrentTargetEdits(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	setting := f.put(s, kindSettings, settingsMD, nil)
	page := f.pagePut(s, 1, promptMD, nil)
	original := f.book(s).guard()
	f.metadataPut(s, setting)
	if f.book(s).guard() != original {
		t.Fatal("upstream metadata changed the guard")
	}
	f.pagePut(f.chapter("ch3"), 1, promptMD, nil)
	if f.book(s).guard() != original {
		t.Fatal("a future chapter changed the guard")
	}
	f.metadataPut(s, page)
	if f.book(s).guard() == original {
		t.Fatal("a chapter document save must change the guard")
	}
}

func TestPreviousDialogueDoesNotStaleFollowingScriptButPreviousStateDoes(t *testing.T) {
	f := newFixture(t)
	s, later := f.scope, f.chapter("ch2")
	first := f.put(s, kindScript, scriptMD, nil)
	f.put(later, kindScript, scriptMD, nil)
	f.put(s, kindScript, strings.Replace(scriptMD, "别出声", "快进来", 1), rev(first.Revision))
	if f.docs(later)[0].Stale {
		t.Fatal("dialogue change staled the next chapter")
	}
	f.put(s, kindScript, strings.Replace(scriptMD, "左臂包扎", "右腿受伤", 1), rev(2))
	if !f.docs(later)[0].Stale {
		t.Fatal("state change must stale the next chapter")
	}
}

func TestSyncUpdatesUpstreamFirstPreservesOldInstructionAndChecksLateCAS(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	f.pipeline(s)
	f.pagePut(s, 1, promptMD, nil)
	f.metadataPut(s, f.doc(s, byKind(kindScript)))
	f.put(s, kindSettings, strings.Replace(settingsMD, "黑发", "银发", 1), rev(1))
	b := f.book(s)
	plan := b.plan()
	kinds := []string{}
	allowed := map[string]bool{}
	for _, target := range plan.Targets {
		kinds = append(kinds, target.Kind)
		allowed[target.DocumentID] = true
	}
	if !slices.Equal(kinds, []string{kindScript, kindStoryboard, kindPagePrompt}) {
		t.Fatal(kinds)
	}
	guard := b.guard()
	job := f.insertJob(s, "sync", 3)
	for _, tc := range []struct{ kind, md string }{{kindScript, scriptMD}, {kindStoryboard, boardMD}} {
		step, err := prepareSync(f.conn, s, guard, allowed, map[int64]bool{})
		if err != nil || step.kind != tc.kind {
			t.Fatal(err, step)
		}
		mustContain(t, step.prompt, "最新作品设定")
		mustContain(t, step.prompt, "银发")
		if strings.Contains(step.prompt, "只改优化要求") {
			t.Fatal("historical instruction must not be replayed")
		}
		if tc.kind == kindStoryboard {
			_, err := applySync(f.conn, s, job.ID, *step, completed(boardMD+"\n\n"+strings.ReplaceAll(boardMD, "第1页", "第2页")))
			mustErr(t, err, "不会自动改变分页数量")
			if f.doc(s, byKind(kindStoryboard)).Revision != 1 {
				t.Fatal("page count change saved")
			}
		}
		if guard, err = applySync(f.conn, s, job.ID, *step, completed(tc.md)); err != nil {
			t.Fatal(err)
		}
	}
	if got := f.doc(s, byKind(kindScript)).OptimizationInstruction; got != "只改优化要求" {
		t.Fatal(got)
	}
	step, err := prepareSync(f.conn, s, guard, allowed, map[int64]bool{})
	if err != nil {
		t.Fatal(err)
	}
	f.metadataPut(s, f.doc(s, byKind(kindPagePrompt)))
	_, err = applySync(f.conn, s, job.ID, *step, completed(promptMD))
	mustErr(t, err, "外部编辑")
	if j := f.jobs(s)[0]; j.CompletedPages != 2 {
		t.Fatal(j.CompletedPages)
	}
}

func TestLegacyRemovedPageStateDoesNotDisappearFromTheHistoricalReceipt(t *testing.T) {
	f := newFixture(t)
	s, later := f.scope, f.chapter("ch2")
	board := f.put(s, kindStoryboard, boardMD+"\n"+strings.ReplaceAll(boardMD, "第1页", "第2页"), nil)
	second := f.pagePut(s, 2, promptMD, nil)
	f.exec(`UPDATE comic_md_revisions SET created_at=100`)
	script := f.put(later, kindScript, scriptMD, nil)
	legacy := `[["source","src2"],["settings",null],["ch1","src1",null,null]]`
	f.exec(`UPDATE comic_md_documents SET dependencies=? WHERE id=?`, legacy, script.ID)
	f.exec(`UPDATE comic_md_revisions SET dependencies=?,created_at=200 WHERE document_id=?`, legacy, script.ID)
	if f.doc(later, byID(script.ID)).Stale {
		t.Fatal("legacy receipt is verifiable")
	}
	f.put(s, kindStoryboard, boardMD, rev(board.Revision))
	f.exec(`UPDATE comic_md_revisions SET created_at=300 WHERE document_id=? AND revision=2`, board.ID)
	if !f.doc(s, byID(second.ID)).OutOfPlan {
		t.Fatal("page 2 left the plan")
	}
	if !f.doc(later, byID(script.ID)).Stale {
		t.Fatal("a removed page's state must stay in the historical receipt")
	}
}

func TestStoredDependenciesAreVersionTwoReceipts(t *testing.T) {
	f := newFixture(t)
	f.pipeline(f.scope)
	page := f.pagePut(f.scope, 1, promptMD, nil)
	raw := f.queryText(`SELECT dependencies FROM comic_md_documents WHERE id=?`, page.ID)
	var r receipt
	if err := json.Unmarshal([]byte(raw), &r); err != nil || r.Version != 2 {
		t.Fatal(raw)
	}
	keys := sortedKeys(r.Values)
	want := []string{"doc::settings", "doc:ch1:script", "doc:ch1:storyboard", "source:ch1"} // byte order, as stored
	if !slices.Equal(keys, want) {
		t.Fatalf("keys = %v", keys)
	}
	if r.Values["source:ch1"].Label != "章节正文" || *r.Values["source:ch1"].Hash != contentHash("第一章正文") {
		t.Fatal(r.Values["source:ch1"])
	}
	if settingsDeps := f.queryText(`SELECT dependencies FROM comic_md_documents WHERE kind='settings'`); settingsDeps != `{"version":2,"values":{}}` {
		t.Fatal(settingsDeps)
	}
}

func TestSyncPlanBlocksOnStaleEarlierChaptersAndIncompleteUpstream(t *testing.T) {
	f := newFixture(t)
	s, later := f.scope, f.chapter("ch2")
	first := f.pagePut(s, 1, promptMD, nil)
	f.pagePut(s, 2, promptMD, nil)
	f.put(later, kindScript, scriptMD, nil)
	f.pagePut(s, 1, strings.ReplaceAll(promptMD, "左臂包扎", "右腿受伤"), rev(first.Revision))
	plan := f.book(later).plan()
	if len(plan.Targets) != 1 || plan.BlockedReason == nil || *plan.BlockedReason != "前章关联资料尚未更新，请先到前章同步或核对，再更新本章" {
		t.Fatalf("plan = %+v", plan)
	}
	_, err := prepareSync(f.conn, later, f.book(later).guard(), map[string]bool{plan.Targets[0].DocumentID: true}, nil)
	mustErr(t, err, "前章关联资料尚未更新")

	g := newFixture(t)
	settings := g.put(g.scope, kindSettings, settingsMD, nil)
	g.put(g.scope, kindScript, scriptMD, nil)
	g.put(g.scope, kindSettings, "## 世界观\n古代小镇。", rev(settings.Revision))
	plan = g.book(g.scope).plan()
	if len(plan.Targets) != 1 || plan.Targets[0].Kind != kindScript || plan.BlockedReason == nil || *plan.BlockedReason != "请先补全作品设定的必要节点" {
		t.Fatalf("plan = %+v", plan)
	}
	if !slices.Equal(plan.Targets[0].Reasons, []string{"作品设定已变化，可能受影响，请同步或核对"}) {
		t.Fatal(plan.Targets[0].Reasons)
	}
}

func TestSyncRefusesUpdatesOutsideTheAuthorizedPlan(t *testing.T) {
	f := newFixture(t)
	s := f.scope
	f.pipeline(s)
	f.put(s, kindSettings, strings.ReplaceAll(settingsMD, "黑发", "银发"), rev(1))
	b := f.book(s)
	_, err := prepareSync(f.conn, s, b.guard(), map[string]bool{}, map[int64]bool{})
	mustErr(t, err, "仍有超出本次更新范围的内容需要核对")
	_, err = prepareSync(f.conn, s, "other-guard", map[string]bool{}, map[int64]bool{})
	mustErr(t, err, "同步期间关联资料已有外部编辑")
}
