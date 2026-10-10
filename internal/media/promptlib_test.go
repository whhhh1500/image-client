package media

import (
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestPromptlibFileNameAcceptsOnlyCaseImages(t *testing.T) {
	for input, want := range map[string]string{
		"case1.jpg":           "case1.jpg",
		"/images/case334.png": "case334.png",
		` C:\x\CASE7.WEBP `:   "case7.webp",
		"case12.jpeg":         "case12.jpeg",
	} {
		if got, err := promptlibFileName(input); err != nil || got != want {
			t.Errorf("promptlibFileName(%q) = %q, %v; want %q", input, got, err, want)
		}
	}
	for input, want := range map[string]string{
		"../secret.jpg": "无效的案例图文件名",
		"case1.exe":     "不支持的案例图格式",
		"banner.svg":    "不支持的案例图格式",
		"case.png":      "无效的案例图文件名",
		"case1a.png":    "无效的案例图文件名",
		"case1":         "无效的案例图文件名",
		"":              "无效的案例图文件名",
	} {
		if _, err := promptlibFileName(input); errText(err) != want {
			t.Errorf("promptlibFileName(%q) error %q, want %q", input, errText(err), want)
		}
	}
}

func TestCachePromptlibImageDownloadsEachFileOnce(t *testing.T) {
	env := newTestEnv(t)
	image := pngBytes(t, testImage(3, 3))
	var hits atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		if r.URL.Path != "/img-case-assets/images/case9.png" {
			http.NotFound(w, r)
			return
		}
		time.Sleep(50 * time.Millisecond)
		w.Write(image)
	}))
	defer server.Close()
	env.svc.promptlibBase = server.URL + promptlibImagePrefix

	var wg sync.WaitGroup
	paths := make([]string, 6)
	errs := make([]error, 6)
	for i := range paths {
		wg.Add(1)
		go func() {
			defer wg.Done()
			paths[i], errs[i] = env.svc.cachePromptlibImage(context.Background(), "images/CASE9.png")
		}()
	}
	wg.Wait()
	want := filepath.Join(env.paths.PromptlibImages(), "case9.png")
	for i := range paths {
		if errs[i] != nil || paths[i] != want {
			t.Fatalf("call %d: %q %v", i, paths[i], errs[i])
		}
	}
	if hits.Load() != 1 {
		t.Fatalf("downloaded %d times, want once", hits.Load())
	}
	if data, _ := os.ReadFile(want); string(data) != string(image) {
		t.Fatal("cached file differs from the download")
	}
	if listed, err := env.svc.listCachedPromptlibImages(); err != nil || !slices.Equal(listed, []string{want}) {
		t.Fatalf("listed %v %v", listed, err)
	}
}

func TestCachePromptlibImageRejectsBadDownloads(t *testing.T) {
	env := newTestEnv(t)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch filepath.Base(r.URL.Path) {
		case "case1.png":
			http.NotFound(w, r)
		case "case2.png":
			w.Write([]byte("<html>not an image</html>"))
		case "case3.png":
			w.Write(make([]byte, maxPromptlibImageBytes+1))
		}
	}))
	defer server.Close()
	env.svc.promptlibBase = server.URL + "/"
	for name, want := range map[string]string{
		"case1.png": "下载案例图失败（404 Not Found）",
		"case2.png": "案例图内容格式校验失败",
		"case3.png": "案例图超过 8 MiB 大小限制",
		"case4.svg": "不支持的案例图格式",
	} {
		if _, err := env.svc.cachePromptlibImage(context.Background(), name); errText(err) != want {
			t.Errorf("%s: error %q, want %q", name, errText(err), want)
		}
	}
	if n := countFiles(t, env.paths.PromptlibImages()); n != 0 {
		t.Fatalf("failed downloads left %d files", n)
	}
}

func TestListCachedPromptlibImagesSkipsForeignFiles(t *testing.T) {
	env := newTestEnv(t)
	if err := os.RemoveAll(env.paths.PromptlibImages()); err != nil {
		t.Fatal(err)
	}
	if listed, err := env.svc.listCachedPromptlibImages(); err != nil || listed == nil || len(listed) != 0 {
		t.Fatalf("missing dir: %#v %v", listed, err)
	}
	dir := env.paths.PromptlibImages()
	for _, name := range []string{"case1.jpg", "CASE2.PNG", "banner.svg", "case3.jpg.part-123", "notes.txt"} {
		writeFile(t, filepath.Join(dir, name), []byte("x"))
	}
	os.Mkdir(filepath.Join(dir, "case4.png"), 0o755)
	listed, err := env.svc.listCachedPromptlibImages()
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, path := range listed {
		names = append(names, filepath.Base(path))
	}
	slices.Sort(names)
	if strings.Join(names, ",") != "CASE2.PNG,case1.jpg" {
		t.Fatalf("listed %v", names)
	}
}
