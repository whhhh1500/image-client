package store

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"
)

// openTestDB opens a fresh database in a temp dir. The DB is closed before the
// temp dir is removed (cleanups run last-in first-out).
func openTestDB(t *testing.T) (*DB, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "image-client.db")
	db, err := Open(path)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db, path
}

// rawExec runs statements on a direct connection, bypassing the app's checks.
func rawExec(t *testing.T, path string, statements ...string) {
	t.Helper()
	conn, err := sqlite.OpenConn(path, sqlite.OpenReadWrite)
	if err != nil {
		t.Fatalf("raw open: %v", err)
	}
	defer conn.Close()
	for _, s := range statements {
		if err := sqlitex.ExecuteTransient(conn, s, nil); err != nil {
			t.Fatalf("raw %q: %v", s, err)
		}
	}
}

// rawInt reads one integer pragma such as PRAGMA user_version from path.
func rawInt(t *testing.T, path, query string) int {
	t.Helper()
	conn, err := sqlite.OpenConn(path, sqlite.OpenReadOnly)
	if err != nil {
		t.Fatalf("raw open: %v", err)
	}
	defer conn.Close()
	var value int
	err = sqlitex.ExecuteTransient(conn, query, &sqlitex.ExecOptions{
		ResultFunc: func(stmt *sqlite.Stmt) error { value = stmt.ColumnInt(0); return nil },
	})
	if err != nil {
		t.Fatalf("%s: %v", query, err)
	}
	return value
}

// rawText reads the first column of the first row of query from path.
func rawText(t *testing.T, path, query string) string {
	t.Helper()
	conn, err := sqlite.OpenConn(path, sqlite.OpenReadOnly)
	if err != nil {
		t.Fatalf("raw open: %v", err)
	}
	defer conn.Close()
	var value string
	err = sqlitex.ExecuteTransient(conn, query, &sqlitex.ExecOptions{
		ResultFunc: func(stmt *sqlite.Stmt) error { value = stmt.ColumnText(0); return nil },
	})
	if err != nil {
		t.Fatalf("%s: %v", query, err)
	}
	return value
}

func TestOpenCreatesLatestSchemaWithForeignKeys(t *testing.T) {
	db, path := openTestDB(t)
	if got := SchemaVersion(); got != 29 {
		t.Fatalf("SchemaVersion() = %d, want 29", got)
	}
	if got := rawInt(t, path, "PRAGMA user_version"); got != 29 {
		t.Fatalf("user_version = %d, want 29", got)
	}
	if err := db.With(context.Background(), func(conn *sqlite.Conn) error {
		fk := 0
		err := sqlitex.ExecuteTransient(conn, "PRAGMA foreign_keys", &sqlitex.ExecOptions{
			ResultFunc: func(stmt *sqlite.Stmt) error { fk = stmt.ColumnInt(0); return nil },
		})
		if fk != 1 {
			return fmt.Errorf("foreign_keys = %d, want 1", fk)
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
}

func TestReopenIsNoOpAndTakesNoBackup(t *testing.T) {
	_, path := openTestDB(t)
	db2, err := Open(path)
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	t.Cleanup(func() { db2.Close() })
	if got := rawInt(t, path, "PRAGMA user_version"); got != 29 {
		t.Fatalf("user_version after reopen = %d, want 29", got)
	}
	if _, err := os.Stat(filepath.Join(filepath.Dir(path), "backups")); !os.IsNotExist(err) {
		t.Fatalf("reopening an up-to-date database must not create backups: %v", err)
	}
}

// createClosedDB creates a database at latest schema and closes its pool, so the
// test can change the file directly. Callers must not register a second Close.
func createClosedDB(t *testing.T) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "image-client.db")
	db, err := Open(path)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestMigrationTakesVerifiedBackupBeforeUpgrade(t *testing.T) {
	path := createClosedDB(t)
	// Simulate a v28 database: no app_secrets table, version 28.
	rawExec(t, path, "PRAGMA user_version = 28", "DROP TABLE app_secrets")

	upgraded, err := Open(path)
	if err != nil {
		t.Fatalf("Open of a v28 database: %v", err)
	}
	t.Cleanup(func() { upgraded.Close() })

	if got := rawInt(t, path, "PRAGMA user_version"); got != 29 {
		t.Fatalf("user_version after migration = %d, want 29", got)
	}
	if n := rawInt(t, path, "SELECT count(*) FROM sqlite_master WHERE name = 'app_secrets'"); n != 1 {
		t.Fatalf("v29 migration did not recreate app_secrets")
	}

	backups, err := filepath.Glob(filepath.Join(filepath.Dir(path), "backups", "image-client-pre-migration-v28-*.sqlite"))
	if err != nil {
		t.Fatal(err)
	}
	if len(backups) != 1 {
		t.Fatalf("pre-migration backups = %v, want exactly one", backups)
	}
	backup := backups[0]
	if got := rawText(t, backup, "PRAGMA integrity_check"); got != "ok" {
		t.Fatalf("backup integrity_check = %q, want ok", got)
	}
	if got := rawInt(t, backup, "PRAGMA user_version"); got != 28 {
		t.Fatalf("backup user_version = %d, want 28 (the pre-migration state)", got)
	}
	if n := rawInt(t, backup, "SELECT count(*) FROM sqlite_master WHERE name = 'app_secrets'"); n != 0 {
		t.Fatalf("backup must be the pre-migration copy, but it already has app_secrets")
	}
}

func TestOpenRejectsNewerSchema(t *testing.T) {
	path := createClosedDB(t)
	rawExec(t, path, "PRAGMA user_version = 999")

	_, err := Open(path)
	if err == nil || !strings.Contains(err.Error(), "高于当前程序支持") {
		t.Fatalf("Open of v999 database: err = %v, want 高于当前程序支持", err)
	}
	if _, statErr := os.Stat(filepath.Join(filepath.Dir(path), "backups")); !os.IsNotExist(statErr) {
		t.Fatalf("a newer schema must fail before any backup is taken")
	}
}

func TestPruneBackupsKeepsThreeNewestByEmbeddedTimestamp(t *testing.T) {
	dir := t.TempDir()
	prefix := "image-client-pre-migration-v"
	// Lexical order disagrees with numeric order (9 > 10 as text), so pruning must parse the stamp.
	stamps := []int64{9, 10, 100, 2, 1}
	for _, s := range stamps {
		name := fmt.Sprintf("%s29-%d-%08d.sqlite", prefix, s, s)
		if err := os.WriteFile(filepath.Join(dir, name), []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	for _, keep := range []string{"unrelated.sqlite", prefix + "28-bad.sqlite"} {
		if err := os.WriteFile(filepath.Join(dir, keep), []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}

	if err := pruneBackups(dir, prefix); err != nil {
		t.Fatalf("pruneBackups: %v", err)
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	remaining := map[string]bool{}
	for _, e := range entries {
		remaining[e.Name()] = true
	}
	for _, s := range []int64{100, 10, 9} {
		if name := fmt.Sprintf("%s29-%d-%08d.sqlite", prefix, s, s); !remaining[name] {
			t.Errorf("newest backup %s was pruned", name)
		}
	}
	for _, s := range []int64{2, 1} {
		if name := fmt.Sprintf("%s29-%d-%08d.sqlite", prefix, s, s); remaining[name] {
			t.Errorf("old backup %s survived pruning", name)
		}
	}
	if !remaining["unrelated.sqlite"] || !remaining[prefix+"28-bad.sqlite"] {
		t.Errorf("files that are not backups must be left alone")
	}
}
