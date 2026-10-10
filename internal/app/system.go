package app

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"

	wails "github.com/wailsapp/wails/v2/pkg/runtime"

	"imageclient/internal/config"
	"imageclient/internal/ipc"
	"imageclient/internal/platform"
)

// URLs the frontend may open in the browser: the gateway invite link and the
// project repository. Anything else is refused, as the Tauri capability did.
const (
	inviteURL = "https://oenai.cc.cd/j/i1.cmVzZWxsZXIAZmQxeVp2TEY1V3hJb0JWeDltNDYwYlpS.cn5C6uhqhbNTFeR46uZf1TPAP-3Uf7-zsIcsDZovdK8"
	repoURL   = "https://github.com/whhhh1500/image-client"
)

func (a *App) registerSystem(r *ipc.Router) {
	r.Handle("app_info", ipc.Typed(func(context.Context, struct{}) (appInfo, error) {
		platformName := runtime.GOOS
		if platformName == "darwin" {
			platformName = "macos"
		}
		return appInfo{Name: ProductName, Version: a.version, Platform: platformName}, nil
	}))
	r.Handle("data_dir", ipc.Typed(func(context.Context, struct{}) (string, error) {
		return a.paths.Slash(), nil
	}))
	r.Handle("logs_dir", ipc.Typed(func(context.Context, struct{}) (string, error) {
		return filepath.ToSlash(a.paths.Logs()), nil
	}))
	r.Handle("client_logs", ipc.Void(func(_ context.Context, in struct {
		Entries []clientLogEntry `json:"entries"`
	}) error {
		for _, entry := range in.Entries[:min(len(in.Entries), 100)] {
			var fields map[string]any
			if len(entry.Fields) > 0 && json.Unmarshal(entry.Fields, &fields) != nil {
				fields = map[string]any{"value": string(entry.Fields)}
			}
			a.logs.Record(platform.ParseLevel(entry.Level), "frontend", entry.Event, fields)
		}
		return nil
	}))

	r.Handle("config_status", ipc.Typed(func(context.Context, struct{}) (config.Status, error) {
		return a.config.Status(), nil
	}))
	r.Handle("save_config", ipc.Typed(func(_ context.Context, in struct {
		Config config.SaveRequest `json:"config"`
	}) (config.Status, error) {
		return a.config.Save(in.Config, "db")
	}))

	type sqlArgs struct {
		Query      string            `json:"query"`
		BindValues []json.RawMessage `json:"bindValues"`
	}
	r.Handle("db_execute", ipc.Typed(func(ctx context.Context, in sqlArgs) (any, error) {
		return a.db.FrontendExec(ctx, in.Query, in.BindValues)
	}))
	r.Handle("db_select", ipc.Typed(func(ctx context.Context, in sqlArgs) (any, error) {
		return a.db.FrontendSelect(ctx, in.Query, in.BindValues)
	}))

	r.Handle("mark_history_listener_ready", ipc.Void(func(context.Context, struct{}) error {
		a.feed.MarkListenerReady()
		return nil
	}))
	r.Handle("acknowledge_history_revision", ipc.Void(func(_ context.Context, in struct {
		Revision uint64 `json:"revision"`
	}) error {
		a.feed.Acknowledge(in.Revision)
		return nil
	}))

	r.Handle("dialog_open", ipc.Typed(a.openDialog))
	r.Handle("dialog_confirm", ipc.Typed(a.confirmDialog))
	r.Handle("reveal_item_in_dir", ipc.Void(func(_ context.Context, in pathArg) error {
		return revealInFileManager(in.Path)
	}))
	r.Handle("open_path", ipc.Void(func(_ context.Context, in pathArg) error {
		// Only the logs directory may be opened, as the Tauri capability allowed.
		if !within(a.paths.Logs(), in.Path) {
			return fmt.Errorf("不允许打开该路径")
		}
		return openWithSystem(in.Path)
	}))
	r.Handle("open_url", ipc.Void(func(_ context.Context, in struct {
		URL string `json:"url"`
	}) error {
		if in.URL != inviteURL && in.URL != repoURL && !strings.HasPrefix(in.URL, repoURL+"/") {
			return fmt.Errorf("不允许打开该链接")
		}
		wails.BrowserOpenURL(a.windowContext(), in.URL)
		return nil
	}))
}

type appInfo struct {
	Name     string `json:"name"`
	Version  string `json:"version"`
	Platform string `json:"platform"`
}

type clientLogEntry struct {
	Level  string          `json:"level"`
	Event  string          `json:"event"`
	Fields json.RawMessage `json:"fields"`
}

type pathArg struct {
	Path string `json:"path"`
}

type dialogOptions struct {
	Options struct {
		Multiple  bool `json:"multiple"`
		Directory bool `json:"directory"`
		Filters   []struct {
			Name       string   `json:"name"`
			Extensions []string `json:"extensions"`
		} `json:"filters"`
	} `json:"options"`
}

// openDialog returns a path, a list of paths (multiple), or nil when cancelled.
func (a *App) openDialog(_ context.Context, in dialogOptions) (any, error) {
	opts := wails.OpenDialogOptions{}
	for _, f := range in.Options.Filters {
		patterns := make([]string, len(f.Extensions))
		for i, ext := range f.Extensions {
			patterns[i] = "*." + ext
		}
		opts.Filters = append(opts.Filters, wails.FileFilter{DisplayName: f.Name, Pattern: strings.Join(patterns, ";")})
	}
	switch {
	case in.Options.Directory:
		dir, err := wails.OpenDirectoryDialog(a.windowContext(), opts)
		if err != nil || dir == "" {
			return nil, err
		}
		return dir, nil
	case in.Options.Multiple:
		files, err := wails.OpenMultipleFilesDialog(a.windowContext(), opts)
		if err != nil || len(files) == 0 {
			return nil, err
		}
		return files, nil
	default:
		file, err := wails.OpenFileDialog(a.windowContext(), opts)
		if err != nil || file == "" {
			return nil, err
		}
		return file, nil
	}
}

func (a *App) confirmDialog(_ context.Context, in struct {
	Message     string `json:"message"`
	Title       string `json:"title"`
	OKLabel     string `json:"okLabel"`
	CancelLabel string `json:"cancelLabel"`
}) (bool, error) {
	ok, cancel := cmpOr(in.OKLabel, "确定"), cmpOr(in.CancelLabel, "取消")
	choice, err := wails.MessageDialog(a.windowContext(), wails.MessageDialogOptions{
		Type:          wails.QuestionDialog,
		Title:         cmpOr(in.Title, ProductName),
		Message:       in.Message,
		Buttons:       []string{ok, cancel},
		DefaultButton: ok,
		CancelButton:  cancel,
	})
	if err != nil {
		return false, err
	}
	// Windows question dialogs always use Yes/No and ignore custom labels.
	return choice == ok || choice == "Yes", nil
}

func cmpOr(value, fallback string) string {
	if strings.TrimSpace(value) == "" {
		return fallback
	}
	return value
}

func within(root, path string) bool {
	rel, err := filepath.Rel(filepath.Clean(root), filepath.Clean(path))
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) && !filepath.IsAbs(rel)
}

func revealInFileManager(path string) error {
	if _, err := os.Stat(path); err != nil {
		return fmt.Errorf("文件不存在: %w", err)
	}
	switch runtime.GOOS {
	case "windows":
		// explorer.exe returns 1 even on success.
		exec.Command("explorer.exe", "/select,", filepath.Clean(path)).Start()
		return nil
	case "darwin":
		return exec.Command("open", "-R", path).Start()
	default:
		return exec.Command("xdg-open", filepath.Dir(path)).Start()
	}
}

func openWithSystem(path string) error {
	switch runtime.GOOS {
	case "windows":
		exec.Command("explorer.exe", filepath.Clean(path)).Start()
		return nil
	case "darwin":
		return exec.Command("open", path).Start()
	default:
		return exec.Command("xdg-open", path).Start()
	}
}
