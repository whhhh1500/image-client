// Package novel serves the novel-work commands: the work list, work creation,
// the work snapshot, and chapter revisions.
//
// Every write runs in one IMMEDIATE transaction together with its idempotency
// receipt, so a replayed request returns the response it first produced and a
// failed request leaves nothing behind.
package novel

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/google/uuid"
	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/ipc"
	"imageclient/internal/store"
)

const (
	maxTitleBytes          = 512
	maxChapterContentBytes = 8 * 1024 * 1024
)

// errArchived is refused for every write to an archived work.
var errArchived = errors.New("小说已归档，恢复后才能修改")

// Deps are the collaborators the service needs.
type Deps struct {
	DB *store.DB
}

// Service implements the novel commands.
type Service struct {
	db *store.DB
}

// New builds the service.
func New(d Deps) *Service {
	return &Service{db: d.DB}
}

// Register installs the commands on r.
func (s *Service) Register(r *ipc.Router) {
	r.Handle("novel_work_list", ipc.Typed(func(ctx context.Context, in ipc.Input[ListWorksInput]) ([]Work, error) {
		return s.listWorks(ctx, in.Input)
	}))
	r.Handle("novel_work_create", ipc.Typed(func(ctx context.Context, in ipc.Input[CreateWorkInput]) (Work, error) {
		return s.createWork(ctx, in.Input)
	}))
	r.Handle("novel_work_get", ipc.Typed(func(ctx context.Context, in ipc.Input[GetWorkInput]) (Snapshot, error) {
		return s.getSnapshot(ctx, in.Input)
	}))
	r.Handle("novel_chapter_revision_create", ipc.Typed(func(ctx context.Context, in ipc.Input[ChapterRevisionInput]) (ChapterRevision, error) {
		return s.createChapterRevision(ctx, in.Input)
	}))
}

// requiredText trims value and rejects an empty result. Callers use the
// trimmed value for every lookup and store.
func requiredText(value, field string) (string, error) {
	v := strings.TrimSpace(value)
	if v == "" {
		return "", fmt.Errorf("%s 不能为空", field)
	}
	return v, nil
}

func titleText(value, field string) (string, error) {
	v, err := requiredText(value, field)
	if err != nil {
		return "", err
	}
	if len(v) > maxTitleBytes {
		return "", fmt.Errorf("%s 不能超过 %d 字节", field, maxTitleBytes)
	}
	return v, nil
}

// normalizeContent checks the body size on the raw input, then converts line
// endings to LF.
func normalizeContent(content string) (string, error) {
	if len(content) > maxChapterContentBytes {
		return "", fmt.Errorf("章节正文不能超过 %d 字节", maxChapterContentBytes)
	}
	normalized := strings.ReplaceAll(strings.ReplaceAll(content, "\r\n", "\n"), "\r", "\n")
	if strings.TrimSpace(normalized) == "" {
		return "", errors.New("章节正文不能为空")
	}
	return normalized, nil
}

func newID(prefix string) string {
	return prefix + "_" + uuid.NewString()
}

func nowMillis() int64 {
	return time.Now().UnixMilli()
}

// nullable turns an optional input into the value hashed for idempotency:
// nil for an absent field, the value otherwise.
func nullable[T any](p *T) any {
	if p == nil {
		return nil
	}
	return *p
}

// query runs sql and calls each for every row.
func query(conn *sqlite.Conn, sql string, args []any, each func(*sqlite.Stmt)) error {
	return sqlitex.Execute(conn, sql, &sqlitex.ExecOptions{
		Args:       args,
		ResultFunc: func(stmt *sqlite.Stmt) error { each(stmt); return nil },
	})
}

// scalarInt reads the single integer a query returns, 0 when it returns no row.
func scalarInt(conn *sqlite.Conn, sql string, args ...any) (int64, error) {
	var n int64
	err := query(conn, sql, args, func(stmt *sqlite.Stmt) { n = stmt.ColumnInt64(0) })
	return n, err
}

func scalarBool(conn *sqlite.Conn, sql string, args ...any) (bool, error) {
	n, err := scalarInt(conn, sql, args...)
	return n != 0, err
}

// optionalText reads a nullable text column.
func optionalText(stmt *sqlite.Stmt, col int) *string {
	if stmt.ColumnType(col) == sqlite.TypeNull {
		return nil
	}
	s := stmt.ColumnText(col)
	return &s
}

// withReceipt runs action at most once per idempotency key. A replay with the
// same key and payload returns the stored response; the same key with another
// payload is refused. conn must already be inside a write transaction.
func withReceipt[T any](conn *sqlite.Conn, command, rawKey string, request map[string]any, action func() (T, error)) (T, error) {
	var zero T
	key, err := requiredText(rawKey, "idempotencyKey")
	if err != nil {
		return zero, err
	}
	hash := requestHash(request)

	var storedHash, storedJSON string
	found := false
	err = query(conn,
		`SELECT request_hash, response_json FROM novel_operation_receipts WHERE command_name = ? AND idempotency_key = ?`,
		[]any{command, key},
		func(stmt *sqlite.Stmt) {
			found = true
			storedHash = stmt.ColumnText(0)
			storedJSON = stmt.ColumnText(1)
		})
	if err != nil {
		return zero, fmt.Errorf("读取小说操作回执失败：%w", err)
	}
	if found {
		if storedHash != hash {
			return zero, errors.New("idempotencyKey 已用于不同业务载荷")
		}
		var replay T
		if err := json.Unmarshal([]byte(storedJSON), &replay); err != nil {
			return zero, fmt.Errorf("读取小说操作回执响应失败：%w", err)
		}
		return replay, nil
	}

	response, err := action()
	if err != nil {
		return zero, err
	}
	body, err := json.Marshal(response)
	if err != nil {
		return zero, fmt.Errorf("序列化小说操作回执失败：%w", err)
	}
	err = sqlitex.Execute(conn,
		`INSERT INTO novel_operation_receipts (id, command_name, idempotency_key, request_hash, response_json, created_at)
		 VALUES (?, ?, ?, ?, ?, ?)`,
		&sqlitex.ExecOptions{Args: []any{newID("nreceipt"), command, key, hash, string(body), nowMillis()}})
	if err != nil {
		return zero, fmt.Errorf("保存小说操作回执失败：%w", err)
	}
	return response, nil
}

// requestHash fingerprints a request exactly as the Rust backend did: the
// compact serde_json encoding of the input object, keys sorted. Receipts
// written before the port stay replayable.
func requestHash(request map[string]any) string {
	var b bytes.Buffer
	writeCanonical(&b, request)
	sum := sha256.Sum256(b.Bytes())
	return "sha256:" + hex.EncodeToString(sum[:])
}

// writeCanonical encodes the value kinds requests contain. Object keys are
// sorted and strings are escaped as serde_json escapes them, which is not the
// same as encoding/json (it escapes <, > and U+2028).
func writeCanonical(b *bytes.Buffer, v any) {
	switch v := v.(type) {
	case nil:
		b.WriteString("null")
	case bool:
		b.WriteString(strconv.FormatBool(v))
	case int64:
		b.WriteString(strconv.FormatInt(v, 10))
	case string:
		writeJSONString(b, v)
	case map[string]any:
		b.WriteByte('{')
		for i, k := range slices.Sorted(maps.Keys(v)) {
			if i > 0 {
				b.WriteByte(',')
			}
			writeJSONString(b, k)
			b.WriteByte(':')
			writeCanonical(b, v[k])
		}
		b.WriteByte('}')
	default:
		panic(fmt.Sprintf("novel: unsupported hash value %T", v))
	}
}

const hexDigits = "0123456789abcdef"

func writeJSONString(b *bytes.Buffer, s string) {
	b.WriteByte('"')
	for _, r := range s {
		switch r {
		case '"':
			b.WriteString(`\"`)
		case '\\':
			b.WriteString(`\\`)
		case '\b':
			b.WriteString(`\b`)
		case '\f':
			b.WriteString(`\f`)
		case '\n':
			b.WriteString(`\n`)
		case '\r':
			b.WriteString(`\r`)
		case '\t':
			b.WriteString(`\t`)
		default:
			if r < 0x20 {
				b.WriteString(`\u00`)
				b.WriteByte(hexDigits[r>>4])
				b.WriteByte(hexDigits[r&0xF])
			} else {
				b.WriteRune(r)
			}
		}
	}
	b.WriteByte('"')
}
