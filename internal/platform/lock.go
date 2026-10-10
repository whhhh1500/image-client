package platform

import (
	"errors"
	"os"
	"path/filepath"
)

// ErrAlreadyRunning means another process holds the data directory.
var ErrAlreadyRunning = errors.New("another instance is using this data directory")

// InstanceLock is held for the life of the process. Startup marks running
// comic jobs as interrupted, so a second process on the same data directory
// would break the first one's work in progress. The lock is keyed on the
// directory, so isolated runs with their own IMAGE_CLIENT_DATA_DIR still run
// side by side; the OS releases it when the process exits, even on a crash.
type InstanceLock struct{ file *os.File }

// LockDataDir takes the instance lock in dir.
func LockDataDir(dir string) (*InstanceLock, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, err
	}
	file, err := os.OpenFile(filepath.Join(dir, ".instance.lock"), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, err
	}
	if err := tryLock(file); err != nil {
		file.Close()
		return nil, err
	}
	return &InstanceLock{file: file}, nil
}

// Release drops the lock early (tests); normally process exit releases it.
func (l *InstanceLock) Release() error {
	if l == nil || l.file == nil {
		return nil
	}
	unlock(l.file)
	err := l.file.Close()
	l.file = nil
	return err
}
