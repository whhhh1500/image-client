package app

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"unicode/utf16"

	"github.com/syndtr/goleveldb/leveldb"

	"imageclient/internal/ipc"
	"imageclient/internal/platform"
)

func latin1(s string) []byte { return append([]byte{1}, []byte(s)...) }

func utf16le(s string) []byte {
	out := []byte{0}
	for _, u := range utf16.Encode([]rune(s)) {
		out = append(out, byte(u), byte(u>>8))
	}
	return out
}

// writeProfile builds a WebView2 profile whose localStorage holds entries for
// the Tauri origin and for another origin.
func writeProfile(t *testing.T) string {
	t.Helper()
	profile := t.TempDir()
	dir := filepath.Join(profile, "EBWebView", "Default", "Local Storage", "leveldb")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	db, err := leveldb.OpenFile(dir, nil)
	if err != nil {
		t.Fatal(err)
	}
	put := func(key, value []byte) {
		if err := db.Put(key, value, nil); err != nil {
			t.Fatal(err)
		}
	}
	prefix := "_" + legacyOrigin + "\x00"
	put([]byte("VERSION"), []byte("1"))
	put([]byte("META:"+legacyOrigin), []byte{8, 1})
	put(append([]byte(prefix), latin1("video-md:last-workflow:p1")...), latin1(`{"id":"w1"}`))
	put(append([]byte(prefix), utf16le("草稿:小说")...), utf16le("第一章 正文"))
	put(append([]byte("_http://other.localhost\x00"), latin1("other")...), latin1("x"))
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
	return profile
}

func TestReadLocalStorageKeepsOnlyTheLegacyOrigin(t *testing.T) {
	profile := writeProfile(t)
	dir := filepath.Join(profile, "EBWebView", "Default", "Local Storage", "leveldb")
	entries, err := readLocalStorage(dir, legacyOrigin)
	if err != nil {
		t.Fatal(err)
	}
	want := map[string]string{"video-md:last-workflow:p1": `{"id":"w1"}`, "草稿:小说": "第一章 正文"}
	if len(entries) != len(want) {
		t.Fatalf("entries = %v, want %v", entries, want)
	}
	for k, v := range want {
		if entries[k] != v {
			t.Errorf("entries[%q] = %q, want %q", k, entries[k], v)
		}
	}
}

func TestLegacyLocalStorageIsHandedOverOnce(t *testing.T) {
	profile := writeProfile(t)
	a := &App{paths: platform.Paths{Root: t.TempDir()}}
	router := ipc.NewRouter()
	a.registerLegacyStorage(router)
	a.loadLegacyLocalStorage(profile)

	take := func() map[string]string {
		t.Helper()
		out, err := router.Invoke(context.Background(), "legacy_local_storage_take", nil)
		if err != nil {
			t.Fatal(err)
		}
		raw, _ := json.Marshal(out)
		var m map[string]string
		json.Unmarshal(raw, &m)
		return m
	}
	if got := take(); len(got) != 2 {
		t.Fatalf("first take = %v, want 2 entries", got)
	}
	if got := take(); len(got) != 0 {
		t.Fatalf("second take = %v, want none", got)
	}
	if _, err := os.Stat(filepath.Join(a.paths.Root, legacyMarker)); err != nil {
		t.Fatalf("marker not written: %v", err)
	}

	// The marker stops a later start from reading the old profile again.
	again := &App{paths: a.paths}
	again.loadLegacyLocalStorage(profile)
	if again.legacyStorage != nil {
		t.Fatalf("migration repeated: %v", again.legacyStorage)
	}
}

func TestMissingLegacyProfileMarksMigrationDone(t *testing.T) {
	a := &App{paths: platform.Paths{Root: t.TempDir()}}
	a.loadLegacyLocalStorage(t.TempDir())
	if _, err := os.Stat(filepath.Join(a.paths.Root, legacyMarker)); err != nil {
		t.Fatalf("marker not written for a profile without localStorage: %v", err)
	}
}

func TestDecodeChromiumString(t *testing.T) {
	cases := []struct {
		in   []byte
		want string
		ok   bool
	}{
		{latin1("abc"), "abc", true},
		{append([]byte{1}, 0xE9), "é", true},
		{utf16le("中文"), "中文", true},
		{[]byte{0, 0x41}, "", false}, // odd UTF-16 length
		{[]byte{7, 'a'}, "", false},  // unknown encoding
		{nil, "", false},
	}
	for _, c := range cases {
		got, ok := decodeChromiumString(c.in)
		if got != c.want || ok != c.ok {
			t.Errorf("decode(%v) = %q, %v; want %q, %v", c.in, got, ok, c.want, c.ok)
		}
	}
}
