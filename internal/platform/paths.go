// Package platform holds the process-wide plumbing every feature relies on:
// the data directory layout, structured logging, filesystem helpers and the
// shared HTTP clients.
package platform

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
)

// DataDirEnv overrides the data root with an absolute, non-root directory,
// which isolates test and harness runs from the user's real data.
const DataDirEnv = "IMAGE_CLIENT_DATA_DIR"

// Paths is the on-disk layout under the data root. The layout is shared with
// earlier releases, so existing user data keeps working.
type Paths struct {
	Root string
}

func (p Paths) Assets() string          { return filepath.Join(p.Root, "assets") }
func (p Paths) Logs() string            { return filepath.Join(p.Root, "logs") }
func (p Paths) PromptlibImages() string { return filepath.Join(p.Root, "promptlib-images") }
func (p Paths) Database() string        { return filepath.Join(p.Root, "image-client.db") }
func (p Paths) BackendConfig() string   { return filepath.Join(p.Root, "backend-config.json") }

// Slash is the data root with forward slashes, as the frontend expects.
func (p Paths) Slash() string { return filepath.ToSlash(p.Root) }

// Ensure creates the data root and its fixed subdirectories, private to the user.
func (p Paths) Ensure() error {
	for _, dir := range []string{p.Root, p.Assets(), p.Logs(), p.PromptlibImages()} {
		if err := MkdirPrivate(dir); err != nil {
			return err
		}
	}
	return nil
}

var (
	resolveOnce sync.Once
	resolved    Paths
	resolveErr  error
)

// ResolvePaths returns the data layout, validating DataDirEnv once per process.
// An invalid override is an error rather than a fallback: an isolated run must
// never touch the real user data.
func ResolvePaths() (Paths, error) {
	resolveOnce.Do(func() {
		root, err := dataRoot(os.Getenv(DataDirEnv), os.LookupEnv)
		resolved, resolveErr = Paths{Root: root}, err
	})
	return resolved, resolveErr
}

// OverrideActive reports whether the data root comes from DataDirEnv.
func OverrideActive() bool {
	_, ok := os.LookupEnv(DataDirEnv)
	return ok
}

func dataRoot(override string, lookup func(string) (string, bool)) (string, error) {
	if _, set := lookup(DataDirEnv); !set {
		return filepath.Join(homeDir(lookup), "ImageClient"), nil
	}
	return validateOverride(override)
}

// homeDir prefers the platform's native variable (USERPROFILE on Windows,
// HOME elsewhere) so isolated runs that redirect it stay isolated.
func homeDir(lookup func(string) (string, bool)) string {
	order := []string{"HOME", "USERPROFILE"}
	if runtime.GOOS == "windows" {
		order = []string{"USERPROFILE", "HOME"}
	}
	for _, name := range order {
		if value, ok := lookup(name); ok && value != "" {
			return value
		}
	}
	if home, err := os.UserHomeDir(); err == nil {
		return home
	}
	return os.TempDir()
}

func validateOverride(value string) (string, error) {
	invalid := func(reason string) (string, error) {
		return "", fmt.Errorf("%s %s", DataDirEnv, reason)
	}
	if strings.TrimSpace(value) == "" {
		return invalid("must not be empty")
	}
	if !filepath.IsAbs(value) {
		return invalid("must be an absolute directory path")
	}
	for _, segment := range strings.FieldsFunc(value, func(r rune) bool { return r == '/' || r == '\\' }) {
		if segment == ".." {
			return invalid("must not contain '..' path segments")
		}
	}
	if isRoot(value) {
		return invalid("must not be a filesystem root")
	}
	info, err := os.Stat(value)
	switch {
	case errors.Is(err, os.ErrNotExist):
		return value, nil
	case err != nil:
		return invalid(fmt.Sprintf("cannot be validated: %v", err))
	case !info.IsDir():
		return invalid("must name a directory, not a file")
	}
	if real, err := filepath.EvalSymlinks(value); err == nil && isRoot(real) {
		return invalid("must not resolve to a filesystem root")
	}
	return value, nil
}

func isRoot(path string) bool {
	clean := filepath.Clean(path)
	return filepath.Dir(clean) == clean
}
