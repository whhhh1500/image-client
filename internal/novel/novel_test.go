package novel

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"maps"
	"path/filepath"
	"strings"
	"testing"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/ipc"
	"imageclient/internal/store"
)

func newTestRouter(t *testing.T) (*store.DB, *ipc.Router) {
	t.Helper()
	db, err := store.Open(filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = db.Close() })
	r := ipc.NewRouter()
	New(Deps{DB: db}).Register(r)
	return db, r
}

// rawCall runs a command the way the frontend does ({"input": in} as JSON) and
// returns the result as it goes over the wire.
func rawCall(t *testing.T, r *ipc.Router, command string, in map[string]any) ([]byte, error) {
	t.Helper()
	args, err := json.Marshal(map[string]any{"input": in})
	if err != nil {
		t.Fatal(err)
	}
	res, err := r.Invoke(context.Background(), command, args)
	if err != nil {
		return nil, err
	}
	body, err := json.Marshal(res)
	if err != nil {
		t.Fatal(err)
	}
	return body, nil
}

// call is rawCall decoded back into T.
func call[T any](t *testing.T, r *ipc.Router, command string, in map[string]any) (T, error) {
	t.Helper()
	var out T
	body, err := rawCall(t, r, command, in)
	if err != nil {
		return out, err
	}
	if err := json.Unmarshal(body, &out); err != nil {
		t.Fatal(err)
	}
	return out, nil
}

func mustCall[T any](t *testing.T, r *ipc.Router, command string, in map[string]any) T {
	t.Helper()
	out, err := call[T](t, r, command, in)
	if err != nil {
		t.Fatalf("%s: %v", command, err)
	}
	return out
}

func createWork(t *testing.T, r *ipc.Router, project, key string) Work {
	t.Helper()
	return mustCall[Work](t, r, "novel_work_create", map[string]any{
		"projectId": project, "title": "测试小说", "idempotencyKey": key,
	})
}

func snapshot(t *testing.T, r *ipc.Router, project string, work Work) Snapshot {
	t.Helper()
	return mustCall[Snapshot](t, r, "novel_work_get", map[string]any{
		"projectId": project, "novelWorkId": work.ID,
	})
}

// baseRevision is a valid request that creates chapter 1; tests override fields.
func baseRevision(workID, key string) map[string]any {
	return map[string]any{
		"projectId": "project-a", "novelWorkId": workID,
		"chapterNo": 1, "sequenceNo": 1, "title": "第一章",
		"content": "正文", "idempotencyKey": key,
	}
}

func with(m map[string]any, fields map[string]any) map[string]any {
	out := maps.Clone(m)
	maps.Copy(out, fields)
	return out
}

func wantError(t *testing.T, err error, want string) {
	t.Helper()
	if err == nil {
		t.Fatalf("error = nil, want %q", want)
	}
	if err.Error() != want {
		t.Fatalf("error = %q, want %q", err.Error(), want)
	}
}

func exec(t *testing.T, db *store.DB, sql string, args ...any) {
	t.Helper()
	err := db.Tx(context.Background(), func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, sql, &sqlitex.ExecOptions{Args: args})
	})
	if err != nil {
		t.Fatal(err)
	}
}

func count(t *testing.T, db *store.DB, sql string, args ...any) int64 {
	t.Helper()
	var n int64
	err := db.With(context.Background(), func(conn *sqlite.Conn) error {
		v, err := scalarInt(conn, sql, args...)
		n = v
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return n
}

func TestWorkCreateReplayAndProjectScope(t *testing.T) {
	db, r := newTestRouter(t)
	work := createWork(t, r, "project-a", "work-create-a")
	replay := createWork(t, r, "project-a", "work-create-a")
	if replay.ID != work.ID {
		t.Fatalf("replay id = %s, want %s", replay.ID, work.ID)
	}
	if work.Status != "active" || work.Description != "" || work.PublishedCanonVersionID == "" {
		t.Fatalf("unexpected work: %+v", work)
	}

	list := mustCall[[]Work](t, r, "novel_work_list", map[string]any{"projectId": "project-a"})
	if len(list) != 1 {
		t.Fatalf("project-a works = %d, want 1", len(list))
	}
	other, err := rawCall(t, r, "novel_work_list", map[string]any{"projectId": "project-b"})
	if err != nil {
		t.Fatal(err)
	}
	if string(other) != "[]" {
		t.Fatalf("empty list wire = %s, want []", other)
	}

	for _, check := range []struct {
		name string
		sql  string
	}{
		{"canon C0", "SELECT COUNT(*) FROM novel_canon_versions WHERE novel_work_id = ? AND version = 0"},
		{"state S0", "SELECT COUNT(*) FROM novel_state_versions WHERE novel_work_id = ? AND version = 0"},
		{"main lineage", "SELECT COUNT(*) FROM novel_analysis_lineages WHERE novel_work_id = ? AND continuous_through_sequence_no = 0"},
	} {
		if n := count(t, db, check.sql, work.ID); n != 1 {
			t.Errorf("%s rows = %d, want 1", check.name, n)
		}
	}

	_, err = call[Snapshot](t, r, "novel_work_get", map[string]any{"projectId": "project-b", "novelWorkId": work.ID})
	wantError(t, err, "小说不存在或不属于当前项目")
}

func TestWorkCreateValidation(t *testing.T) {
	_, r := newTestRouter(t)
	valid := map[string]any{"projectId": "project-a", "title": "标题", "idempotencyKey": "k-valid"}
	cases := []struct {
		name   string
		fields map[string]any
		want   string
	}{
		{"blank project", map[string]any{"projectId": " "}, "projectId 不能为空"},
		{"blank title", map[string]any{"title": "  ", "idempotencyKey": "k-blank-title"}, "title 不能为空"},
		{"title too long", map[string]any{"title": strings.Repeat("x", maxTitleBytes+1), "idempotencyKey": "k-long"}, "title 不能超过 512 字节"},
		{"blank key", map[string]any{"idempotencyKey": "  "}, "idempotencyKey 不能为空"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, err := call[Work](t, r, "novel_work_create", with(valid, c.fields))
			wantError(t, err, c.want)
		})
	}

	t.Run("title at the byte limit is accepted", func(t *testing.T) {
		work := mustCall[Work](t, r, "novel_work_create", with(valid, map[string]any{
			"title": strings.Repeat("x", maxTitleBytes), "idempotencyKey": "k-max-title",
		}))
		if len(work.Title) != maxTitleBytes {
			t.Fatalf("title bytes = %d", len(work.Title))
		}
	})

	t.Run("key reused with another payload", func(t *testing.T) {
		createWork(t, r, "project-a", "k-reused")
		_, err := call[Work](t, r, "novel_work_create", map[string]any{
			"projectId": "project-a", "title": "另一本", "idempotencyKey": "k-reused",
		})
		wantError(t, err, "idempotencyKey 已用于不同业务载荷")
	})
}

func TestWorkListArchivedFilter(t *testing.T) {
	db, r := newTestRouter(t)
	active := createWork(t, r, "project-a", "list-active")
	archived := createWork(t, r, "project-a", "list-archived")
	exec(t, db, "UPDATE novel_works SET status = 'archived' WHERE id = ?", archived.ID)

	visible := mustCall[[]Work](t, r, "novel_work_list", map[string]any{"projectId": "project-a"})
	if len(visible) != 1 || visible[0].ID != active.ID {
		t.Fatalf("default list = %+v, want only the active work", visible)
	}
	all := mustCall[[]Work](t, r, "novel_work_list", map[string]any{"projectId": "project-a", "includeArchived": true})
	if len(all) != 2 {
		t.Fatalf("includeArchived list = %d works, want 2", len(all))
	}

	got := snapshot(t, r, "project-a", archived)
	if got.Work.Status != "archived" {
		t.Fatalf("snapshot of archived work status = %s", got.Work.Status)
	}
	_, err := call[ChapterRevision](t, r, "novel_chapter_revision_create", baseRevision(archived.ID, "archived-write"))
	wantError(t, err, "小说已归档，恢复后才能修改")
}

func TestWorkSnapshotWireShape(t *testing.T) {
	_, r := newTestRouter(t)
	work := createWork(t, r, "project-a", "snapshot-shape")
	body, err := rawCall(t, r, "novel_work_get", map[string]any{"projectId": "project-a", "novelWorkId": work.ID})
	if err != nil {
		t.Fatal(err)
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(body, &fields); err != nil {
		t.Fatal(err)
	}
	wantFields := []string{"work", "volumes", "chapters", "revisions", "entities", "contextStatus",
		"currentContextRevisionId", "currentNovelStateThroughSequenceNo", "canonicalVersionId", "lineages"}
	if len(fields) != len(wantFields) {
		t.Fatalf("snapshot has %d fields, want %d: %s", len(fields), len(wantFields), body)
	}
	for _, name := range wantFields {
		if _, ok := fields[name]; !ok {
			t.Errorf("snapshot missing %q", name)
		}
	}
	for _, name := range []string{"volumes", "chapters", "revisions", "entities"} {
		if string(fields[name]) != "[]" {
			t.Errorf("%s = %s, want []", name, fields[name])
		}
	}
	if string(fields["currentContextRevisionId"]) != "null" {
		t.Errorf("currentContextRevisionId = %s, want null", fields["currentContextRevisionId"])
	}
	if string(fields["contextStatus"]) != `"not_started"` {
		t.Errorf("contextStatus = %s", fields["contextStatus"])
	}
	if string(fields["currentNovelStateThroughSequenceNo"]) != "0" {
		t.Errorf("currentNovelStateThroughSequenceNo = %s", fields["currentNovelStateThroughSequenceNo"])
	}
	snap := snapshot(t, r, "project-a", work)
	if len(snap.Lineages) != 1 || snap.Lineages[0].Status != "active" {
		t.Fatalf("lineages = %+v", snap.Lineages)
	}
	if snap.CanonicalVersionID != work.PublishedCanonVersionID {
		t.Fatalf("canonicalVersionId = %s, want %s", snap.CanonicalVersionID, work.PublishedCanonVersionID)
	}
}

func TestChapterRevisionReplayAndNormalization(t *testing.T) {
	_, r := newTestRouter(t)
	work := createWork(t, r, "project-a", "work-create-revision")
	input := with(baseRevision(work.ID, "revision-create-1"), map[string]any{
		"content": "第一行\r\n第二行",
	})
	revision := mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", input)
	replay := mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", input)
	if revision.ID != replay.ID {
		t.Fatalf("replay id = %s, want %s", replay.ID, revision.ID)
	}
	const wantContent = "第一行\n第二行"
	if revision.Content != wantContent {
		t.Fatalf("content = %q", revision.Content)
	}
	sum := sha256.Sum256([]byte(wantContent))
	if revision.ContentHash != "sha256:"+hex.EncodeToString(sum[:]) {
		t.Fatalf("contentHash = %s", revision.ContentHash)
	}
	if revision.RevisionNo != 1 || revision.SourceKind != "paste" || revision.AnalysisStatus == nil || *revision.AnalysisStatus != "saved" {
		t.Fatalf("unexpected revision: %+v", revision)
	}
	if revision.AssetID != nil || revision.RequestedParentContextRevisionID != nil {
		t.Fatalf("absent optional fields should be null: %+v", revision)
	}

	snap := snapshot(t, r, "project-a", work)
	if len(snap.Chapters) != 1 || snap.Chapters[0].LatestRevisionID == nil || *snap.Chapters[0].LatestRevisionID != revision.ID {
		t.Fatalf("chapters = %+v", snap.Chapters)
	}
	if snap.CurrentNovelStateThroughSequenceNo != 0 {
		t.Fatalf("state through = %d", snap.CurrentNovelStateThroughSequenceNo)
	}

	second := mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", map[string]any{
		"projectId": "project-a", "novelWorkId": work.ID, "chapterId": snap.Chapters[0].ID,
		"content": "修订", "idempotencyKey": "revision-create-2",
	})
	if second.RevisionNo != 2 || second.ChapterID != snap.Chapters[0].ID {
		t.Fatalf("second revision = %+v", second)
	}

	_, err := call[ChapterRevision](t, r, "novel_chapter_revision_create", with(input, map[string]any{
		"projectId": "project-b", "idempotencyKey": "revision-wrong-owner",
	}))
	wantError(t, err, "小说不存在或不属于当前项目")
}

func TestChapterRevisionTitleRules(t *testing.T) {
	db, r := newTestRouter(t)
	work := createWork(t, r, "project-a", "rename-work")
	mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", with(baseRevision(work.ID, "rename-1"), map[string]any{
		"title": "原章节名", "content": "原正文",
	}))
	chapterID := snapshot(t, r, "project-a", work).Chapters[0].ID

	t.Run("a new title replaces the chapter title", func(t *testing.T) {
		input := map[string]any{
			"projectId": "project-a", "novelWorkId": work.ID, "chapterId": chapterID,
			"title": "  新章节名  ", "content": "新正文", "idempotencyKey": "rename-2",
		}
		second := mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", input)
		snap := snapshot(t, r, "project-a", work)
		if snap.Chapters[0].Title == nil || *snap.Chapters[0].Title != "新章节名" {
			t.Fatalf("title = %v", snap.Chapters[0].Title)
		}
		if snap.Chapters[0].LatestRevisionID == nil || *snap.Chapters[0].LatestRevisionID != second.ID {
			t.Fatalf("head did not move to the second revision")
		}
		replay := mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", input)
		if replay.ID != second.ID {
			t.Fatalf("replay id = %s, want %s", replay.ID, second.ID)
		}
	})

	t.Run("an absent title keeps the chapter title", func(t *testing.T) {
		mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", map[string]any{
			"projectId": "project-a", "novelWorkId": work.ID, "chapterId": chapterID,
			"content": "第三稿", "idempotencyKey": "rename-3",
		})
		snap := snapshot(t, r, "project-a", work)
		if snap.Chapters[0].Title == nil || *snap.Chapters[0].Title != "新章节名" {
			t.Fatalf("title = %v, want 新章节名", snap.Chapters[0].Title)
		}
	})

	t.Run("an invalid title saves nothing", func(t *testing.T) {
		before := snapshot(t, r, "project-a", work)
		storedBefore := count(t, db, "SELECT COUNT(*) FROM novel_chapter_revisions WHERE novel_chapter_id = ?", chapterID)
		for i, title := range []string{"   ", strings.Repeat("x", maxTitleBytes+1)} {
			_, err := call[ChapterRevision](t, r, "novel_chapter_revision_create", map[string]any{
				"projectId": "project-a", "novelWorkId": work.ID, "chapterId": chapterID,
				"title": title, "content": "不应写入的正文", "idempotencyKey": "rename-invalid-" + string(rune('a'+i)),
			})
			if err == nil {
				t.Fatalf("title %d: expected an error", i)
			}
		}
		after := snapshot(t, r, "project-a", work)
		if len(after.Revisions) != len(before.Revisions) {
			t.Fatalf("revisions = %d, want %d", len(after.Revisions), len(before.Revisions))
		}
		if n := count(t, db, "SELECT COUNT(*) FROM novel_chapter_revisions WHERE novel_chapter_id = ?", chapterID); n != storedBefore {
			t.Fatalf("stored revisions = %d, want %d", n, storedBefore)
		}
		if after.Chapters[0].LatestRevisionID == nil || *after.Chapters[0].LatestRevisionID != *before.Chapters[0].LatestRevisionID {
			t.Fatalf("head moved on a rejected write")
		}
	})
}

func TestChapterRevisionValidation(t *testing.T) {
	db, r := newTestRouter(t)
	work := createWork(t, r, "project-a", "validation-work")
	exec(t, db, "INSERT INTO assets (id, kind, path, created_at, metadata) VALUES ('asset-a', 'text', 'a', 1, ?)", `{"projectId":"project-a"}`)
	exec(t, db, "INSERT INTO assets (id, kind, path, created_at, metadata) VALUES ('asset-b', 'text', 'b', 1, ?)", `{"projectId":"project-b"}`)
	base := baseRevision(work.ID, "validation-base")

	cases := []struct {
		name   string
		fields map[string]any
		want   string
	}{
		{"blank content", map[string]any{"content": "  \n "}, "章节正文不能为空"},
		{"oversized content", map[string]any{"content": strings.Repeat("a", maxChapterContentBytes+1)}, "章节正文不能超过 8388608 字节"},
		{"unknown source kind", map[string]any{"sourceKind": "bogus"}, "sourceKind 无效"},
		{"asset source without asset", map[string]any{"sourceKind": "asset"}, "asset sourceKind 必须提供 assetId"},
		{"missing asset", map[string]any{"assetId": "asset-missing"}, "正文资产不存在"},
		{"asset of another project", map[string]any{"assetId": "asset-b"}, "正文资产不属于当前项目"},
		{"missing parent context", map[string]any{"parentContextRevisionId": "ctx-missing"}, "冻结父上下文不存在或不属于当前小说"},
		{"new chapter without number", map[string]any{"chapterNo": nil}, "新章节必须提供 chapterNo"},
		{"zero chapter number", map[string]any{"chapterNo": 0}, "sequenceNo 和 chapterNo 必须是正整数"},
		{"unknown volume", map[string]any{"volumeId": "vol-missing"}, "卷不存在或不属于当前小说"},
		{"chapter of another work", map[string]any{"chapterId": "nchapter-missing", "chapterNo": nil}, "章节不存在或不属于当前小说"},
		{"blank title on a new chapter", map[string]any{"title": "  "}, "title 不能为空"},
		{"blank idempotency key", map[string]any{"idempotencyKey": ""}, "idempotencyKey 不能为空"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			// Field checks run after the idempotency key check, so each case
			// gets its own valid key unless the case sets one.
			in := with(base, map[string]any{"idempotencyKey": "validation-" + c.name})
			_, err := call[ChapterRevision](t, r, "novel_chapter_revision_create", with(in, c.fields))
			wantError(t, err, c.want)
		})
	}

	t.Run("owned asset is accepted", func(t *testing.T) {
		revision := mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", with(base, map[string]any{
			"idempotencyKey": "asset-ok", "sourceKind": "asset", "assetId": "asset-a",
		}))
		if revision.AssetID == nil || *revision.AssetID != "asset-a" || revision.SourceKind != "asset" {
			t.Fatalf("revision = %+v", revision)
		}
	})

	if n := count(t, db, "SELECT COUNT(*) FROM novel_chapter_revisions"); n != 1 {
		t.Fatalf("stored revisions = %d, want only the owned asset revision", n)
	}
}

func TestChapterRevisionReplayRejectsChangedPayload(t *testing.T) {
	_, r := newTestRouter(t)
	work := createWork(t, r, "project-a", "payload-work")
	input := baseRevision(work.ID, "payload-key")
	first := mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", input)
	replay := mustCall[ChapterRevision](t, r, "novel_chapter_revision_create", input)
	if replay.ID != first.ID {
		t.Fatalf("replay id = %s, want %s", replay.ID, first.ID)
	}
	_, err := call[ChapterRevision](t, r, "novel_chapter_revision_create", with(input, map[string]any{"content": "另一份正文"}))
	wantError(t, err, "idempotencyKey 已用于不同业务载荷")
}

// The receipt hash must match the Rust backend's serde_json encoding, or
// receipts written before the port cannot be replayed.
func TestRequestHashMatchesSerdeEncoding(t *testing.T) {
	request := map[string]any{
		"projectId":      "p",
		"title":          "a\"b\n \u0001<&>",
		"description":    nil,
		"idempotencyKey": "k",
	}
	const want = `{"description":null,"idempotencyKey":"k","projectId":"p","title":"a\"b\n` + " " + `\u0001<&>"}`
	var b bytes.Buffer
	writeCanonical(&b, request)
	if b.String() != want {
		t.Fatalf("encoding = %s\nwant      %s", b.String(), want)
	}
	sum := sha256.Sum256([]byte(want))
	if got := requestHash(request); got != "sha256:"+hex.EncodeToString(sum[:]) {
		t.Fatalf("requestHash = %s", got)
	}
}
