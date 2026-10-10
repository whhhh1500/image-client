package platform

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"

	"github.com/google/uuid"
)

// MkdirPrivate creates dir (and parents), readable only by the user on Unix.
func MkdirPrivate(dir string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	return SecureDir(dir)
}

// SecureDir restricts a directory to the owner on Unix; Windows relies on the
// profile ACLs of the data root.
func SecureDir(dir string) error {
	if runtime.GOOS == "windows" {
		return nil
	}
	return os.Chmod(dir, 0o700)
}

// SecureFile restricts an existing file to the owner on Unix.
func SecureFile(path string) error {
	if runtime.GOOS == "windows" {
		return nil
	}
	if _, err := os.Stat(path); err != nil {
		return nil
	}
	return os.Chmod(path, 0o600)
}

// WriteFileAtomic writes data to a temporary sibling and renames it over path,
// so readers never observe a partially written file.
func WriteFileAtomic(path string, data []byte) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	tmp := filepath.Join(dir, fmt.Sprintf(".%s-%s.tmp", filepath.Base(path), uuid.NewString()))
	if err := os.WriteFile(tmp, data, 0o600); err != nil {
		return err
	}
	if err := os.Rename(tmp, path); err != nil {
		os.Remove(tmp)
		return err
	}
	return SecureFile(path)
}
