package media

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/google/uuid"
	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/platform"
	"imageclient/internal/store"
)

// decodeBase64Limited rejects an oversized payload before allocating the
// decoded buffer.
func decodeBase64Limited(value string, maxDecoded int, tooLarge, invalid string) ([]byte, error) {
	if len(value) > (maxDecoded/3+1)*4 {
		return nil, errors.New(tooLarge)
	}
	data, err := base64.StdEncoding.Strict().DecodeString(value)
	if err != nil {
		return nil, errors.New(invalid)
	}
	return data, nil
}

func optionalString(s string) any {
	if s == "" {
		return nil
	}
	return s
}

// SaveText writes generated text as <output>/剧本/<time>/<uuid>.md. model is
// only logged; an empty model means none.
func (s *Service) SaveText(label, text, model string) (store.AssetRef, error) {
	slog.Info("asset.text_save.start", "label", label, "model", optionalString(model), "textStats", platform.TextStats(text))
	dir := filepath.Join(s.outputDir(), "剧本", runTag(time.Now()))
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return store.AssetRef{}, fmt.Errorf("创建剧本目录失败: %w", err)
	}
	id := uuid.NewString()
	path := filepath.Join(dir, id+".md")
	if err := os.WriteFile(path, []byte(text), 0o644); err != nil {
		return store.AssetRef{}, fmt.Errorf("保存剧本失败: %w", err)
	}
	asset := store.AssetRef{ID: id, Kind: "text", Path: path, Format: "md"}
	slog.Info("asset.text_save.end", "status", "success", "assetId", id, "path", path, "bytes", len(text))
	return asset, nil
}

// SaveMedia stores media the frontend (or the REST API) produced, such as a
// stitched video, under <output>/<图片|视频>/合成/<time>/<uuid>.<ext>.
func (s *Service) SaveMedia(kind, ext string, data []byte) (store.AssetRef, error) {
	if len(data) > MaxMediaAssetBytes {
		return store.AssetRef{}, errors.New("媒体资产超过 512 MiB 大小限制")
	}
	slog.Info("asset.media_save.start", "kind", kind, "extension", ext, "bytes", len(data))
	ext = asciiLower(strings.TrimLeft(ext, "."))
	if ext == "" || strings.IndexFunc(ext, func(r rune) bool { return r > 0x7f || !isASCIIAlnum(byte(r)) }) >= 0 {
		return store.AssetRef{}, errors.New("无效的媒体扩展名")
	}
	var allowed bool
	switch kind {
	case "image":
		allowed = ext == "png" || ext == "jpg" || ext == "jpeg" || ext == "webp" || ext == "gif"
	case "video":
		allowed = ext == "mp4" || ext == "webm" || ext == "mov"
	}
	if !allowed {
		return store.AssetRef{}, fmt.Errorf("不支持的媒体类型或扩展名: %s.%s", kind, ext)
	}
	if kind == "image" && DetectImageFormat(data) == "" {
		return store.AssetRef{}, errors.New("图片资产内容格式校验失败")
	}
	if kind == "video" && !ValidVideo(data, ext) {
		return store.AssetRef{}, fmt.Errorf("视频资产内容格式校验失败: %s", ext)
	}
	dir := filepath.Join(s.outputDir(), kindFolder(kind), "合成", runTag(time.Now()))
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return store.AssetRef{}, fmt.Errorf("创建目录失败: %w", err)
	}
	id := uuid.NewString()
	path := filepath.Join(dir, id+"."+ext)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return store.AssetRef{}, fmt.Errorf("写入文件失败: %w", err)
	}
	slog.Info("asset.media_save.end", "status", "success", "assetId", id, "path", path, "bytes", len(data))
	return store.AssetRef{ID: id, Kind: kind, Path: path, Format: ext}, nil
}

func kindFolder(kind string) string {
	if kind == "video" {
		return "视频"
	}
	return "图片"
}

// readTextAsset returns a md/txt/json document from the data or output dir.
func (s *Service) readTextAsset(path string) (string, error) {
	canonical, err := s.allowedPath(path, "文档文件不存在或不可访问", "文档不在应用数据或输出目录中")
	if err != nil {
		return "", err
	}
	_, ext, _ := splitName(filepath.Base(canonical))
	switch asciiLower(ext) {
	case "md", "txt", "json":
	default:
		return "", errors.New("只允许读取 md、txt、json 文档")
	}
	info, err := os.Stat(canonical)
	if err != nil {
		return "", fmt.Errorf("读取文档信息失败: %w", err)
	}
	if info.Size() > MaxTextAssetBytes {
		return "", errors.New("文档超过 20 MiB 大小限制")
	}
	data, err := os.ReadFile(canonical)
	if err != nil {
		return "", fmt.Errorf("读取文档失败: %w", err)
	}
	if !utf8.Valid(data) {
		return "", errors.New("读取文档失败: 文件不是有效的 UTF-8 文本")
	}
	return string(data), nil
}

type documentVersionRequest struct {
	Label               string          `json:"label"`
	Text                string          `json:"text"`
	Model               *string         `json:"model"`
	ProjectID           *string         `json:"projectId"`
	DocumentID          string          `json:"documentId"`
	Params              json.RawMessage `json:"params"`
	ExpectedHeadAssetID *string         `json:"expectedHeadAssetId"`
	AllowBranch         bool            `json:"allowBranch"`
}

type documentVersionResult struct {
	Asset   store.AssetRef `json:"asset"`
	Params  map[string]any `json:"params"`
	Version int64          `json:"version"`
}

var errVersionConflict = errors.New("版本冲突：当前生产版已变化。请创建历史分支，或迁移到最新版本后再保存")

// saveDocumentVersion writes a new version of a text document. Unless the
// caller is creating a history branch, it succeeds only while the production
// head is still the version the caller edited (optimistic concurrency); the
// head check and the insert share one IMMEDIATE transaction.
func (s *Service) saveDocumentVersion(ctx context.Context, req documentVersionRequest) (documentVersionResult, error) {
	label, text := strings.TrimSpace(req.Label), strings.TrimSpace(req.Text)
	if label == "" || text == "" || strings.TrimSpace(req.DocumentID) == "" {
		return documentVersionResult{}, errors.New("文档标题、内容和 documentId 不能为空")
	}
	model := ""
	if req.Model != nil {
		model = *req.Model
	}
	asset, err := s.SaveText(label, text, model)
	if err != nil {
		return documentVersionResult{}, err
	}
	var (
		params  map[string]any
		version int64
	)
	err = s.db.Tx(ctx, func(conn *sqlite.Conn) error {
		var head *string
		err := sqlitex.Execute(conn, `SELECT id FROM assets WHERE kind='text' AND json_extract(metadata,'$.params.documentId')=?
			AND COALESCE(json_extract(metadata,'$.params.videoBranch'),0) != 1
			ORDER BY CAST(COALESCE(json_extract(metadata,'$.params.version'),0) AS INTEGER) DESC, created_at DESC, id DESC LIMIT 1`,
			&sqlitex.ExecOptions{Args: []any{req.DocumentID}, ResultFunc: func(stmt *sqlite.Stmt) error {
				id := stmt.ColumnText(0)
				head = &id
				return nil
			}})
		if err != nil {
			return fmt.Errorf("读取文档当前版本失败: %w", err)
		}
		if !req.AllowBranch && !sameOptional(req.ExpectedHeadAssetID, head) {
			return errVersionConflict
		}
		err = sqlitex.Execute(conn, `SELECT COALESCE(MAX(CAST(COALESCE(json_extract(metadata,'$.params.version'),0) AS INTEGER)),0)+1
			FROM assets WHERE kind='text' AND json_extract(metadata,'$.params.documentId')=?`,
			&sqlitex.ExecOptions{Args: []any{req.DocumentID}, ResultFunc: func(stmt *sqlite.Stmt) error {
				version = stmt.ColumnInt64(0)
				return nil
			}})
		if err != nil {
			return fmt.Errorf("分配文档版本失败: %w", err)
		}
		object, ok := decodeObject(req.Params)
		if !ok {
			return errors.New("文档 params 必须是对象")
		}
		object["documentId"] = req.DocumentID
		object["version"] = version
		object["text"] = text
		object["title"] = label
		// Branch identity is a backend invariant: params.videoBranch always
		// follows allowBranch, whatever the caller put there.
		object["videoBranch"] = req.AllowBranch
		metadata, err := marshalJSON(map[string]any{
			"source": label, "model": req.Model, "projectId": req.ProjectID, "params": object,
		})
		if err != nil {
			return fmt.Errorf("序列化文档历史失败: %w", err)
		}
		err = sqlitex.Execute(conn, `INSERT INTO assets (id,kind,path,width,height,duration_s,format,created_at,metadata)
			VALUES (?,?,?,NULL,NULL,NULL,?,?,?)`,
			&sqlitex.ExecOptions{Args: []any{asset.ID, asset.Kind, asset.Path, asset.Format, time.Now().UnixMilli(), string(metadata)}})
		if err != nil {
			return fmt.Errorf("写入文档版本失败: %w", err)
		}
		params = object
		return nil
	})
	if err != nil {
		os.Remove(asset.Path)
		return documentVersionResult{}, err
	}
	return documentVersionResult{Asset: asset, Params: params, Version: version}, nil
}

func sameOptional(a, b *string) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}
