package media

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
)

func importEnv(t *testing.T) testEnv {
	t.Helper()
	env := newTestEnv(t)
	env.exec(t, `INSERT INTO settings(key,value) VALUES('projects',?)`, `["legacy",{"id":"project-a","name":"A"}]`)
	return env
}

func importRequestFor(t *testing.T, files ...importFile) importRequest {
	t.Helper()
	if len(files) == 0 {
		files = []importFile{bytesFile("source.png", pngBytes(t, testImage(1, 1)))}
	}
	return importRequest{
		ProjectID: " project-a ", ImportEntry: "image_reference", UploadBatchID: "batch-a",
		Files: files, Params: json.RawMessage(`{"scope":"reference","n":1.50}`),
	}
}

func bytesFile(name string, data []byte) importFile {
	return importFile{Source: "bytes", FileName: name, DataBase64: base64.StdEncoding.EncodeToString(data)}
}

func importRoot(env testEnv) string { return filepath.Join(env.paths.Assets(), "外部导入") }

func TestImportPersistsExternalAssetWithCatalogMetadata(t *testing.T) {
	env := importEnv(t)
	imported, err := env.svc.importFiles(context.Background(), importRequestFor(t))
	if err != nil {
		t.Fatal(err)
	}
	if len(imported) != 1 {
		t.Fatalf("imported %d assets", len(imported))
	}
	item := imported[0]
	rel, _ := filepath.Rel(importRoot(env), item.Asset.Path)
	if !regexp.MustCompile(`^图片[/\\]`+uuidName+`\.png$`).MatchString(rel) || item.Asset.Format != "png" || item.Asset.Kind != "image" {
		t.Fatalf("asset %+v", item.Asset)
	}
	catalog := item.Params["catalog"].(map[string]any)
	if catalog["category"] != "upload" || catalog["group"].(map[string]any)["id"] != "batch-a" ||
		item.Params["originalFileName"] != "source.png" || item.ProjectID != "project-a" || item.Source != "source.png" || item.CreatedAt == 0 {
		t.Fatalf("imported %+v", item)
	}
	metadata := env.queryText(t, `SELECT metadata FROM assets WHERE id=?`, item.Asset.ID)[0]
	var stored map[string]any
	if err := json.Unmarshal([]byte(metadata), &stored); err != nil {
		t.Fatal(err)
	}
	params := stored["params"].(map[string]any)
	if stored["projectId"] != "project-a" || stored["source"] != "source.png" || params["origin"] != "external_upload" ||
		params["importEntry"] != "image_reference" || params["uploadBatchId"] != "batch-a" || params["scope"] != "reference" {
		t.Fatalf("metadata %s", metadata)
	}
	if _, hasModel := stored["model"]; hasModel || !strings.Contains(metadata, `"n":1.50`) {
		t.Fatalf("metadata %s", metadata)
	}
}

func TestImportRejectsInvalidBatchesBeforeWritingFiles(t *testing.T) {
	env := importEnv(t)
	png := pngBytes(t, testImage(1, 1))
	dir := t.TempDir()
	cases := []struct {
		name   string
		mutate func(*importRequest)
		want   string
	}{
		{"unknown project", func(r *importRequest) { r.ProjectID = "missing" }, "项目不存在，不能导入资产"},
		{"blank project", func(r *importRequest) { r.ProjectID = " " }, "导入资产必须指定项目"},
		{"no files", func(r *importRequest) { r.Files = nil }, "一次请选择 1 到 8 个文件"},
		{"too many files", func(r *importRequest) { r.Files = make([]importFile, 9) }, "一次请选择 1 到 8 个文件"},
		{"unknown entry", func(r *importRequest) { r.ImportEntry = "elsewhere" }, "未知的外部导入入口"},
		{"blank batch", func(r *importRequest) { r.UploadBatchID = "  " }, "导入批次 ID 不能为空"},
		{"null params", func(r *importRequest) { r.Params = json.RawMessage(`null`) }, "导入资产 params 必须是对象"},
		{"missing params", func(r *importRequest) { r.Params = nil }, "导入资产 params 必须是对象"},
		{"blank file name", func(r *importRequest) { r.Files = []importFile{bytesFile(" ", png)} }, "导入文件名不能为空"},
		{"invalid base64", func(r *importRequest) {
			r.Files = []importFile{{Source: "bytes", FileName: "a.png", DataBase64: "@@@@"}}
		}, "导入文件内容不是有效的 base64"},
		{"directory path", func(r *importRequest) { r.Files = []importFile{{Source: "path", Path: dir}} }, "导入目标必须是文件"},
		{"magic number only", func(r *importRequest) {
			r.Files = []importFile{bytesFile("truncated.png", []byte("\x89PNG\r\n\x1a\n"))}
		}, "图片“truncated.png”内容格式校验失败"},
		{"image disguised as video", func(r *importRequest) {
			r.ImportEntry = "short_drama_video_reference"
			r.Files = []importFile{bytesFile("not-video.mp4", png)}
		}, "short_drama_video_reference 只接受实际视频文件，未导入“not-video.mp4”"},
		{"video into image entry", func(r *importRequest) {
			r.Files = []importFile{bytesFile("clip.mp4", []byte("\x00\x00\x00\x18ftypisom"))}
		}, "image_reference 只接受实际图片文件，未导入“clip.mp4”"},
		{"unsupported content", func(r *importRequest) {
			r.ImportEntry = "asset_library_upload"
			r.Files = []importFile{bytesFile("clip.webm", []byte("\x00\x00\x00\x18ftypisom"))}
		}, unsupportedImportMsg},
		{"one bad file sinks the batch", func(r *importRequest) {
			r.Files = []importFile{bytesFile("ok.png", png), bytesFile("bad.txt", []byte("text"))}
		}, unsupportedImportMsg},
	}
	for _, tc := range cases {
		req := importRequestFor(t)
		tc.mutate(&req)
		if _, err := env.svc.importFiles(context.Background(), req); errText(err) != tc.want {
			t.Errorf("%s: error %q, want %q", tc.name, errText(err), tc.want)
		}
	}
	if _, err := os.Stat(importRoot(env)); !os.IsNotExist(err) {
		t.Fatalf("a rejected import wrote files: %v", err)
	}
	if rows := env.queryText(t, `SELECT COUNT(*) FROM assets`); rows[0] != "0" {
		t.Fatalf("%s assets registered", rows[0])
	}
}

func TestImportReadsPathsAndClassifiesVideosByContent(t *testing.T) {
	env := importEnv(t)
	video := writeFile(t, filepath.Join(t.TempDir(), "Clip.MP4"), []byte("\x00\x00\x00\x18ftypisom-video"))
	image := writeFile(t, filepath.Join(t.TempDir(), "photo.jpeg"), pngBytes(t, testImage(2, 2)))
	req := importRequestFor(t, importFile{Source: "path", Path: video}, importFile{Source: "path", Path: image})
	req.ImportEntry = "asset_library_upload"
	imported, err := env.svc.importFiles(context.Background(), req)
	if err != nil {
		t.Fatal(err)
	}
	if imported[0].Asset.Kind != "video" || imported[0].Asset.Format != "mp4" || imported[0].Source != "Clip.MP4" ||
		filepath.Base(filepath.Dir(imported[0].Asset.Path)) != "视频" {
		t.Fatalf("video import %+v", imported[0])
	}
	// The format comes from the content, not the name.
	if imported[1].Asset.Format != "png" || filepath.Base(filepath.Dir(imported[1].Asset.Path)) != "图片" {
		t.Fatalf("image import %+v", imported[1])
	}
	if imported[0].CreatedAt != imported[1].CreatedAt {
		t.Fatal("one batch shares one timestamp")
	}
}

func TestImportFileJSONShapes(t *testing.T) {
	var file importFile
	if err := json.Unmarshal([]byte(`{"source":"bytes","fileName":"browser.png","dataBase64":"AAEC"}`), &file); err != nil {
		t.Fatal(err)
	}
	if file.Source != "bytes" || file.FileName != "browser.png" || file.DataBase64 != "AAEC" {
		t.Fatalf("decoded %+v", file)
	}
	if err := json.Unmarshal([]byte(`{"source":"path","path":"C:/a.png"}`), &file); err != nil || file.Path != "C:/a.png" {
		t.Fatalf("path shape: %+v %v", file, err)
	}
	if err := json.Unmarshal([]byte(`{"source":"url","url":"https://x"}`), &file); err == nil {
		t.Fatal("an unknown source must be rejected")
	}
}

func TestImportRemovesItsFilesWhenRegistrationFails(t *testing.T) {
	env := importEnv(t)
	env.exec(t, `CREATE TRIGGER fail_import BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT, 'boom'); END`)
	png := pngBytes(t, testImage(1, 1))
	req := importRequestFor(t, bytesFile("a.png", png), bytesFile("b.png", png))
	if _, err := env.svc.importFiles(context.Background(), req); !strings.HasPrefix(errText(err), "登记外部资产失败：") {
		t.Fatalf("error = %v", err)
	}
	if n := countFiles(t, importRoot(env)); n != 0 {
		t.Fatalf("%d files left behind", n)
	}
}
