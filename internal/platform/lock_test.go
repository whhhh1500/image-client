package platform

import (
	"errors"
	"path/filepath"
	"testing"
)

func TestLockDataDirExcludesSecondInstance(t *testing.T) {
	dirA := filepath.Join(t.TempDir(), "a")
	dirB := filepath.Join(t.TempDir(), "b")

	first, err := LockDataDir(dirA)
	if err != nil {
		t.Fatalf("first lock: %v", err)
	}
	t.Cleanup(func() { first.Release() })

	if _, err := LockDataDir(dirA); !errors.Is(err, ErrAlreadyRunning) {
		t.Fatalf("second lock on same dir: err = %v, want ErrAlreadyRunning", err)
	}

	other, err := LockDataDir(dirB)
	if err != nil {
		t.Fatalf("lock on a different dir must succeed: %v", err)
	}
	t.Cleanup(func() { other.Release() })

	if err := first.Release(); err != nil {
		t.Fatalf("Release: %v", err)
	}
	again, err := LockDataDir(dirA)
	if err != nil {
		t.Fatalf("lock after Release: %v", err)
	}
	t.Cleanup(func() { again.Release() })
}

func TestInstanceLockReleaseIsIdempotentAndNilSafe(t *testing.T) {
	var nilLock *InstanceLock
	if err := nilLock.Release(); err != nil {
		t.Fatalf("nil Release: %v", err)
	}
	lock, err := LockDataDir(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if err := lock.Release(); err != nil {
		t.Fatalf("first Release: %v", err)
	}
	if err := lock.Release(); err != nil {
		t.Fatalf("second Release: %v", err)
	}
}
