package media

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"imageclient/internal/platform"
)

// Prompt-library case images are downloaded once from a fixed public bucket
// and served from promptlib-images/ afterwards.
const (
	promptlibImageHost     = "pub-7ecb2a3a62b94375a9abd336abf0bcc6.r2.dev"
	promptlibImagePrefix   = "/img-case-assets/images/"
	maxPromptlibImageBytes = 8 << 20
	promptlibTimeout       = 30 * time.Second
)

// promptlibFileName reduces a name or path to "case<digits>.<image ext>",
// lower-cased; anything else is rejected, so only the bucket's case images
// can be requested and cached.
func promptlibFileName(fileName string) (string, error) {
	name := strings.ReplaceAll(strings.TrimSpace(fileName), `\`, "/")
	name = asciiLower(name[strings.LastIndexByte(name, '/')+1:])
	dot := strings.LastIndexByte(name, '.')
	if dot < 0 {
		return "", errors.New("无效的案例图文件名")
	}
	stem, ext := name[:dot], name[dot+1:]
	switch ext {
	case "jpg", "jpeg", "png", "webp", "gif":
	default:
		return "", errors.New("不支持的案例图格式")
	}
	if !strings.HasPrefix(stem, "case") || len(stem) <= 4 || !allDigits(stem[4:]) {
		return "", errors.New("无效的案例图文件名")
	}
	return name, nil
}

// lockPromptlib serialises downloads of one file, so concurrent requests for
// the same image fetch it once.
func (s *Service) lockPromptlib(ctx context.Context, name string) (func(), error) {
	s.promptlibMu.Lock()
	lock, ok := s.promptlibLocks[name]
	if !ok {
		lock = make(chan struct{}, 1)
		s.promptlibLocks[name] = lock
	}
	s.promptlibMu.Unlock()
	select {
	case lock <- struct{}{}:
		return func() { <-lock }, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// cachePromptlibImage returns the local path of a case image, downloading it
// on first use.
func (s *Service) cachePromptlibImage(ctx context.Context, fileName string) (string, error) {
	name, err := promptlibFileName(fileName)
	if err != nil {
		return "", err
	}
	dest := filepath.Join(s.paths.PromptlibImages(), name)
	unlock, err := s.lockPromptlib(ctx, name)
	if err != nil {
		return "", err
	}
	defer unlock()
	if info, err := os.Stat(dest); err == nil && info.Mode().IsRegular() && info.Size() > 0 {
		return dest, nil
	}
	if err := os.MkdirAll(s.paths.PromptlibImages(), 0o755); err != nil {
		return "", fmt.Errorf("创建案例图缓存目录失败：%w", err)
	}

	url := s.promptlibBase + name
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return "", fmt.Errorf("下载案例图失败：%w", err)
	}
	resp, err := platform.Do(ctx, platform.ClientFor(url), req, platform.Timeouts{Total: promptlibTimeout})
	if err != nil {
		return "", fmt.Errorf("下载案例图失败：%w", err)
	}
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		resp.Body.Close()
		return "", fmt.Errorf("下载案例图失败（%s）", resp.Status)
	}
	data, err := platform.ReadAllLimited(resp, maxPromptlibImageBytes, "案例图", nil)
	if err != nil {
		var tooLarge *platform.TooLargeError
		if errors.As(err, &tooLarge) {
			return "", errors.New("案例图超过 8 MiB 大小限制")
		}
		return "", fmt.Errorf("读取案例图失败：%w", err)
	}
	if DetectImageFormat(data) == "" {
		return "", errors.New("案例图内容格式校验失败")
	}
	tmp := filepath.Join(s.paths.PromptlibImages(), fmt.Sprintf("%s.part-%d", name, os.Getpid()))
	if err := os.WriteFile(tmp, data, 0o644); err != nil {
		return "", fmt.Errorf("写入案例图缓存失败：%w", err)
	}
	if err := os.Rename(tmp, dest); err != nil {
		os.Remove(tmp)
		return "", fmt.Errorf("保存案例图缓存失败：%w", err)
	}
	slog.Info("promptlib.image_cached", "fileName", name, "bytes", len(data), "path", dest)
	return dest, nil
}

// listCachedPromptlibImages lists the case images already on disk, so the
// UI can skip the network for them.
func (s *Service) listCachedPromptlibImages() ([]string, error) {
	paths := []string{}
	entries, err := os.ReadDir(s.paths.PromptlibImages())
	if errors.Is(err, os.ErrNotExist) {
		return paths, nil
	}
	if err != nil {
		return nil, fmt.Errorf("读取案例图缓存失败：%w", err)
	}
	for _, entry := range entries {
		path := filepath.Join(s.paths.PromptlibImages(), entry.Name())
		if info, err := os.Stat(path); err != nil || !info.Mode().IsRegular() {
			continue
		}
		if _, err := promptlibFileName(entry.Name()); err == nil {
			paths = append(paths, path)
		}
	}
	return paths, nil
}
