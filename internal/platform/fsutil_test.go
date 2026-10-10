package platform

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestWriteFileAtomicCreatesParentsAndReplaces(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "nested", "deeper")
	path := filepath.Join(dir, "backend-config.json")

	if err := WriteFileAtomic(path, []byte("first")); err != nil {
		t.Fatalf("first write: %v", err)
	}
	if got, _ := os.ReadFile(path); string(got) != "first" {
		t.Fatalf("content = %q, want first", got)
	}

	if err := WriteFileAtomic(path, []byte("second, longer content")); err != nil {
		t.Fatalf("replace: %v", err)
	}
	if got, _ := os.ReadFile(path); string(got) != "second, longer content" {
		t.Fatalf("replaced content = %q", got)
	}

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].Name() != "backend-config.json" {
		var names []string
		for _, e := range entries {
			names = append(names, e.Name())
		}
		t.Fatalf("directory has leftovers: %s", strings.Join(names, ", "))
	}
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".tmp") {
			t.Fatalf("temporary file left behind: %s", e.Name())
		}
	}
}
