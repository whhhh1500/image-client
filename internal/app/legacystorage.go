package app

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"unicode/utf16"

	"github.com/syndtr/goleveldb/leveldb"
	"github.com/syndtr/goleveldb/leveldb/opt"
	"github.com/syndtr/goleveldb/leveldb/util"

	"imageclient/internal/ipc"
)

// The Tauri build kept some UI state in WebView localStorage (unsaved drafts,
// last-opened workflows, presets). localStorage is per origin, and the page
// origin changed from http://tauri.localhost to http://wails.localhost, so the
// entries are copied once: read here from the shared WebView2 profile before
// the browser opens it, handed to the frontend, which writes the ones it does
// not already have.
const (
	legacyOrigin = "http://tauri.localhost"
	legacyMarker = ".legacy-local-storage-migrated"
)

// loadLegacyLocalStorage reads the old origin's entries unless the migration
// already ran. Failures only cost the old UI state, so they are logged.
func (a *App) loadLegacyLocalStorage(webviewDir string) {
	if webviewDir == "" {
		return
	}
	if _, err := os.Stat(filepath.Join(a.paths.Root, legacyMarker)); err == nil {
		return
	}
	dir := filepath.Join(webviewDir, "EBWebView", "Default", "Local Storage", "leveldb")
	entries, err := readLocalStorage(dir, legacyOrigin)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		a.markLegacyMigrated()
	case err != nil:
		slog.Warn("legacy_local_storage.read_failed", "error", err.Error())
	default:
		a.legacyStorage = entries
		slog.Info("legacy_local_storage.loaded", "entries", len(entries))
	}
}

func (a *App) markLegacyMigrated() {
	if err := os.WriteFile(filepath.Join(a.paths.Root, legacyMarker), nil, 0o600); err != nil {
		slog.Warn("legacy_local_storage.mark_failed", "error", err.Error())
	}
}

func (a *App) registerLegacyStorage(r *ipc.Router) {
	// Returns the old entries once; the marker is written when they are handed
	// over, so a crash before the frontend takes them retries next start.
	r.Handle("legacy_local_storage_take", ipc.Typed(func(context.Context, struct{}) (map[string]string, error) {
		a.mu.Lock()
		entries := a.legacyStorage
		a.legacyStorage = nil
		a.mu.Unlock()
		if entries == nil {
			return map[string]string{}, nil
		}
		a.markLegacyMigrated()
		return entries, nil
	}))
}

// readLocalStorage extracts one origin's entries from a Chromium localStorage
// LevelDB. It works on a copy so a profile in use is never touched.
//
// Keys are "_" + origin + "\x00" + encoded key; keys and values start with an
// encoding byte: 1 for Latin-1, 0 for UTF-16LE.
func readLocalStorage(dir, origin string) (map[string]string, error) {
	if _, err := os.Stat(dir); err != nil {
		return nil, err
	}
	copyDir, err := os.MkdirTemp("", "image-client-localstorage-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(copyDir)
	if err := copyFiles(dir, copyDir); err != nil {
		return nil, fmt.Errorf("复制旧本地存储失败: %w", err)
	}
	db, err := leveldb.OpenFile(copyDir, &opt.Options{ReadOnly: true})
	if err != nil {
		return nil, fmt.Errorf("打开旧本地存储失败: %w", err)
	}
	defer db.Close()

	prefix := []byte("_" + origin + "\x00")
	entries := map[string]string{}
	it := db.NewIterator(util.BytesPrefix(prefix), nil)
	defer it.Release()
	for it.Next() {
		key, okKey := decodeChromiumString(bytes.TrimPrefix(it.Key(), prefix))
		value, okValue := decodeChromiumString(it.Value())
		if okKey && okValue {
			entries[key] = value
		}
	}
	return entries, it.Error()
}

func copyFiles(src, dst string) error {
	files, err := os.ReadDir(src)
	if err != nil {
		return err
	}
	for _, f := range files {
		if !f.Type().IsRegular() || f.Name() == "LOCK" {
			continue
		}
		data, err := os.ReadFile(filepath.Join(src, f.Name()))
		if err != nil {
			return err
		}
		if err := os.WriteFile(filepath.Join(dst, f.Name()), data, 0o600); err != nil {
			return err
		}
	}
	return nil
}

func decodeChromiumString(b []byte) (string, bool) {
	if len(b) == 0 {
		return "", false
	}
	switch b[0] {
	case 1: // Latin-1: each byte is a code point
		runes := make([]rune, len(b)-1)
		for i, c := range b[1:] {
			runes[i] = rune(c)
		}
		return string(runes), true
	case 0: // UTF-16LE
		body := b[1:]
		if len(body)%2 != 0 {
			return "", false
		}
		units := make([]uint16, len(body)/2)
		for i := range units {
			units[i] = uint16(body[2*i]) | uint16(body[2*i+1])<<8
		}
		return string(utf16.Decode(units)), true
	}
	return "", false
}
