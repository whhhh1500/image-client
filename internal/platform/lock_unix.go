//go:build !windows

package platform

import (
	"errors"
	"os"
	"syscall"
)

func tryLock(file *os.File) error {
	err := syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
	if errors.Is(err, syscall.EWOULDBLOCK) {
		return ErrAlreadyRunning
	}
	return err
}

func unlock(file *os.File) { syscall.Flock(int(file.Fd()), syscall.LOCK_UN) }
