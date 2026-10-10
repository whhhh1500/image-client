package app

import (
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// fallthroughHandler stands in for the rest of the application.
func fallthroughHandler(hit *bool) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		*hit = true
		w.WriteHeader(http.StatusTeapot)
		w.Write([]byte("next"))
	})
}

func localFileRequest(method, path string) *http.Request {
	return httptest.NewRequest(method, localFilePrefix+url.PathEscape(path), nil)
}

func writeTempFile(t *testing.T, dir, name, content string) string {
	t.Helper()
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestLocalFileServesBytes(t *testing.T) {
	file := writeTempFile(t, t.TempDir(), "pic.png", "hello image bytes")
	hit := false
	h := assetMiddleware(true)(fallthroughHandler(&hit))

	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, localFileRequest(http.MethodGet, file))
	if rec.Code != http.StatusOK || rec.Body.String() != "hello image bytes" {
		t.Fatalf("status %d body %q", rec.Code, rec.Body.String())
	}
	if hit {
		t.Fatal("a local file request must not reach the application handler")
	}
	if cc := rec.Header().Get("Cache-Control"); cc != "no-cache" {
		t.Errorf("Cache-Control = %q, want no-cache", cc)
	}
}

func TestLocalFileRangeRequest(t *testing.T) {
	file := writeTempFile(t, t.TempDir(), "clip.mp4", "0123456789")
	h := assetMiddleware(true)(fallthroughHandler(new(bool)))

	req := localFileRequest(http.MethodGet, file)
	req.Header.Set("Range", "bytes=0-3")
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusPartialContent {
		t.Fatalf("status = %d, want 206", rec.Code)
	}
	if rec.Body.String() != "0123" {
		t.Fatalf("range body = %q, want 0123", rec.Body.String())
	}
	if cr := rec.Header().Get("Content-Range"); cr != "bytes 0-3/10" {
		t.Fatalf("Content-Range = %q", cr)
	}
}

func TestLocalFileHeadHasNoBody(t *testing.T) {
	file := writeTempFile(t, t.TempDir(), "a.txt", "abc")
	h := assetMiddleware(true)(fallthroughHandler(new(bool)))
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, localFileRequest(http.MethodHead, file))
	if rec.Code != http.StatusOK || rec.Body.Len() != 0 {
		t.Fatalf("HEAD: status %d, body %d bytes", rec.Code, rec.Body.Len())
	}
}

func TestLocalFileRefusals(t *testing.T) {
	root := t.TempDir()
	secret := writeTempFile(t, filepath.Join(root, ".ssh"), "id_ed25519", "private")
	aws := writeTempFile(t, filepath.Join(root, ".aws"), "credentials", "private")
	gcloud := writeTempFile(t, filepath.Join(root, ".config", "gcloud"), "creds.json", "private")
	plain := writeTempFile(t, filepath.Join(root, "docs"), "note.txt", "ok")
	dir := filepath.Join(root, "docs")

	cases := []struct {
		name   string
		method string
		path   string
		want   int
	}{
		{"credential directory .ssh", http.MethodGet, secret, http.StatusNotFound},
		{"credential directory .aws", http.MethodGet, aws, http.StatusNotFound},
		{"credential directory .config/gcloud", http.MethodGet, gcloud, http.StatusNotFound},
		{"relative path", http.MethodGet, "relative/file.txt", http.StatusNotFound},
		{"directory instead of file", http.MethodGet, dir, http.StatusNotFound},
		{"missing file", http.MethodGet, filepath.Join(root, "missing.png"), http.StatusNotFound},
		{"POST is not allowed", http.MethodPost, plain, http.StatusMethodNotAllowed},
		{"PUT is not allowed", http.MethodPut, plain, http.StatusMethodNotAllowed},
		{"plain file still served", http.MethodGet, plain, http.StatusOK},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			h := assetMiddleware(true)(fallthroughHandler(new(bool)))
			rec := httptest.NewRecorder()
			h.ServeHTTP(rec, localFileRequest(tc.method, tc.path))
			if rec.Code != tc.want {
				t.Fatalf("status = %d, want %d", rec.Code, tc.want)
			}
		})
	}
}

func TestDeniedPathMatchesWholeSegmentsOnly(t *testing.T) {
	sep := string(filepath.Separator)
	cases := map[string]bool{
		filepath.Join(sep, "home", "u", ".ssh", "config"):         true,
		filepath.Join(sep, "home", "u", ".SSH", "config"):         true,
		filepath.Join(sep, "home", "u", ".config", "gcloud", "x"): true,
		filepath.Join(sep, "home", "u", "notes", "ssh-keys.txt"):  false,
		filepath.Join(sep, "home", "u", "my.ssh.backup", "x.png"): false,
		filepath.Join(sep, "home", "u", ".config", "other", "x"):  false,
	}
	for path, want := range cases {
		if got := denied(path); got != want {
			t.Errorf("denied(%q) = %v, want %v", path, got, want)
		}
	}
}

func TestNonFileRequestsReachNextAndGetCSPOnlyWhenEnforced(t *testing.T) {
	t.Run("enforced adds CSP and calls next", func(t *testing.T) {
		hit := false
		h := assetMiddleware(true)(fallthroughHandler(&hit))
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/index.html", nil))
		if !hit {
			t.Fatal("non-file request must reach the application handler")
		}
		csp := rec.Header().Get("Content-Security-Policy")
		if csp != contentSecurityPolicy {
			t.Fatalf("CSP = %q, want the production policy", csp)
		}
		if !strings.Contains(csp, "script-src 'self'") || strings.Contains(csp, "script-src 'self' 'unsafe-inline'") {
			t.Fatalf("script-src must stay 'self' without inline scripts: %q", csp)
		}
	})

	t.Run("not enforced sends no CSP", func(t *testing.T) {
		hit := false
		h := assetMiddleware(false)(fallthroughHandler(&hit))
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/index.html", nil))
		if !hit {
			t.Fatal("non-file request must reach the application handler")
		}
		if got := rec.Header().Get("Content-Security-Policy"); got != "" {
			t.Fatalf("CSP header present in dev mode: %q", got)
		}
	})
}
