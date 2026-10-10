package app

import (
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
)

// localFilePrefix is how the frontend displays files from disk:
// "/__file/" + encodeURIComponent(absolutePath). It replaces Tauri's asset
// protocol and serves range requests, so videos can seek.
const localFilePrefix = "/__file/"

// Credential stores stay unreadable from the page even though any other
// absolute path may be displayed (assets can live in a user-chosen folder).
var deniedDirs = []string{".ssh", ".aws", ".gnupg", filepath.Join(".config", "gcloud")}

// contentSecurityPolicy mirrors the policy the Tauri build enforced. The
// local REST API is reachable for the in-app docs; remote https images and
// media are allowed for the prompt library.
const contentSecurityPolicy = "default-src 'self'; " +
	"connect-src 'self' http://127.0.0.1:8123 https:; " +
	"img-src 'self' data: blob: https:; media-src 'self' data: blob: https:; " +
	"style-src 'self' 'unsafe-inline'; font-src 'self' data:; script-src 'self'"

// assetMiddleware serves local files and, in production, adds the CSP to HTML.
func assetMiddleware(enforceCSP bool) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if strings.HasPrefix(r.URL.Path, localFilePrefix) {
				serveLocalFile(w, r)
				return
			}
			if enforceCSP {
				w.Header().Set("Content-Security-Policy", contentSecurityPolicy)
			}
			next.ServeHTTP(w, r)
		})
	}
}

func serveLocalFile(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	path, err := url.PathUnescape(strings.TrimPrefix(r.URL.EscapedPath(), localFilePrefix))
	if err != nil || !filepath.IsAbs(path) || denied(path) {
		http.NotFound(w, r)
		return
	}
	// A link must not reach a denied directory either.
	if real, err := filepath.EvalSymlinks(path); err != nil || denied(real) {
		http.NotFound(w, r)
		return
	}
	file, err := os.Open(path)
	if err != nil {
		http.NotFound(w, r)
		return
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() {
		http.NotFound(w, r)
		return
	}
	w.Header().Set("Cache-Control", "no-cache")
	http.ServeContent(w, r, info.Name(), info.ModTime(), file)
}

func denied(path string) bool {
	clean := strings.ToLower(filepath.Clean(path))
	sep := string(filepath.Separator)
	for _, dir := range deniedDirs {
		if strings.Contains(clean+sep, sep+strings.ToLower(dir)+sep) {
			return true
		}
	}
	return false
}
