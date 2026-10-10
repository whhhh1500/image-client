package store

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"
)

func binds(values ...string) []json.RawMessage {
	out := make([]json.RawMessage, len(values))
	for i, v := range values {
		out[i] = json.RawMessage(v)
	}
	return out
}

const upsertSetting = "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value"

func TestFrontendExecSelectRoundTrip(t *testing.T) {
	db, _ := openTestDB(t)
	ctx := context.Background()

	res, err := db.FrontendExec(ctx, upsertSetting, binds(`"theme"`, `"dark"`))
	if err != nil {
		t.Fatalf("first upsert: %v", err)
	}
	if res.RowsAffected != 1 || res.LastInsertID == nil {
		t.Fatalf("exec result = %+v, want 1 row and a last insert id", res)
	}
	if _, err := db.FrontendExec(ctx, upsertSetting, binds(`"theme"`, `"light"`)); err != nil {
		t.Fatalf("second upsert: %v", err)
	}

	rows, err := db.FrontendSelect(ctx, "SELECT value FROM settings WHERE key = ?", binds(`"theme"`))
	if err != nil {
		t.Fatalf("select: %v", err)
	}
	if len(rows) != 1 || rows[0]["value"] != "light" {
		t.Fatalf("rows = %v, want one row with value light", rows)
	}

	empty, err := db.FrontendSelect(ctx, "SELECT value FROM settings WHERE key = ?", binds(`"missing"`))
	if err != nil || empty == nil || len(empty) != 0 {
		t.Fatalf("no-match select = %v (nil=%v), err %v; want empty non-nil slice", empty, empty == nil, err)
	}
}

func TestFrontendInsertSetsLastInsertID(t *testing.T) {
	db, _ := openTestDB(t)
	ins, err := db.FrontendExec(context.Background(),
		"INSERT INTO assets (id, kind, path, created_at) VALUES (?, ?, ?, ?)",
		binds(`"a-1"`, `"image"`, `"/tmp/a.png"`, `1700000000000`))
	if err != nil {
		t.Fatalf("insert: %v", err)
	}
	if ins.RowsAffected != 1 || ins.LastInsertID == nil || *ins.LastInsertID <= 0 {
		t.Fatalf("insert result = %+v, want 1 row and a positive LastInsertID", ins)
	}
}

// Deleting assets with SQL needs reads of the tables that reference them for
// the foreign-key check; those tables are not the frontend's, so the
// authorizer refuses. The frontend never deletes assets with SQL, and the row
// must survive the refused statement.
func TestFrontendDeleteFromAssetsIsRefused(t *testing.T) {
	db, _ := openTestDB(t)
	ctx := context.Background()
	if _, err := db.FrontendExec(ctx,
		"INSERT INTO assets (id, kind, path, created_at) VALUES (?, ?, ?, ?)",
		binds(`"a-1"`, `"image"`, `"/tmp/a.png"`, `1700000000000`)); err != nil {
		t.Fatal(err)
	}
	if _, err := db.FrontendExec(ctx, "DELETE FROM assets WHERE id = ?", binds(`"a-1"`)); err == nil {
		t.Fatal("delete from assets succeeded; want the authorizer to refuse it")
	}
	rows, err := db.FrontendSelect(ctx, "SELECT id FROM assets WHERE id = ?", binds(`"a-1"`))
	if err != nil || len(rows) != 1 {
		t.Fatalf("rows = %v, %v; want the asset kept", rows, err)
	}
}

func TestFrontendBindTypes(t *testing.T) {
	db, _ := openTestDB(t)
	ctx := context.Background()
	if _, err := db.FrontendExec(ctx, upsertSetting, binds(`"seed"`, `"x"`)); err != nil {
		t.Fatal(err)
	}

	cases := []struct {
		name string
		raw  string
		want any
	}{
		{"null", `null`, nil},
		{"true is 1", `true`, int64(1)},
		{"false is 0", `false`, int64(0)},
		{"integer", `42`, int64(42)},
		{"large integer keeps precision", `9007199254740993`, int64(9007199254740993)},
		{"real", `1.5`, 1.5},
		{"string", `"text"`, "text"},
		{"empty string", `""`, ""},
		{"array as compact JSON text", `[1, "a"]`, `[1,"a"]`},
		{"object as compact JSON text", `{ "k": [1] }`, `{"k":[1]}`},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			rows, err := db.FrontendSelect(ctx, "SELECT ? AS v FROM settings WHERE key = 'seed'", binds(tc.raw))
			if err != nil {
				t.Fatalf("select: %v", err)
			}
			if len(rows) != 1 {
				t.Fatalf("rows = %d, want 1", len(rows))
			}
			if rows[0]["v"] != tc.want {
				t.Fatalf("bound %s came back as %#v (%T), want %#v", tc.raw, rows[0]["v"], rows[0]["v"], tc.want)
			}
		})
	}
}

func TestFrontendSelectReturnsBlobsAsBase64(t *testing.T) {
	db, _ := openTestDB(t)
	ctx := context.Background()
	err := db.With(ctx, func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, "INSERT INTO settings (key, value) VALUES ('blob', x'0001FF')", nil)
	})
	if err != nil {
		t.Fatal(err)
	}
	rows, err := db.FrontendSelect(ctx, "SELECT value FROM settings WHERE key = 'blob'", nil)
	if err != nil || len(rows) != 1 {
		t.Fatalf("select blob: rows %v err %v", rows, err)
	}
	if rows[0]["value"] != "AAH/" {
		t.Fatalf("blob value = %v, want base64 AAH/", rows[0]["value"])
	}
}

func TestFrontendRejectsUnsafeStatements(t *testing.T) {
	db, _ := openTestDB(t)
	ctx := context.Background()
	if _, err := db.FrontendExec(ctx, upsertSetting, binds(`"seed"`, `"x"`)); err != nil {
		t.Fatal(err)
	}

	type call struct {
		name   string
		query  string
		binds  []json.RawMessage
		exec   bool
		errSub string // substring the error must contain; empty = any error
	}
	big := "SELECT value FROM settings WHERE key = '" + strings.Repeat("a", 8200) + "'"
	many := make([]json.RawMessage, 129)
	for i := range many {
		many[i] = json.RawMessage(`null`)
	}
	calls := []call{
		{name: "multi-statement", query: "SELECT value FROM settings; DELETE FROM settings", errSub: "多语句/注释"},
		{name: "line comment", query: "SELECT value FROM settings -- x", errSub: "多语句/注释"},
		{name: "block comment", query: "SELECT value FROM settings /* x */", errSub: "多语句/注释"},
		{name: "join", query: "SELECT settings.value FROM settings JOIN assets ON 1 = 1", errSub: "join"},
		{name: "union", query: "SELECT value FROM settings UNION SELECT value FROM settings", errSub: "union"},
		{name: "pragma", query: "PRAGMA user_version", errSub: "pragma"},
		{name: "subquery", query: "SELECT value FROM settings WHERE key IN (SELECT key FROM settings)", errSub: "子查询"},
		{name: "drop table via exec", query: "DROP TABLE settings", exec: true, errSub: "操作类型不允许"},
		{name: "select from app_secrets", query: "SELECT value FROM app_secrets", errSub: "表不允许访问: app_secrets"},
		{name: "comma join app_secrets is denied by the authorizer", query: "SELECT value FROM settings, app_secrets"},
		{name: "comma join assets then app_secrets", query: "SELECT value FROM assets, app_secrets"},
		{name: "insert via select channel", query: "INSERT INTO settings (key, value) VALUES ('x', 'y')", errSub: "操作类型不允许"},
		{name: "insert into app_secrets", query: "INSERT INTO app_secrets (key, value) VALUES ('a', 'b')", exec: true, errSub: "表不允许访问"},
		{name: "update app_secrets", query: "UPDATE app_secrets SET value = 'x'", exec: true, errSub: "表不允许访问"},
		{name: "delete from app_secrets", query: "DELETE FROM app_secrets", exec: true, errSub: "表不允许访问"},
		{name: "attach keyword", query: "SELECT value FROM settings WHERE key = 'a' attach", errSub: "attach"},
		{name: "empty", query: "   ", errSub: "为空"},
		{name: "too many binds", query: "SELECT value FROM settings WHERE key = ?", binds: many, errSub: "参数数量超过 128"},
		{name: "query over 8192 chars", query: big, errSub: "超过 8192 个字符"},
		{name: "number outside float range", query: "SELECT value FROM settings WHERE key = ?", binds: binds(`1e999`), errSub: "数字参数超出范围"},
	}
	for _, c := range calls {
		t.Run(c.name, func(t *testing.T) {
			var err error
			if c.exec {
				_, err = db.FrontendExec(ctx, c.query, c.binds)
			} else {
				_, err = db.FrontendSelect(ctx, c.query, c.binds)
			}
			if err == nil {
				t.Fatalf("%q was accepted, want rejection", c.query)
			}
			if c.errSub != "" && !strings.Contains(err.Error(), c.errSub) {
				t.Fatalf("err = %v, want containing %q", err, c.errSub)
			}
		})
	}

	// The rejected statements must not have changed anything.
	rows, err := db.FrontendSelect(ctx, "SELECT value FROM settings WHERE key = 'seed'", nil)
	if err != nil || len(rows) != 1 {
		t.Fatalf("settings row lost after rejected statements: %v %v", rows, err)
	}
}

func TestFrontendAllowsOnlyWhitelistedTables(t *testing.T) {
	db, _ := openTestDB(t)
	ctx := context.Background()
	for _, table := range []string{"assets", "tasks", "settings", "workflows"} {
		if _, err := db.FrontendSelect(ctx, "SELECT * FROM "+table, nil); err != nil {
			t.Errorf("select from whitelisted %s: %v", table, err)
		}
	}
}

func TestFrontendInsertWithoutSpaceBeforeColumnList(t *testing.T) {
	db, _ := openTestDB(t)
	// Expected: valid SQL on an allowed table is accepted regardless of spacing.
	if _, err := db.FrontendExec(context.Background(), "INSERT INTO settings(key, value) VALUES (?, ?)", binds(`"k"`, `"v"`)); err != nil {
		t.Fatalf("valid INSERT rejected: %v", err)
	}
}
