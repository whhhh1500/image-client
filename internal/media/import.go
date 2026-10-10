package media

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/store"
)

// External imports copy user-picked files into assets/外部导入/<图片|视频>/
// and register them as project assets in one step.
const (
	maxImportFiles       = 8
	maxImageImportBytes  = 50 << 20
	maxVideoImportBytes  = 512 << 20
	maxImportBatchBytes  = maxVideoImportBytes // eight 512 MiB videos must not be read at once
	importTooLarge       = "导入文件超过 512 MiB 大小限制"
	importBatchTooLarge  = "导入批次超过 512 MiB 大小限制"
	unsupportedImportMsg = "仅支持内容校验通过的 PNG、JPG、WebP、GIF、BMP、MP4、WebM 或 MOV 文件"
)

type importRequest struct {
	ProjectID     string          `json:"projectId"`
	ImportEntry   string          `json:"importEntry"`
	UploadBatchID string          `json:"uploadBatchId"`
	Files         []importFile    `json:"files"`
	Params        json.RawMessage `json:"params"`
}

// importFile is a picked path or, from a browser drop, the file's bytes.
type importFile struct {
	Source     string `json:"source"` // "path" | "bytes"
	Path       string `json:"path"`
	FileName   string `json:"fileName"`
	DataBase64 string `json:"dataBase64"`
}

func (f *importFile) UnmarshalJSON(b []byte) error {
	type plain importFile
	var v plain
	if err := json.Unmarshal(b, &v); err != nil {
		return err
	}
	if v.Source != "path" && v.Source != "bytes" {
		return fmt.Errorf("未知的导入文件来源: %q", v.Source)
	}
	*f = importFile(v)
	return nil
}

type importedAsset struct {
	Asset     store.AssetRef `json:"asset"`
	Source    string         `json:"source"`
	ProjectID string         `json:"projectId"`
	Params    map[string]any `json:"params"`
	CreatedAt int64          `json:"createdAt"`
}

// pendingImport is a read and validated file awaiting its write.
type pendingImport struct {
	source string // original file name
	data   []byte
	kind   string
	format string
}

// importKinds lists, per entry point, the kinds of file it accepts.
var importKinds = map[string][]string{
	"image_reference":             {"image"},
	"prompt_library_reference":    {"image"},
	"short_drama_video_reference": {"video"},
	"video_reference":             {"image", "video"},
	"asset_library_upload":        {"image", "video"},
}

func (s *Service) importFiles(ctx context.Context, req importRequest) ([]importedAsset, error) {
	imported, err := s.runImport(ctx, req)
	if err != nil {
		slog.Warn("asset.external_import.failed", "projectId", req.ProjectID, "importEntry", req.ImportEntry, "error", err.Error())
		return nil, err
	}
	ids := make([]string, len(imported))
	for i, item := range imported {
		ids[i] = item.Asset.ID
	}
	slog.Info("asset.external_import.end", "projectId", req.ProjectID, "importEntry", req.ImportEntry, "assetIds", ids)
	return imported, nil
}

// runImport validates and reads every file before touching the library, so a
// rejected batch leaves nothing behind; files are written without holding a
// database connection and registered in one transaction.
func (s *Service) runImport(ctx context.Context, req importRequest) ([]importedAsset, error) {
	req.ProjectID = strings.TrimSpace(req.ProjectID)
	req.ImportEntry = strings.TrimSpace(req.ImportEntry)
	req.UploadBatchID = strings.TrimSpace(req.UploadBatchID)
	if len(req.Files) == 0 || len(req.Files) > maxImportFiles {
		return nil, fmt.Errorf("一次请选择 1 到 %d 个文件", maxImportFiles)
	}
	kinds, ok := importKinds[req.ImportEntry]
	if !ok {
		return nil, errors.New("未知的外部导入入口")
	}
	if req.UploadBatchID == "" {
		return nil, errors.New("导入批次 ID 不能为空")
	}
	params, ok := decodeObject(req.Params)
	if !ok {
		return nil, errors.New("导入资产 params 必须是对象")
	}
	params["catalog"] = map[string]any{
		"version": 1, "category": "upload", "origin": "local_upload",
		"group": map[string]any{"type": "upload_batch", "id": req.UploadBatchID},
	}
	params["origin"] = "external_upload"
	params["importEntry"] = req.ImportEntry
	params["uploadBatchId"] = req.UploadBatchID

	// Preflight sizes from metadata so an oversized selection is rejected
	// before anything is read.
	var batch int64
	for _, file := range req.Files {
		size, err := declaredSize(file)
		if err != nil {
			return nil, err
		}
		if size > maxVideoImportBytes {
			return nil, errors.New(importTooLarge)
		}
		if batch += size; batch > maxImportBatchBytes {
			return nil, errors.New(importBatchTooLarge)
		}
	}
	read := make([]pendingImport, len(req.Files))
	for i, file := range req.Files {
		source, data, err := readImportFile(file)
		if err != nil {
			return nil, err
		}
		read[i] = pendingImport{source: source, data: data}
	}
	for i := range read {
		if err := classifyImport(&read[i]); err != nil {
			return nil, err
		}
	}
	for _, item := range read {
		if !slices.Contains(kinds, item.kind) {
			required := "图片或视频"
			if len(kinds) == 1 {
				required = kindLabel(kinds[0])
			}
			return nil, fmt.Errorf("%s 只接受实际%s文件，未导入“%s”", req.ImportEntry, required, item.source)
		}
	}

	// Reject an unknown project before writing anything.
	if err := s.db.With(ctx, func(conn *sqlite.Conn) error { return verifyProject(conn, req.ProjectID) }); err != nil {
		return nil, err
	}
	destination := filepath.Join(s.paths.Assets(), "外部导入")
	written := make([]store.AssetRef, 0, len(read))
	removeWritten := func() {
		for _, asset := range written {
			os.Remove(asset.Path)
		}
	}
	for _, item := range read {
		asset, err := SaveBytes(filepath.Join(destination, kindLabel(item.kind)), item.kind, item.data, item.format)
		if err != nil {
			removeWritten()
			return nil, err
		}
		written = append(written, asset)
	}

	createdAt := time.Now().UnixMilli()
	result := make([]importedAsset, len(written))
	for i, asset := range written {
		assetParams := maps.Clone(params)
		assetParams["originalFileName"] = read[i].source
		result[i] = importedAsset{Asset: asset, Source: read[i].source, ProjectID: req.ProjectID, Params: assetParams, CreatedAt: createdAt}
	}
	err := s.db.Tx(ctx, func(conn *sqlite.Conn) error {
		// The project may have been deleted while the files were written.
		if err := verifyProject(conn, req.ProjectID); err != nil {
			return err
		}
		for _, item := range result {
			metadata, err := marshalJSON(map[string]any{"source": item.Source, "projectId": item.ProjectID, "params": item.Params})
			if err != nil {
				return fmt.Errorf("登记外部资产失败：%w", err)
			}
			err = sqlitex.Execute(conn, `INSERT INTO assets(id,kind,path,width,height,duration_s,format,created_at,metadata)
				VALUES(?,?,?,NULL,NULL,NULL,?,?,?)`,
				&sqlitex.ExecOptions{Args: []any{item.Asset.ID, item.Asset.Kind, item.Asset.Path, item.Asset.Format, createdAt, string(metadata)}})
			if err != nil {
				return fmt.Errorf("登记外部资产失败：%w", err)
			}
		}
		return nil
	})
	if err != nil {
		removeWritten()
		return nil, err
	}
	return result, nil
}

func kindLabel(kind string) string {
	if kind == "video" {
		return "视频"
	}
	return "图片"
}

// declaredSize is a file's size without reading it; for base64 payloads it
// approximates the decoded length (the exact check follows decoding).
func declaredSize(file importFile) (int64, error) {
	if file.Source == "bytes" {
		if strings.TrimSpace(file.FileName) == "" {
			return 0, errors.New("导入文件名不能为空")
		}
		return int64(len(file.DataBase64) / 4 * 3), nil
	}
	_, size, err := statImportPath(file.Path)
	return size, err
}

func statImportPath(path string) (string, int64, error) {
	canonical, err := canonicalPath(path)
	if err != nil {
		return "", 0, fmt.Errorf("读取导入文件失败：%w", err)
	}
	info, err := os.Stat(canonical)
	if err != nil {
		return "", 0, fmt.Errorf("读取导入文件信息失败：%w", err)
	}
	if !info.Mode().IsRegular() {
		return "", 0, errors.New("导入目标必须是文件")
	}
	return canonical, info.Size(), nil
}

// readImportFile returns the file's display name and contents.
func readImportFile(file importFile) (string, []byte, error) {
	if file.Source == "bytes" {
		if strings.TrimSpace(file.FileName) == "" {
			return "", nil, errors.New("导入文件名不能为空")
		}
		data, err := decodeBase64Limited(file.DataBase64, maxVideoImportBytes, importTooLarge, "导入文件内容不是有效的 base64")
		if err != nil {
			return "", nil, err
		}
		if len(data) > maxVideoImportBytes {
			return "", nil, errors.New(importTooLarge)
		}
		return file.FileName, data, nil
	}
	canonical, size, err := statImportPath(file.Path)
	if err != nil {
		return "", nil, err
	}
	if size > maxVideoImportBytes {
		return "", nil, errors.New(importTooLarge)
	}
	data, err := os.ReadFile(canonical)
	if err != nil {
		return "", nil, fmt.Errorf("读取导入文件失败：%w", err)
	}
	name := filepath.Base(canonical)
	if strings.TrimSpace(name) == "" {
		name = "外部文件"
	}
	return name, data, nil
}

// classifyImport decides the kind from the content: a decodable image of a
// known format, or a video container whose name and signature agree.
func classifyImport(item *pendingImport) error {
	if format := DetectImageFormat(item.data); format != "" {
		if len(item.data) > maxImageImportBytes {
			return fmt.Errorf("图片“%s”超过 50 MiB 大小限制", item.source)
		}
		if ValidateImage(item.data) != nil {
			return fmt.Errorf("图片“%s”内容格式校验失败", item.source)
		}
		item.kind, item.format = "image", format
		return nil
	}
	ext := ""
	if dot := strings.LastIndexByte(item.source, '.'); dot >= 0 {
		ext = asciiLower(item.source[dot+1:])
	}
	if (ext == "mp4" || ext == "webm" || ext == "mov") && ValidVideo(item.data, ext) {
		item.kind, item.format = "video", ext
		return nil
	}
	return errors.New(unsupportedImportMsg)
}

// verifyProject requires projectID in the frontend's project list.
func verifyProject(conn *sqlite.Conn, projectID string) error {
	if strings.TrimSpace(projectID) == "" {
		return errors.New("导入资产必须指定项目")
	}
	var raw string
	found := false
	err := sqlitex.Execute(conn, "SELECT value FROM settings WHERE key='projects'", &sqlitex.ExecOptions{
		ResultFunc: func(stmt *sqlite.Stmt) error {
			raw, found = stmt.ColumnText(0), true
			return nil
		},
	})
	if err != nil || !found {
		return errors.New("项目不存在，不能导入资产")
	}
	var projects []any
	if json.Unmarshal([]byte(raw), &projects) == nil {
		for _, project := range projects {
			if fields, ok := project.(map[string]any); ok && fields["id"] == projectID {
				return nil
			}
		}
	}
	return errors.New("项目不存在，不能导入资产")
}
