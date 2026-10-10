package platform

import (
	"context"
	"crypto/md5"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode/utf8"
)

// Log files are JSON lines named image-client-YYYY-MM-DD[.N].log. Each record
// keeps the established shape: timestamp, sequence, level, target, event,
// pid and a sanitized fields object.
const (
	logPrefix        = "image-client-"
	maxLogFileBytes  = 10 * 1024 * 1024
	maxLogEventBytes = 64 * 1024
	logRetentionDays = 10
)

// TargetKey names the attribute that routes a record to a target other than
// "backend" (the frontend batch uses "frontend").
const TargetKey = "target"

// LogSink writes rotated, size-capped JSONL files and is safe for concurrent use.
type LogSink struct {
	dir      string
	maxBytes int64
	keepDays int
	sequence atomic.Uint64

	mu   sync.Mutex
	day  string
	part int
	size int64
	file *os.File
}

// OpenLogSink prepares dir and removes files older than the retention window.
func OpenLogSink(dir string) (*LogSink, error) {
	if err := MkdirPrivate(dir); err != nil {
		return nil, fmt.Errorf("创建日志目录失败：%w", err)
	}
	sink := &LogSink{dir: dir, maxBytes: maxLogFileBytes, keepDays: logRetentionDays}
	sink.prune(time.Now())
	return sink, nil
}

// Close flushes and closes the current file.
func (s *LogSink) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.file == nil {
		return nil
	}
	err := s.file.Close()
	s.file = nil
	return err
}

func (s *LogSink) path(day string, part int) string {
	if part == 0 {
		return filepath.Join(s.dir, logPrefix+day+".log")
	}
	return filepath.Join(s.dir, fmt.Sprintf("%s%s.%d.log", logPrefix, day, part))
}

func (s *LogSink) prune(now time.Time) {
	oldest := now.AddDate(0, 0, -(s.keepDays - 1)).Format(time.DateOnly)
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return
	}
	for _, entry := range entries {
		name := entry.Name()
		if !strings.HasPrefix(name, logPrefix) || !strings.HasSuffix(name, ".log") || len(name) < len(logPrefix)+10 {
			continue
		}
		day := name[len(logPrefix) : len(logPrefix)+10]
		if _, err := time.Parse(time.DateOnly, day); err == nil && day < oldest {
			os.Remove(filepath.Join(s.dir, name))
		}
	}
}

// open picks the first part of day that still has room.
func (s *LogSink) open(day string) error {
	if s.file != nil {
		s.file.Close()
		s.file = nil
	}
	for part := 0; ; part++ {
		path := s.path(day, part)
		info, err := os.Stat(path)
		if err == nil && info.Size() >= s.maxBytes {
			continue
		}
		file, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
		if err != nil {
			return fmt.Errorf("打开日志文件失败 %s: %w", path, err)
		}
		s.day, s.part, s.file, s.size = day, part, file, 0
		if info != nil {
			s.size = info.Size()
		}
		return nil
	}
}

func (s *LogSink) write(now time.Time, line []byte) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	day := now.Format(time.DateOnly)
	if s.file == nil || s.day != day {
		s.prune(now)
		if err := s.open(day); err != nil {
			return err
		}
	}
	if s.size+int64(len(line)) >= s.maxBytes {
		s.file.Close()
		path := s.path(day, s.part+1)
		file, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
		if err != nil {
			s.file = nil
			return fmt.Errorf("创建日志分卷失败 %s: %w", path, err)
		}
		s.part, s.file, s.size = s.part+1, file, 0
	}
	n, err := s.file.Write(line)
	s.size += int64(n)
	return err
}

// Record writes one entry. Fields are sanitized: secrets and user content are
// replaced by their size, long strings and collections are truncated.
func (s *LogSink) Record(level slog.Level, target, event string, fields map[string]any) {
	now := time.Now()
	if strings.TrimSpace(event) == "" {
		event = "unnamed"
	}
	record := map[string]any{
		"timestamp": now.Format("2006-01-02T15:04:05.000Z07:00"),
		"sequence":  s.sequence.Add(1),
		"level":     levelName(level),
		"target":    truncateRunes(target, 64),
		"event":     truncateRunes(event, 256),
		"pid":       os.Getpid(),
		"fields":    sanitize(normalizeJSON(fields), ""),
	}
	line, err := json.Marshal(record)
	if err == nil && len(line) > maxLogEventBytes {
		record["fields"] = map[string]any{"truncated": true, "originalEventBytes": len(line)}
		line, err = json.Marshal(record)
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "[logging] 序列化日志失败：%v\n", err)
		return
	}
	if err := s.write(now, append(line, '\n')); err != nil {
		fmt.Fprintf(os.Stderr, "[logging] %v\n", err)
	}
}

func levelName(level slog.Level) string {
	switch {
	case level >= slog.LevelError:
		return "ERROR"
	case level >= slog.LevelWarn:
		return "WARN"
	case level >= slog.LevelInfo:
		return "INFO"
	default:
		return "DEBUG"
	}
}

// ParseLevel maps the frontend's level names; unknown names are INFO.
func ParseLevel(name string) slog.Level {
	switch strings.ToUpper(name) {
	case "DEBUG":
		return slog.LevelDebug
	case "WARN":
		return slog.LevelWarn
	case "ERROR":
		return slog.LevelError
	default:
		return slog.LevelInfo
	}
}

// Handler adapts the sink to slog: the message is the event name and the
// attributes become fields.
func (s *LogSink) Handler() slog.Handler { return &logHandler{sink: s, target: "backend"} }

type logHandler struct {
	sink   *LogSink
	target string
	attrs  []slog.Attr
	groups []string
}

func (h *logHandler) Enabled(context.Context, slog.Level) bool { return true }

func (h *logHandler) Handle(_ context.Context, r slog.Record) error {
	fields := make(map[string]any, len(h.attrs)+r.NumAttrs())
	target := h.target
	add := func(a slog.Attr) bool {
		if a.Key == TargetKey && len(h.groups) == 0 {
			target = a.Value.String()
			return true
		}
		insertAttr(fields, h.groups, a)
		return true
	}
	for _, a := range h.attrs {
		add(a)
	}
	r.Attrs(add)
	h.sink.Record(r.Level, target, r.Message, fields)
	return nil
}

func (h *logHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	next := *h
	next.attrs = append(append([]slog.Attr(nil), h.attrs...), attrs...)
	return &next
}

func (h *logHandler) WithGroup(name string) slog.Handler {
	next := *h
	next.groups = append(append([]string(nil), h.groups...), name)
	return &next
}

func insertAttr(fields map[string]any, groups []string, a slog.Attr) {
	for _, group := range groups {
		child, ok := fields[group].(map[string]any)
		if !ok {
			child = map[string]any{}
			fields[group] = child
		}
		fields = child
	}
	value := a.Value.Resolve()
	if value.Kind() == slog.KindGroup {
		group := map[string]any{}
		for _, inner := range value.Group() {
			insertAttr(group, nil, inner)
		}
		fields[a.Key] = group
		return
	}
	switch v := value.Any().(type) {
	case error:
		fields[a.Key] = v.Error()
	case time.Duration:
		fields[a.Key] = v.Milliseconds()
	default:
		fields[a.Key] = v
	}
}

// normalizeJSON turns arbitrary values (structs, typed slices) into plain JSON
// values so sanitize can walk them.
func normalizeJSON(value any) any {
	switch value.(type) {
	case nil, string, bool, float64, int, int64, uint64, map[string]any, []any:
		if m, ok := value.(map[string]any); ok {
			out := make(map[string]any, len(m))
			for k, v := range m {
				out[k] = normalizeJSON(v)
			}
			return out
		}
		if s, ok := value.([]any); ok {
			out := make([]any, len(s))
			for i, v := range s {
				out[i] = normalizeJSON(v)
			}
			return out
		}
		return value
	}
	raw, err := json.Marshal(value)
	if err != nil {
		return fmt.Sprint(value)
	}
	var plain any
	if json.Unmarshal(raw, &plain) != nil {
		return string(raw)
	}
	return plain
}

var (
	sensitiveSubstrings = []string{"authorization", "password", "secret", "apikey", "accesskey", "privatekey"}
	sensitiveExact      = map[string]bool{
		"token": true, "accesstoken": true, "refreshtoken": true, "authtoken": true, "bearertoken": true,
		// User content is never logged verbatim.
		"prompt": true, "system": true, "content": true, "text": true, "user": true, "data": true,
		"input": true, "result": true, "response": true, "body": true, "database64": true, "bindvalues": true,
	}
)

func sensitiveKey(key string) bool {
	k := strings.NewReplacer("-", "", "_", "").Replace(strings.ToLower(key))
	for _, needle := range sensitiveSubstrings {
		if strings.Contains(k, needle) {
			return true
		}
	}
	return sensitiveExact[k]
}

func sanitize(value any, key string) any {
	if key != "" && sensitiveKey(key) {
		switch v := value.(type) {
		case string:
			return map[string]any{"redacted": true, "chars": utf8.RuneCountInString(v)}
		case []any:
			return map[string]any{"redacted": true, "items": len(v)}
		case map[string]any:
			return map[string]any{"redacted": true, "keys": len(v)}
		default:
			return "<redacted>"
		}
	}
	switch v := value.(type) {
	case string:
		return truncateRunes(v, 2048)
	case []any:
		if len(v) > 100 {
			v = v[:100]
		}
		out := make([]any, len(v))
		for i, item := range v {
			out[i] = sanitize(item, "")
		}
		return out
	case map[string]any:
		out := make(map[string]any, min(len(v), 100))
		for k, item := range v {
			if len(out) == 100 {
				break
			}
			out[k] = sanitize(item, k)
		}
		return out
	default:
		return v
	}
}

func truncateRunes(value string, max int) string {
	if utf8.RuneCountInString(value) <= max {
		return value
	}
	runes := []rune(value)
	return string(runes[:max]) + "…"
}

// TextStats describes user text without logging it.
func TextStats(text string) map[string]any {
	sum := md5.Sum([]byte(text))
	return map[string]any{"chars": utf8.RuneCountInString(text), "bytes": len(text), "md5": hex.EncodeToString(sum[:])}
}

// SafeURL keeps scheme, host, port and path; query strings may carry keys.
func SafeURL(raw string) string {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme == "" || u.Host == "" {
		return "<invalid-url>"
	}
	return u.Scheme + "://" + u.Host + u.Path
}

var credentialFields = []string{
	"authorization", "api_key", "api-key", "apikey", "token", "access_token", "access-token", "accesstoken",
	"refresh_token", "refresh-token", "refreshtoken", "auth_token", "auth-token", "authtoken",
	"bearer_token", "bearer-token", "bearertoken", "access_key", "access-key", "accesskey",
	"client_secret", "client-secret", "clientsecret", "secret", "secret_key", "secret-key", "secretkey",
	"private_key", "private-key", "privatekey", "password",
}

// ErrorText prepares an error message for logs: an error that assigns a
// credential is hidden entirely, bearer tokens and sk- keys are masked.
func ErrorText(err any) string {
	text := truncateRunes(fmt.Sprint(err), 2000)
	if containsCredentialAssignment(text) {
		return "敏感错误详情已隐藏"
	}
	return redactTokens(text)
}

func containsCredentialAssignment(value string) bool {
	lower := strings.ToLower(value)
	for _, field := range credentialFields {
		for offset := 0; ; {
			i := strings.Index(lower[offset:], field)
			if i < 0 {
				break
			}
			start := offset + i
			offset = start + len(field)
			if start > 0 {
				prev := lower[start-1]
				if isWordByte(prev) || prev == '_' || prev == '-' {
					continue
				}
			}
			after := strings.TrimLeft(lower[offset:], " \t")
			after = strings.TrimLeft(strings.TrimPrefix(strings.TrimPrefix(after, `"`), "'"), " \t")
			if strings.HasPrefix(after, ":") || strings.HasPrefix(after, "=") {
				return true
			}
		}
	}
	return false
}

func isWordByte(b byte) bool {
	return b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z' || b >= '0' && b <= '9'
}

func redactTokens(value string) string {
	var out strings.Builder
	lower := strings.ToLower(value)
	for i := 0; i < len(value); {
		switch {
		case strings.HasPrefix(lower[i:], "bearer "):
			out.WriteString(value[i : i+7])
			out.WriteString("<redacted>")
			i += 7
			for i < len(value) && !strings.ContainsRune(" \t\r\n\"'", rune(value[i])) {
				i++
			}
		case strings.HasPrefix(lower[i:], "sk-"):
			out.WriteString(value[i : i+3])
			out.WriteString("<redacted>")
			i += 3
			for i < len(value) && (isWordByte(value[i]) || strings.ContainsRune("._-", rune(value[i]))) {
				i++
			}
		default:
			r, size := utf8.DecodeRuneInString(value[i:])
			out.WriteRune(r)
			i += size
		}
	}
	return out.String()
}
