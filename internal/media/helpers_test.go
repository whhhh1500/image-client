package media

import (
	"bytes"
	"context"
	"encoding/json"
	"image"
	"image/color"
	"image/png"
	"os"
	"path/filepath"
	"testing"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/config"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

type testEnv struct {
	svc    *Service
	db     *store.DB
	paths  platform.Paths
	output string
}

// newTestEnv builds a service over a temporary data root whose output
// directory is <tmp>/output, with a migrated database.
func newTestEnv(t *testing.T) testEnv {
	t.Helper()
	root := t.TempDir()
	paths := platform.Paths{Root: filepath.Join(root, "data")}
	if err := paths.Ensure(); err != nil {
		t.Fatal(err)
	}
	output := filepath.Join(root, "output")
	snapshot, _ := json.Marshal(config.Settings{OutputDir: output, Source: "test"})
	if err := os.WriteFile(paths.BackendConfig(), snapshot, 0o600); err != nil {
		t.Fatal(err)
	}
	db, err := store.Open(paths.Database())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	svc := New(Deps{Paths: paths, Config: config.Load(paths), DB: db})
	return testEnv{svc: svc, db: db, paths: paths, output: output}
}

func (e testEnv) exec(t *testing.T, query string, args ...any) {
	t.Helper()
	err := e.db.With(context.Background(), func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, query, &sqlitex.ExecOptions{Args: args})
	})
	if err != nil {
		t.Fatalf("%s: %v", query, err)
	}
}

func (e testEnv) queryText(t *testing.T, query string, args ...any) []string {
	t.Helper()
	var rows []string
	err := e.db.With(context.Background(), func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, query, &sqlitex.ExecOptions{Args: args, ResultFunc: func(stmt *sqlite.Stmt) error {
			rows = append(rows, stmt.ColumnText(0))
			return nil
		}})
	})
	if err != nil {
		t.Fatalf("%s: %v", query, err)
	}
	return rows
}

func countFiles(t *testing.T, root string) int {
	t.Helper()
	n := 0
	filepath.WalkDir(root, func(_ string, d os.DirEntry, err error) error {
		if err == nil && !d.IsDir() {
			n++
		}
		return nil
	})
	return n
}

func writeFile(t *testing.T, path string, data []byte) string {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

// testImage is a w×h NRGBA image with varied colours and some transparency.
func testImage(w, h int) *image.NRGBA {
	img := image.NewNRGBA(image.Rect(0, 0, w, h))
	for y := range h {
		for x := range w {
			img.SetNRGBA(x, y, color.NRGBA{R: uint8(x * 7), G: uint8(y * 13), B: uint8(x*y + 3), A: uint8(255 - (x+y)%3*60)})
		}
	}
	return img
}

func pngBytes(t *testing.T, img image.Image) []byte {
	t.Helper()
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func errText(err error) string {
	if err == nil {
		return "<nil>"
	}
	return err.Error()
}
