package store

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"math"
	"slices"
	"strconv"
	"strings"
	"unicode/utf8"

	"zombiezen.com/go/sqlite"
)

// The frontend reads and writes its own tables (assets, tasks, settings,
// workflows) directly. Everything else — secrets, comic and novel state — is
// reachable only through backend commands. A string filter rejects the obvious
// cases with a clear message; SQLite's authorizer is the authority, because it
// sees the real table behind every read and write, joins and aliases included.
var frontendTables = []string{"assets", "tasks", "settings", "workflows"}

const (
	maxQueryChars = 8192
	maxBindValues = 128
	maxBindBytes  = 32 << 20
	maxResultRows = 200_000
)

// ExecResult is what db_execute returns.
type ExecResult struct {
	RowsAffected int    `json:"rowsAffected"`
	LastInsertID *int64 `json:"lastInsertId"`
}

// FrontendExec runs one INSERT, UPDATE or DELETE from the frontend.
func (db *DB) FrontendExec(ctx context.Context, query string, binds []json.RawMessage) (ExecResult, error) {
	var result ExecResult
	err := db.frontendStatement(ctx, query, binds, false, func(conn *sqlite.Conn, stmt *sqlite.Stmt) error {
		if _, err := stmt.Step(); err != nil {
			return fmt.Errorf("SQLite 执行失败: %w", err)
		}
		id := conn.LastInsertRowID()
		result = ExecResult{RowsAffected: conn.Changes(), LastInsertID: &id}
		return nil
	})
	return result, err
}

// FrontendSelect runs one SELECT and returns rows keyed by column name.
// Blobs come back as base64, non-finite reals as strings.
func (db *DB) FrontendSelect(ctx context.Context, query string, binds []json.RawMessage) ([]map[string]any, error) {
	rows := []map[string]any{}
	err := db.frontendStatement(ctx, query, binds, true, func(_ *sqlite.Conn, stmt *sqlite.Stmt) error {
		columns := stmt.ColumnCount()
		for {
			more, err := stmt.Step()
			if err != nil {
				return fmt.Errorf("SQLite 查询失败: %w", err)
			}
			if !more {
				return nil
			}
			if len(rows) >= maxResultRows {
				return fmt.Errorf("SQLite 查询结果超过 %d 行限制", maxResultRows)
			}
			row := make(map[string]any, columns)
			for i := range columns {
				row[stmt.ColumnName(i)] = columnValue(stmt, i)
			}
			rows = append(rows, row)
		}
	})
	return rows, err
}

func (db *DB) frontendStatement(ctx context.Context, query string, binds []json.RawMessage, isSelect bool,
	run func(*sqlite.Conn, *sqlite.Stmt) error) error {
	if err := validateBinds(binds); err != nil {
		return err
	}
	if err := validateQuery(query, isSelect); err != nil {
		return err
	}
	return db.With(ctx, func(conn *sqlite.Conn) error {
		// The authorizer is consulted while the statement is prepared; a
		// transient statement is never cached, so it cannot be reused unchecked.
		if err := conn.SetAuthorizer(sqlite.AuthorizeFunc(authorizeFrontend)); err != nil {
			return fmt.Errorf("初始化 SQLite 授权器失败: %w", err)
		}
		stmt, _, err := conn.PrepareTransient(query)
		conn.SetAuthorizer(nil)
		if err != nil {
			label := "SQLite 执行失败"
			if isSelect {
				label = "SQLite 查询准备失败"
			}
			return fmt.Errorf("%s: %w", label, err)
		}
		defer stmt.Finalize()
		for i, raw := range binds {
			if err := bind(stmt, i+1, raw); err != nil {
				return err
			}
		}
		return run(conn, stmt)
	})
}

func authorizeFrontend(action sqlite.Action) sqlite.AuthResult {
	switch action.Type() {
	case sqlite.OpRead, sqlite.OpUpdate, sqlite.OpInsert, sqlite.OpDelete:
		if slices.Contains(frontendTables, action.Table()) {
			return sqlite.AuthResultOK
		}
		return sqlite.AuthResultDeny
	case sqlite.OpSelect, sqlite.OpFunction, sqlite.OpRecursive, sqlite.OpTransaction, sqlite.OpSavepoint:
		return sqlite.AuthResultOK
	default:
		return sqlite.AuthResultDeny
	}
}

func validateQuery(query string, isSelect bool) error {
	if utf8.RuneCountInString(query) > maxQueryChars {
		return fmt.Errorf("SQLite 语句超过 %d 个字符", maxQueryChars)
	}
	normalized := strings.Join(strings.Fields(query), " ")
	if normalized == "" || strings.ContainsAny(normalized, ";") || strings.Contains(normalized, "--") || strings.Contains(normalized, "/*") {
		return fmt.Errorf("SQLite 语句为空或包含不允许的多语句/注释")
	}
	lower := strings.ToLower(normalized)
	padded := " " + lower + " "
	for _, keyword := range []string{"join", "union", "pragma", "attach", "detach", "vacuum", "reindex"} {
		if strings.Contains(padded, " "+keyword+" ") {
			return fmt.Errorf("SQLite 语句包含不允许的关键字: %s", keyword)
		}
	}
	var allowed bool
	if isSelect {
		allowed = strings.HasPrefix(lower, "select ")
	} else {
		allowed = strings.HasPrefix(lower, "insert ") || strings.HasPrefix(lower, "update ") || strings.HasPrefix(lower, "delete ")
	}
	if !allowed {
		return fmt.Errorf("SQLite 操作类型不允许")
	}
	if (isSelect && strings.Contains(lower[len("select "):], "select ")) || (!isSelect && strings.Contains(lower, "select ")) {
		return fmt.Errorf("SQLite 不允许子查询")
	}
	tokens := strings.Fields(lower)
	tokenAfter := func(word string) string {
		if i := slices.Index(tokens, word); i >= 0 && i+1 < len(tokens) {
			return tokens[i+1]
		}
		return ""
	}
	var table string
	switch {
	case isSelect || strings.HasPrefix(lower, "delete "):
		table = tokenAfter("from")
	case strings.HasPrefix(lower, "update "):
		table = tokens[1]
	default:
		table = tokenAfter("into")
	}
	// "settings(key," names the table "settings": cut at the first character
	// that cannot be part of an identifier.
	table = strings.TrimLeftFunc(table, func(r rune) bool { return !isIdentRune(r) })
	if end := strings.IndexFunc(table, func(r rune) bool { return !isIdentRune(r) }); end >= 0 {
		table = table[:end]
	}
	if table == "" {
		return fmt.Errorf("无法识别 SQLite 表名")
	}
	if !slices.Contains(frontendTables, table) {
		return fmt.Errorf("SQLite 表不允许访问: %s", table)
	}
	return nil
}

func isIdentRune(r rune) bool { return r == '_' || r >= 'a' && r <= 'z' || r >= '0' && r <= '9' }

func validateBinds(binds []json.RawMessage) error {
	if len(binds) > maxBindValues {
		return fmt.Errorf("SQLite 参数数量超过 %d", maxBindValues)
	}
	for _, raw := range binds {
		if len(raw) > maxBindBytes {
			return fmt.Errorf("SQLite 单个参数超过 32 MiB 大小限制")
		}
	}
	return nil
}

// bind maps a JSON value: null, booleans as 0/1, integers, reals, strings,
// and arrays/objects as their JSON text.
func bind(stmt *sqlite.Stmt, param int, raw json.RawMessage) error {
	raw = bytes.TrimSpace(raw)
	switch {
	case len(raw) == 0 || string(raw) == "null":
		stmt.BindNull(param)
	case string(raw) == "true":
		stmt.BindInt64(param, 1)
	case string(raw) == "false":
		stmt.BindInt64(param, 0)
	case raw[0] == '"':
		var text string
		if err := json.Unmarshal(raw, &text); err != nil {
			return fmt.Errorf("SQLite 参数无效: %w", err)
		}
		stmt.BindText(param, text)
	case raw[0] == '[' || raw[0] == '{':
		var compact bytes.Buffer
		if err := json.Compact(&compact, raw); err != nil {
			return fmt.Errorf("序列化 SQLite JSON 参数失败: %w", err)
		}
		stmt.BindText(param, compact.String())
	default:
		if n, err := strconv.ParseInt(string(raw), 10, 64); err == nil {
			stmt.BindInt64(param, n)
		} else if f, err := strconv.ParseFloat(string(raw), 64); err == nil {
			stmt.BindFloat(param, f)
		} else {
			return fmt.Errorf("SQLite 数字参数超出范围")
		}
	}
	return nil
}

func columnValue(stmt *sqlite.Stmt, i int) any {
	switch stmt.ColumnType(i) {
	case sqlite.TypeNull:
		return nil
	case sqlite.TypeInteger:
		return stmt.ColumnInt64(i)
	case sqlite.TypeFloat:
		f := stmt.ColumnFloat(i)
		if math.IsNaN(f) || math.IsInf(f, 0) {
			return strconv.FormatFloat(f, 'g', -1, 64) // JSON has no NaN/Infinity
		}
		return f
	case sqlite.TypeBlob:
		buf := make([]byte, stmt.ColumnLen(i))
		stmt.ColumnBytes(i, buf)
		return base64.StdEncoding.EncodeToString(buf)
	default:
		return stmt.ColumnText(i)
	}
}
