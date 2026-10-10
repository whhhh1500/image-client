package platform

import "golang.org/x/sys/windows"

// Alert shows a blocking native message box. It works before any window
// exists, which is when startup failures happen.
func Alert(title, message string) {
	t, _ := windows.UTF16PtrFromString(title)
	m, _ := windows.UTF16PtrFromString(message)
	windows.MessageBox(0, m, t, windows.MB_OK|windows.MB_ICONINFORMATION|windows.MB_SETFOREGROUND)
}
