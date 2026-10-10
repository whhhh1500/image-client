//go:build !windows

package platform

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
)

// Alert reports a startup problem: a native dialog where one is available
// without a window, stderr otherwise.
func Alert(title, message string) {
	fmt.Fprintf(os.Stderr, "%s: %s\n", title, message)
	if runtime.GOOS == "darwin" {
		script := fmt.Sprintf("display dialog %q with title %q buttons {\"OK\"} default button 1", message, title)
		exec.Command("osascript", "-e", script).Run()
	}
}
