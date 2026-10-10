// Package media manages asset files: saving generated output, validating and
// converting images, importing user files, text documents, the prompt-library
// image cache and publishing media to a hosting endpoint.
package media

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"image"
	_ "image/gif"
	_ "image/jpeg"
	_ "image/png"
	"io"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/google/uuid"
	_ "golang.org/x/image/bmp"
	_ "golang.org/x/image/webp"

	"imageclient/internal/store"
)

// DetectImageFormat recognises PNG, JPEG, WebP, GIF and BMP by signature and
// returns the file extension, or "" for anything else.
func DetectImageFormat(b []byte) string {
	switch {
	case bytes.HasPrefix(b, []byte("\x89PNG\r\n\x1a\n")):
		return "png"
	case len(b) >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF:
		return "jpg"
	case len(b) >= 12 && bytes.HasPrefix(b, []byte("RIFF")) && string(b[8:12]) == "WEBP":
		return "webp"
	case bytes.HasPrefix(b, []byte("GIF87a")) || bytes.HasPrefix(b, []byte("GIF89a")):
		return "gif"
	case bytes.HasPrefix(b, []byte("BM")):
		return "bmp"
	}
	return ""
}

// Decode bounds: a small file can declare billions of pixels, so dimensions
// are checked from the header before any pixel buffer is allocated.
const (
	MaxImageDimension = 16384
	maxImagePixels    = 1 << 30 / 4 // 1 GiB of RGBA
)

// DecodeImage decodes b after checking its declared size.
func DecodeImage(b []byte) (image.Image, string, error) {
	cfg, format, err := image.DecodeConfig(bytes.NewReader(b))
	if err != nil {
		return nil, "", fmt.Errorf("识别图片格式失败: %w", err)
	}
	if cfg.Width <= 0 || cfg.Height <= 0 || cfg.Width > MaxImageDimension || cfg.Height > MaxImageDimension ||
		int64(cfg.Width)*int64(cfg.Height) > maxImagePixels {
		return nil, "", fmt.Errorf("解码图片失败: 图片尺寸 %dx%d 超出限制", cfg.Width, cfg.Height)
	}
	img, format, err := image.Decode(bytes.NewReader(b))
	if err != nil {
		return nil, "", fmt.Errorf("解码图片失败: %w", err)
	}
	return img, format, nil
}

// ValidateImage decodes and discards, for paths that only need to know the
// content is a real, bounded image.
func ValidateImage(b []byte) error {
	_, _, err := DecodeImage(b)
	return err
}

// ValidVideo checks the container signature for mp4/mov (ftyp box) or webm (EBML).
func ValidVideo(b []byte, ext string) bool {
	switch ext {
	case "mp4", "mov":
		return len(b) >= 12 && string(b[4:8]) == "ftyp"
	case "webm":
		return bytes.HasPrefix(b, []byte{0x1A, 0x45, 0xDF, 0xA3})
	}
	return false
}

// SaveBytes writes data to dir/<uuid>.<format> and describes it as an asset.
func SaveBytes(dir, kind string, data []byte, format string) (store.AssetRef, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return store.AssetRef{}, fmt.Errorf("创建资产目录失败: %w", err)
	}
	id := uuid.NewString()
	path := filepath.Join(dir, id+"."+format)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return store.AssetRef{}, fmt.Errorf("写入资产失败: %w", err)
	}
	return store.AssetRef{ID: id, Kind: kind, Path: path, Format: format}, nil
}

type hashEntry struct {
	modTime time.Time
	size    int64
	digest  string
}

var (
	hashMu    sync.Mutex
	hashCache = map[string]hashEntry{}
)

// FileSHA256 returns the lowercase hex SHA-256 of a regular file no larger
// than maxBytes, cached while its size and modification time are unchanged.
func FileSHA256(path string, maxBytes int64) (string, bool) {
	info, err := os.Stat(path)
	if err != nil || !info.Mode().IsRegular() || info.Size() > maxBytes {
		return "", false
	}
	hashMu.Lock()
	cached, ok := hashCache[path]
	hashMu.Unlock()
	if ok && cached.size == info.Size() && cached.modTime.Equal(info.ModTime()) {
		return cached.digest, true
	}
	file, err := os.Open(path)
	if err != nil {
		return "", false
	}
	defer file.Close()
	sum := sha256.New()
	if n, err := io.Copy(sum, io.LimitReader(file, maxBytes+1)); err != nil || n > maxBytes {
		return "", false
	}
	digest := hex.EncodeToString(sum.Sum(nil))
	hashMu.Lock()
	hashCache[path] = hashEntry{info.ModTime(), info.Size(), digest}
	hashMu.Unlock()
	return digest, true
}
