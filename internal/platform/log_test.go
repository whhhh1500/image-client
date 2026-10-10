package platform

import (
	"bufio"
	"crypto/md5"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"
	"unicode/utf8"
)

func newTestSink(t *testing.T) (*LogSink, string) {
	t.Helper()
	dir := filepath.Join(t.TempDir(), "logs")
	sink, err := OpenLogSink(dir)
	if err != nil {
		t.Fatalf("OpenLogSink: %v", err)
	}
	t.Cleanup(func() { sink.Close() })
	return sink, dir
}

// readLogLines returns every JSON record in dir, across all log files in name order.
func readLogLines(t *testing.T, dir string) []map[string]any {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".log") {
			names = append(names, e.Name())
		}
	}
	sort.Strings(names)
	var out []map[string]any
	for _, name := range names {
		f, err := os.Open(filepath.Join(dir, name))
		if err != nil {
			t.Fatal(err)
		}
		sc := bufio.NewScanner(f)
		sc.Buffer(make([]byte, 0, 64*1024), 1<<20)
		for sc.Scan() {
			var rec map[string]any
			if err := json.Unmarshal(sc.Bytes(), &rec); err != nil {
				t.Fatalf("%s: invalid JSON line: %v", name, err)
			}
			out = append(out, rec)
		}
		f.Close()
		if err := sc.Err(); err != nil {
			t.Fatal(err)
		}
	}
	return out
}

func todayLog(dir string) string {
	return filepath.Join(dir, logPrefix+time.Now().Format(time.DateOnly)+".log")
}

func TestRecordShapeAndSequence(t *testing.T) {
	sink, dir := newTestSink(t)
	sink.Record(slog.LevelInfo, "backend", "image.start", map[string]any{"model": "m1"})
	sink.Record(slog.LevelWarn, "frontend", "ui.warn", nil)

	recs := readLogLines(t, dir)
	if len(recs) != 2 {
		t.Fatalf("got %d records, want 2", len(recs))
	}
	for _, key := range []string{"timestamp", "sequence", "level", "target", "event", "pid", "fields"} {
		if _, ok := recs[0][key]; !ok {
			t.Errorf("record missing key %q: %v", key, recs[0])
		}
	}
	if recs[0]["level"] != "INFO" || recs[0]["target"] != "backend" || recs[0]["event"] != "image.start" {
		t.Errorf("unexpected first record: %v", recs[0])
	}
	if recs[1]["level"] != "WARN" || recs[1]["target"] != "frontend" {
		t.Errorf("unexpected second record: %v", recs[1])
	}
	if recs[0]["pid"] != float64(os.Getpid()) {
		t.Errorf("pid = %v, want %d", recs[0]["pid"], os.Getpid())
	}
	if recs[0]["sequence"].(float64) >= recs[1]["sequence"].(float64) {
		t.Errorf("sequence not increasing: %v then %v", recs[0]["sequence"], recs[1]["sequence"])
	}
	if fields := recs[0]["fields"].(map[string]any); fields["model"] != "m1" {
		t.Errorf("fields = %v, want model m1", fields)
	}
}

func TestRecordEmptyEventBecomesUnnamed(t *testing.T) {
	sink, dir := newTestSink(t)
	sink.Record(slog.LevelInfo, "backend", "   ", nil)
	sink.Record(slog.LevelInfo, "backend", "", nil)
	recs := readLogLines(t, dir)
	if len(recs) != 2 {
		t.Fatalf("got %d records, want 2", len(recs))
	}
	for _, rec := range recs {
		if rec["event"] != "unnamed" {
			t.Errorf("event = %v, want unnamed", rec["event"])
		}
	}
}

func TestRecordRedactsSensitiveFields(t *testing.T) {
	sink, dir := newTestSink(t)
	sink.Record(slog.LevelInfo, "backend", "redact", map[string]any{
		"prompt":        "draw a cat",
		"apiKey":        "sk-live",
		"Authorization": "Bearer abc",
		"bindValues":    []any{1, "x"},
		"clientSecret":  map[string]any{"a": 1, "b": 2},
		"token":         42,
		"content":       "hello",
		"name":          "kept",
	})
	fields := readLogLines(t, dir)[0]["fields"].(map[string]any)

	want := map[string]any{
		"prompt":        map[string]any{"redacted": true, "chars": float64(10)},
		"apiKey":        map[string]any{"redacted": true, "chars": float64(7)},
		"Authorization": map[string]any{"redacted": true, "chars": float64(10)},
		"bindValues":    map[string]any{"redacted": true, "items": float64(2)},
		"clientSecret":  map[string]any{"redacted": true, "keys": float64(2)},
		"token":         "<redacted>",
		"content":       map[string]any{"redacted": true, "chars": float64(5)},
		"name":          "kept",
	}
	for key, value := range want {
		got, _ := json.Marshal(fields[key])
		exp, _ := json.Marshal(value)
		if string(got) != string(exp) {
			t.Errorf("fields[%q] = %s, want %s", key, got, exp)
		}
	}
}

func TestRecordTruncatesLongStringsByRunes(t *testing.T) {
	sink, dir := newTestSink(t)
	sink.Record(slog.LevelInfo, "backend", "long", map[string]any{"note": strings.Repeat("中", 2100)})
	got := readLogLines(t, dir)[0]["fields"].(map[string]any)["note"].(string)
	if !strings.HasSuffix(got, "…") {
		t.Fatalf("truncated string must end with an ellipsis, got %q", got[len(got)-min(len(got), 12):])
	}
	if n := utf8.RuneCountInString(got); n != 2049 {
		t.Fatalf("rune count = %d, want 2048 runes plus ellipsis (2049)", n)
	}
}

func TestRecordOversizedEventIsReplacedByMarker(t *testing.T) {
	sink, dir := newTestSink(t)
	fields := map[string]any{}
	for i := 0; i < 60; i++ {
		fields[fmt.Sprintf("k%02d", i)] = strings.Repeat("x", 2000)
	}
	sink.Record(slog.LevelInfo, "backend", "huge", fields)

	rec := readLogLines(t, dir)[0]
	got := rec["fields"].(map[string]any)
	if got["truncated"] != true {
		t.Fatalf("fields = %v, want truncation marker", got)
	}
	if n := got["originalEventBytes"].(float64); n <= maxLogEventBytes {
		t.Fatalf("originalEventBytes = %v, want > %d", n, maxLogEventBytes)
	}
	if rec["event"] != "huge" {
		t.Fatalf("event must survive truncation, got %v", rec["event"])
	}
}

func TestRotationWhenSizeLimitReached(t *testing.T) {
	sink, dir := newTestSink(t)
	sink.maxBytes = 300
	for i := 0; i < 10; i++ {
		sink.Record(slog.LevelInfo, "backend", "rotate", map[string]any{"i": i, "pad": strings.Repeat("p", 60)})
	}
	if _, err := os.Stat(todayLog(dir)); err != nil {
		t.Fatalf("base file missing: %v", err)
	}
	part := filepath.Join(dir, logPrefix+time.Now().Format(time.DateOnly)+".1.log")
	if _, err := os.Stat(part); err != nil {
		t.Fatalf("rotated part .1.log missing: %v", err)
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		info, err := e.Info()
		if err != nil {
			t.Fatal(err)
		}
		if info.Size() >= sink.maxBytes {
			t.Errorf("%s is %d bytes, limit %d", e.Name(), info.Size(), sink.maxBytes)
		}
	}
	if n := len(readLogLines(t, dir)); n != 10 {
		t.Fatalf("records across parts = %d, want 10", n)
	}
}

func TestPruneRemovesOnlyExpiredLogFiles(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "logs")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	day := func(offset int) string {
		return time.Now().AddDate(0, 0, offset).Format(time.DateOnly)
	}
	files := map[string]bool{ // name -> should survive pruning
		logPrefix + day(-30) + ".log":  false,
		logPrefix + day(-10) + ".log":  false,
		logPrefix + "2001-02-03.1.log": false,
		logPrefix + day(-5) + ".log":   true,
		logPrefix + day(0) + ".log":    true,
		logPrefix + day(0) + ".1.log":  true,
		"other-app.log":                true,
		logPrefix + "notes.log":        true,
	}
	for name := range files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte("{}\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	sink, err := OpenLogSink(dir)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sink.Close() })

	for name, survive := range files {
		_, err := os.Stat(filepath.Join(dir, name))
		if survive && err != nil {
			t.Errorf("%s was pruned but is inside the retention window", name)
		}
		if !survive && err == nil {
			t.Errorf("%s survived pruning but is outside the retention window", name)
		}
	}
}

func TestSlogHandlerRoutesTargetAndFields(t *testing.T) {
	sink, dir := newTestSink(t)
	logger := slog.New(sink.Handler())
	logger.Info("evt", "a", 1, "target", "frontend")
	logger.WithGroup("g").Info("grouped", "x", 2)
	logger.With("base", "b").Info("withAttrs", "y", 3)
	logger.Info("errAttr", "e", errors.New("boom"), "d", 1500*time.Millisecond)
	logger.Warn("lvl")

	recs := readLogLines(t, dir)
	if len(recs) != 5 {
		t.Fatalf("got %d records, want 5", len(recs))
	}
	if recs[0]["target"] != "frontend" {
		t.Errorf("target = %v, want frontend", recs[0]["target"])
	}
	if f := recs[0]["fields"].(map[string]any); len(f) != 1 || f["a"] != float64(1) {
		t.Errorf("fields = %v, want only {a:1}", f)
	}
	if recs[1]["target"] != "backend" {
		t.Errorf("default target = %v, want backend", recs[1]["target"])
	}
	g := recs[1]["fields"].(map[string]any)["g"].(map[string]any)
	if g["x"] != float64(2) {
		t.Errorf("group fields = %v, want g.x=2", g)
	}
	w := recs[2]["fields"].(map[string]any)
	if w["base"] != "b" || w["y"] != float64(3) {
		t.Errorf("WithAttrs fields = %v", w)
	}
	e := recs[3]["fields"].(map[string]any)
	if e["e"] != "boom" || e["d"] != float64(1500) {
		t.Errorf("error/duration fields = %v, want e=boom d=1500", e)
	}
	if recs[4]["level"] != "WARN" {
		t.Errorf("level = %v, want WARN", recs[4]["level"])
	}
}

func TestParseLevel(t *testing.T) {
	cases := map[string]slog.Level{
		"debug": slog.LevelDebug, "WARN": slog.LevelWarn, "error": slog.LevelError,
		"info": slog.LevelInfo, "nope": slog.LevelInfo, "": slog.LevelInfo,
	}
	for in, want := range cases {
		if got := ParseLevel(in); got != want {
			t.Errorf("ParseLevel(%q) = %v, want %v", in, got, want)
		}
	}
}

func TestErrorText(t *testing.T) {
	const hidden = "敏感错误详情已隐藏"
	cases := []struct {
		name string
		in   any
		want string
	}{
		{"assigned api_key hidden", "api_key=abc", hidden},
		{"json token hidden", `{"token": "x"}`, hidden},
		{"password hidden", "password: hunter2", hidden},
		{"Authorization failed kept", "Authorization failed", "Authorization failed"},
		{"bearer masked", "Bearer xyz timed out", "Bearer <redacted> timed out"},
		{"sk key masked", "failed with sk-abc123", "failed with sk-<redacted>"},
		{"plain message kept", errors.New("connection refused"), "connection refused"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := ErrorText(tc.in); got != tc.want {
				t.Fatalf("ErrorText(%v) = %q, want %q", tc.in, got, tc.want)
			}
		})
	}
	if n := utf8.RuneCountInString(ErrorText(strings.Repeat("a", 2500))); n != 2001 {
		t.Errorf("long message rune count = %d, want 2001 (2000 + ellipsis)", n)
	}
}

func TestSafeURL(t *testing.T) {
	cases := map[string]string{
		"https://api.example.com/v1/images?key=secret": "https://api.example.com/v1/images",
		"http://127.0.0.1:8123/path":                   "http://127.0.0.1:8123/path",
		"https://user:pass@host.example/x":             "https://host.example/x",
		"not a url":                                    "<invalid-url>",
		"http://":                                      "<invalid-url>",
		"":                                             "<invalid-url>",
	}
	for in, want := range cases {
		if got := SafeURL(in); got != want {
			t.Errorf("SafeURL(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestTextStats(t *testing.T) {
	text := "héllo"
	sum := md5.Sum([]byte(text))
	stats := TextStats(text)
	if stats["chars"] != 5 || stats["bytes"] != 6 {
		t.Errorf("chars/bytes = %v/%v, want 5/6", stats["chars"], stats["bytes"])
	}
	if stats["md5"] != hex.EncodeToString(sum[:]) {
		t.Errorf("md5 = %v", stats["md5"])
	}
	if strings.Contains(fmt.Sprint(stats), text) {
		t.Errorf("TextStats must not echo the text: %v", stats)
	}
}
