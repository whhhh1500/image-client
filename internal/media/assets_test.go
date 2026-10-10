package media

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"sync"
	"testing"

	"imageclient/internal/ipc"
)

func TestRegisterExposesTheMediaCommands(t *testing.T) {
	router := ipc.NewRouter()
	newTestEnv(t).svc.Register(router)
	want := []string{
		"asset_import_files", "asset_publish_media", "cache_promptlib_image", "compress_image_file",
		"convert_image_file", "inspect_image_file", "list_cached_promptlib_images", "media_hosting_get",
		"media_hosting_save", "persist_assets_batch", "read_text_asset", "save_document_version", "save_media_asset",
	}
	if got := router.Names(); !slices.Equal(got, want) {
		t.Fatalf("commands = %v, want %v", got, want)
	}
}

var uuidName = `[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}`

func TestSaveTextWritesMarkdownUnderTheScriptFolder(t *testing.T) {
	env := newTestEnv(t)
	asset, err := env.svc.SaveText("剧本", "第一幕", "model-a")
	if err != nil {
		t.Fatal(err)
	}
	rel, _ := filepath.Rel(env.output, asset.Path)
	if !regexp.MustCompile(`^剧本[/\\]\d{8}-\d{6}[/\\]` + uuidName + `\.md$`).MatchString(rel) {
		t.Fatalf("unexpected path %s", rel)
	}
	if asset.Kind != "text" || asset.Format != "md" || !strings.HasPrefix(filepath.Base(asset.Path), asset.ID) {
		t.Fatalf("unexpected asset %+v", asset)
	}
	if data, _ := os.ReadFile(asset.Path); string(data) != "第一幕" {
		t.Fatalf("content = %q", data)
	}
}

func TestSaveMediaValidatesKindExtensionAndContent(t *testing.T) {
	env := newTestEnv(t)
	pngData := pngBytes(t, testImage(2, 2))
	mp4 := []byte("\x00\x00\x00\x18ftypisom-rest-of-file")
	cases := []struct {
		kind, ext string
		data      []byte
		err       string
		folder    string
		format    string
	}{
		{kind: "image", ext: ".PNG", data: pngData, folder: "图片", format: "png"},
		{kind: "image", ext: "jpeg", data: pngData, folder: "图片", format: "jpeg"}, // the signature, not the name, is checked
		{kind: "video", ext: "mp4", data: mp4, folder: "视频", format: "mp4"},
		{kind: "video", ext: "MOV", data: mp4, folder: "视频", format: "mov"},
		{kind: "image", ext: "", data: pngData, err: "无效的媒体扩展名"},
		{kind: "image", ext: "p/ng", data: pngData, err: "无效的媒体扩展名"},
		{kind: "image", ext: "bmp", data: pngData, err: "不支持的媒体类型或扩展名：image.bmp"},
		{kind: "audio", ext: "mp3", data: mp4, err: "不支持的媒体类型或扩展名：audio.mp3"},
		{kind: "image", ext: "png", data: []byte("<html>"), err: "图片资产内容格式校验失败"},
		{kind: "video", ext: "webm", data: mp4, err: "视频资产内容格式校验失败：webm"},
	}
	for _, tc := range cases {
		asset, err := env.svc.SaveMedia(tc.kind, tc.ext, tc.data)
		if tc.err != "" {
			if errText(err) != tc.err {
				t.Errorf("%s.%s: error %q, want %q", tc.kind, tc.ext, errText(err), tc.err)
			}
			continue
		}
		if err != nil {
			t.Errorf("%s.%s: %v", tc.kind, tc.ext, err)
			continue
		}
		rel, _ := filepath.Rel(env.output, asset.Path)
		pattern := `^` + tc.folder + `[/\\]合成[/\\]\d{8}-\d{6}[/\\]` + uuidName + `\.` + tc.format + `$`
		if !regexp.MustCompile(pattern).MatchString(rel) || asset.Format != tc.format || asset.Kind != tc.kind {
			t.Errorf("%s.%s: unexpected asset %+v (%s)", tc.kind, tc.ext, asset, rel)
		}
	}
}

func TestMediaBase64IsBoundedBeforeDecoding(t *testing.T) {
	if _, err := decodeBase64Limited(strings.Repeat("A", 13), 8, "too large", "invalid"); errText(err) != "too large" {
		t.Fatalf("oversized payload: %v", err)
	}
	if _, err := decodeBase64Limited("not base64!", 64, "too large", "invalid"); errText(err) != "invalid" {
		t.Fatalf("invalid payload: %v", err)
	}
	if data, err := decodeBase64Limited(base64.StdEncoding.EncodeToString([]byte("12345678")), 8, "", ""); err != nil || string(data) != "12345678" {
		t.Fatalf("valid payload: %q %v", data, err)
	}

	router := ipc.NewRouter()
	env := newTestEnv(t)
	env.svc.Register(router)
	_, err := router.Invoke(context.Background(), "save_media_asset", json.RawMessage(`{"kind":"image","ext":"png","dataBase64":"%%%"}`))
	if errText(err) != "媒体数据不是有效的 base64" {
		t.Fatalf("invoke error = %v", err)
	}
	args, _ := json.Marshal(map[string]string{"kind": "image", "ext": "png", "dataBase64": base64.StdEncoding.EncodeToString(pngBytes(t, testImage(1, 1)))})
	result, err := router.Invoke(context.Background(), "save_media_asset", args)
	if err != nil {
		t.Fatal(err)
	}
	encoded, _ := json.Marshal(result)
	if strings.Contains(string(encoded), "width") || !strings.Contains(string(encoded), `"format":"png"`) {
		t.Fatalf("asset JSON = %s", encoded)
	}
}

func TestReadTextAssetOnlyReadsDocumentsInsideTheRoots(t *testing.T) {
	env := newTestEnv(t)
	inOutput := writeFile(t, filepath.Join(env.output, "剧本", "a.MD"), []byte("# 标题"))
	inData := writeFile(t, filepath.Join(env.paths.Root, "notes.json"), []byte(`{"a":1}`))
	outside := writeFile(t, filepath.Join(t.TempDir(), "secret.md"), []byte("secret"))
	image := writeFile(t, filepath.Join(env.output, "x.png"), []byte("png"))
	invalid := writeFile(t, filepath.Join(env.output, "bad.txt"), []byte{0xff, 0xfe})
	huge := filepath.Join(env.output, "huge.txt")
	writeFile(t, huge, nil)
	if err := os.Truncate(huge, MaxTextAssetBytes+1); err != nil {
		t.Fatal(err)
	}

	if text, err := env.svc.readTextAsset(inOutput); err != nil || text != "# 标题" {
		t.Fatalf("output document: %q %v", text, err)
	}
	if text, err := env.svc.readTextAsset(inData); err != nil || text != `{"a":1}` {
		t.Fatalf("data document: %q %v", text, err)
	}
	for path, want := range map[string]string{
		outside: "文档不在应用数据或输出目录中",
		image:   "只允许读取 md、txt、json 文档",
		huge:    "文档超过 20 MiB 大小限制",
	} {
		if _, err := env.svc.readTextAsset(path); errText(err) != want {
			t.Errorf("%s: error %q, want %q", filepath.Base(path), errText(err), want)
		}
	}
	if _, err := env.svc.readTextAsset(filepath.Join(env.output, "missing.md")); !strings.HasPrefix(errText(err), "文档文件不存在或不可访问：") {
		t.Errorf("missing file: %v", err)
	}
	if _, err := env.svc.readTextAsset(invalid); !strings.HasPrefix(errText(err), "读取文档失败") {
		t.Errorf("invalid UTF-8: %v", err)
	}
}

func documentRequest(documentID, text string, expected *string, allowBranch bool) documentVersionRequest {
	model, project := "test-llm", "project-test"
	return documentVersionRequest{
		Label: "视频剧本", Text: text, Model: &model, ProjectID: &project, DocumentID: documentID,
		// Deliberately wrong for branch requests: the backend derives the
		// branch flag from allowBranch.
		Params:              json.RawMessage(`{"documentType":"video_script","videoBranch":false,"big":12345678901234567890}`),
		ExpectedHeadAssetID: expected, AllowBranch: allowBranch,
	}
}

func TestDocumentVersionCASAllowsExactlyOneConcurrentHead(t *testing.T) {
	env := newTestEnv(t)
	start := make(chan struct{})
	var wg sync.WaitGroup
	errs := make([]error, 2)
	for i, text := range []string{"并发版本 A", "并发版本 B"} {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			_, errs[i] = env.svc.saveDocumentVersion(context.Background(), documentRequest("video:script:concurrent", text, nil, false))
		}()
	}
	close(start)
	wg.Wait()
	succeeded, conflicts := 0, 0
	for _, err := range errs {
		switch {
		case err == nil:
			succeeded++
		case strings.Contains(err.Error(), "版本冲突"):
			conflicts++
		default:
			t.Fatalf("unexpected error: %v", err)
		}
	}
	if succeeded != 1 || conflicts != 1 {
		t.Fatalf("succeeded=%d conflicts=%d", succeeded, conflicts)
	}
	rows := env.queryText(t, `SELECT COUNT(*) || '/' || MAX(CAST(json_extract(metadata,'$.params.version') AS INTEGER))
		FROM assets WHERE json_extract(metadata,'$.params.documentId')=?`, "video:script:concurrent")
	if rows[0] != "1/1" {
		t.Fatalf("rows/version = %s", rows[0])
	}
	if n := countFiles(t, env.output); n != 1 {
		t.Fatalf("%d files written, the loser's file must be removed", n)
	}
}

func TestHistoricalBranchDoesNotReplaceTheProductionHead(t *testing.T) {
	env := newTestEnv(t)
	ctx := context.Background()
	first, err := env.svc.saveDocumentVersion(ctx, documentRequest("video:script:branch", "生产版一", nil, false))
	if err != nil {
		t.Fatal(err)
	}
	second, err := env.svc.saveDocumentVersion(ctx, documentRequest("video:script:branch", "生产版二", &first.Asset.ID, false))
	if err != nil {
		t.Fatal(err)
	}
	branch, err := env.svc.saveDocumentVersion(ctx, documentRequest("video:script:branch", "从旧父版本创建的历史分支", &first.Asset.ID, true))
	if err != nil {
		t.Fatal(err)
	}
	if first.Version != 1 || second.Version != 2 || branch.Version != 3 {
		t.Fatalf("versions = %d %d %d", first.Version, second.Version, branch.Version)
	}
	head := env.queryText(t, `SELECT id FROM assets WHERE json_extract(metadata,'$.params.documentId')=?
		AND COALESCE(json_extract(metadata,'$.params.videoBranch'),0) != 1
		ORDER BY CAST(json_extract(metadata,'$.params.version') AS INTEGER) DESC LIMIT 1`, "video:script:branch")
	if head[0] != second.Asset.ID {
		t.Fatalf("production head = %s, want %s", head[0], second.Asset.ID)
	}
	if flag := env.queryText(t, `SELECT CAST(json_extract(metadata,'$.params.videoBranch') AS INTEGER) FROM assets WHERE id=?`, branch.Asset.ID); flag[0] != "1" {
		t.Fatalf("branch flag = %s", flag[0])
	}
	// A save against a stale head is refused.
	if _, err := env.svc.saveDocumentVersion(ctx, documentRequest("video:script:branch", "过期", &first.Asset.ID, false)); errText(err) != errVersionConflict.Error() {
		t.Fatalf("stale head: %v", err)
	}

	// The stored metadata and the returned params carry the backend fields;
	// numbers pass through unchanged.
	metadata := env.queryText(t, `SELECT metadata FROM assets WHERE id=?`, branch.Asset.ID)[0]
	for _, want := range []string{`"model":"test-llm"`, `"projectId":"project-test"`, `"source":"视频剧本"`, `"videoBranch":true`,
		`"version":3`, `"big":12345678901234567890`, `"title":"视频剧本"`, `"documentId":"video:script:branch"`} {
		if !strings.Contains(metadata, want) {
			t.Errorf("metadata %s lacks %s", metadata, want)
		}
	}
	encoded, _ := json.Marshal(branch)
	if !strings.Contains(string(encoded), `"version":3`) || !strings.Contains(string(encoded), `"text":"从旧父版本创建的历史分支"`) {
		t.Errorf("result JSON = %s", encoded)
	}
}

func TestDocumentVersionValidation(t *testing.T) {
	env := newTestEnv(t)
	ctx := context.Background()
	empty := documentRequest(" ", "text", nil, false)
	if _, err := env.svc.saveDocumentVersion(ctx, empty); errText(err) != "文档标题、内容和 documentId 不能为空" {
		t.Fatalf("blank document id: %v", err)
	}
	notObject := documentRequest("doc", "text", nil, false)
	notObject.Params = json.RawMessage(`[1]`)
	if _, err := env.svc.saveDocumentVersion(ctx, notObject); errText(err) != "文档 params 必须是对象" {
		t.Fatalf("array params: %v", err)
	}
	if n := countFiles(t, env.output); n != 0 {
		t.Fatalf("a failed save left %d files", n)
	}
}

func TestPersistAssetsBatchUpsertsThroughTheCommand(t *testing.T) {
	env := newTestEnv(t)
	router := ipc.NewRouter()
	env.svc.Register(router)
	invoke := func(path string) {
		args := `{"assets":[{"id":"a1","kind":"image","path":"` + path + `","width":3,"height":4,"format":"png"}],
			"source":"生成","model":"m","projectId":null,"params":{"prompt":"p<b>"}}`
		result, err := router.Invoke(context.Background(), "persist_assets_batch", json.RawMessage(args))
		if err != nil || result != nil {
			t.Fatalf("persist: %v %v", result, err)
		}
	}
	invoke("/tmp/one.png")
	invoke("/tmp/two.png")
	rows := env.queryText(t, `SELECT path || '|' || width || '|' || format || '|' || metadata FROM assets WHERE id='a1'`)
	if len(rows) != 1 || !strings.HasPrefix(rows[0], "/tmp/two.png|3|png|") {
		t.Fatalf("rows = %v", rows)
	}
	var meta map[string]any
	if err := json.Unmarshal([]byte(strings.SplitN(rows[0], "|", 4)[3]), &meta); err != nil {
		t.Fatal(err)
	}
	if meta["source"] != "生成" || meta["model"] != "m" || meta["projectId"] != nil || meta["params"].(map[string]any)["prompt"] != "p<b>" {
		t.Fatalf("metadata = %v", meta)
	}
}
