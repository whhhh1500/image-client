package media

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io/fs"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"imageclient/internal/config"
	"imageclient/internal/ipc"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

// Size limits shared with the local REST API.
const (
	MaxMediaAssetBytes = 512 << 20
	MaxTextAssetBytes  = 20 << 20
)

// Deps are the services media needs.
type Deps struct {
	Paths  platform.Paths
	Config *config.Store
	DB     *store.DB
	// ComicCatalog lists the files of a project's comic workspace so that
	// asset_publish_media can resolve "comic-md://" sources. It is optional:
	// without it such sources are reported as unknown.
	ComicCatalog func(ctx context.Context, projectID string) ([]ComicMedia, error)
}

// ComicMedia is one comic-workspace catalog entry as media hosting sees it.
type ComicMedia struct {
	SourceURI string
	Kind      string // "image" | "video" | "text"
	Path      string // empty when the entry has no file
}

// Service owns the asset files under the data and output directories.
type Service struct {
	paths        platform.Paths
	cfg          *config.Store
	db           *store.DB
	comicCatalog func(ctx context.Context, projectID string) ([]ComicMedia, error)

	promptlibBase  string // URL prefix the case images are fetched from
	promptlibMu    sync.Mutex
	promptlibLocks map[string]chan struct{}
}

func New(d Deps) *Service {
	return &Service{
		paths:          d.Paths,
		cfg:            d.Config,
		db:             d.DB,
		comicCatalog:   d.ComicCatalog,
		promptlibBase:  "https://" + promptlibImageHost + promptlibImagePrefix,
		promptlibLocks: map[string]chan struct{}{},
	}
}

type pathArgs struct {
	Path string `json:"path"`
}

type persistBatchArgs struct {
	Assets    []store.AssetRef `json:"assets"`
	Source    string           `json:"source"`
	Model     *string          `json:"model"`
	ProjectID *string          `json:"projectId"`
	Params    json.RawMessage  `json:"params"`
}

type saveMediaArgs struct {
	Kind       string `json:"kind"`
	Ext        string `json:"ext"`
	DataBase64 string `json:"dataBase64"`
}

// Register exposes the media commands.
func (s *Service) Register(r *ipc.Router) {
	r.Handle("asset_import_files", ipc.Typed(func(ctx context.Context, in ipc.Input[importRequest]) ([]importedAsset, error) {
		return s.importFiles(ctx, in.Input)
	}))
	r.Handle("persist_assets_batch", ipc.Void(func(ctx context.Context, in persistBatchArgs) error {
		return s.db.PersistAssets(ctx, in.Assets, store.AssetMeta{
			Source: in.Source, Model: in.Model, ProjectID: in.ProjectID, Params: in.Params,
		})
	}))
	r.Handle("save_document_version", ipc.Typed(func(ctx context.Context, in struct {
		Request documentVersionRequest `json:"request"`
	}) (documentVersionResult, error) {
		return s.saveDocumentVersion(ctx, in.Request)
	}))
	r.Handle("read_text_asset", ipc.Typed(func(_ context.Context, in pathArgs) (string, error) {
		return s.readTextAsset(in.Path)
	}))
	r.Handle("save_media_asset", ipc.Typed(func(_ context.Context, in saveMediaArgs) (store.AssetRef, error) {
		data, err := decodeBase64Limited(in.DataBase64, MaxMediaAssetBytes, "媒体资产超过 512 MiB 大小限制", "媒体数据不是有效的 base64")
		if err != nil {
			return store.AssetRef{}, err
		}
		return s.SaveMedia(in.Kind, in.Ext, data)
	}))
	r.Handle("cache_promptlib_image", ipc.Typed(func(ctx context.Context, in struct {
		FileName string `json:"fileName"`
	}) (string, error) {
		return s.cachePromptlibImage(ctx, in.FileName)
	}))
	r.Handle("list_cached_promptlib_images", ipc.Typed(func(context.Context, struct{}) ([]string, error) {
		return s.listCachedPromptlibImages()
	}))
	r.Handle("inspect_image_file", ipc.Typed(func(_ context.Context, in pathArgs) (imageFileInfo, error) {
		return s.inspectImage(in.Path)
	}))
	r.Handle("compress_image_file", ipc.Typed(func(_ context.Context, in struct {
		Path  string `json:"path"`
		Level string `json:"level"`
	}) (store.AssetRef, error) {
		return s.compressImage(in.Path, in.Level)
	}))
	r.Handle("convert_image_file", ipc.Typed(func(_ context.Context, in struct {
		Path   string `json:"path"`
		Format string `json:"format"`
	}) (store.AssetRef, error) {
		return s.convertImage(in.Path, in.Format)
	}))
	r.Handle("media_hosting_get", ipc.Typed(func(ctx context.Context, _ struct{}) (hostingStatus, error) {
		cfg, err := s.hostingConfig(ctx)
		if err != nil {
			return hostingStatus{}, err
		}
		return cfg.status(), nil
	}))
	r.Handle("media_hosting_save", ipc.Typed(func(ctx context.Context, in ipc.Input[hostingSaveInput]) (hostingStatus, error) {
		return s.saveHosting(ctx, in.Input)
	}))
	r.Handle("asset_publish_media", ipc.Typed(func(ctx context.Context, in ipc.Input[publishRequest]) (publishOutput, error) {
		return s.publishMedia(ctx, in.Input)
	}))
}

// outputDir is the configured directory for produced assets.
func (s *Service) outputDir() string { return s.cfg.OutputPath(s.cfg.Get()) }

// canonicalPath resolves path to an absolute path without symlinks; the file
// must exist.
func canonicalPath(path string) (string, error) {
	if path == "" {
		return "", &fs.PathError{Op: "canonicalize", Path: path, Err: fs.ErrNotExist}
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return filepath.EvalSymlinks(abs)
}

// within reports whether path is root or lies below it. filepath.Rel compares
// case-insensitively on Windows.
func within(root, path string) bool {
	rel, err := filepath.Rel(root, path)
	if err != nil || filepath.IsAbs(rel) {
		return false
	}
	return rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

// allowedPath canonicalizes a path the frontend passed and requires it to lie
// in the data directory or the output directory.
func (s *Service) allowedPath(path, missingPrefix, outsideMsg string) (string, error) {
	canonical, err := canonicalPath(path)
	if err != nil {
		return "", fmt.Errorf("%s：%w", missingPrefix, err)
	}
	for _, root := range []string{s.paths.Root, s.outputDir()} {
		if resolved, err := canonicalPath(root); err == nil && within(resolved, canonical) {
			return canonical, nil
		}
	}
	return "", fmt.Errorf("%s", outsideMsg)
}

// splitName splits a file name like Rust's Path::file_stem/extension: a
// leading dot does not start an extension.
func splitName(name string) (stem, ext string, hasExt bool) {
	i := strings.LastIndexByte(name, '.')
	if i <= 0 {
		return name, "", false
	}
	return name[:i], name[i+1:], true
}

func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if c >= 'A' && c <= 'Z' {
			b[i] = c + 'a' - 'A'
		}
	}
	return string(b)
}

func isASCIIAlnum(c byte) bool {
	return c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9'
}

// runTag names a per-save folder, in local time.
func runTag(now time.Time) string { return now.Format("20060102-150405") }

// variantStamp is the millisecond timestamp in variant file names.
func variantStamp(now time.Time) string {
	return now.Format("20060102150405") + fmt.Sprintf("%03d", now.Nanosecond()/int(time.Millisecond))
}

// marshalJSON encodes v the way the Rust backend did (no HTML escaping), so
// stored JSON stays byte-compatible.
func marshalJSON(v any) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// decodeObject parses raw as a JSON object, keeping numbers verbatim.
func decodeObject(raw json.RawMessage) (map[string]any, bool) {
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	var value any
	if err := dec.Decode(&value); err != nil {
		return nil, false
	}
	object, ok := value.(map[string]any)
	return object, ok && object != nil
}
