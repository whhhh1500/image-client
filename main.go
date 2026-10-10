// Image-Client desktop entry point: a Wails window around the React frontend
// in dist/, with the Go backend in internal/.
package main

import (
	"embed"
	"encoding/json"
	"errors"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"

	"github.com/wailsapp/wails/v2"
	"github.com/wailsapp/wails/v2/pkg/options"
	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
	"github.com/wailsapp/wails/v2/pkg/options/windows"

	"imageclient/internal/app"
	"imageclient/internal/platform"
)

//go:embed all:dist
var dist embed.FS

// wails.json carries the product version, so it is the single place a
// release bumps it for the Go side.
//
//go:embed wails.json
var projectFile []byte

func main() {
	paths, err := platform.ResolvePaths()
	if err != nil {
		platform.Alert(app.ProductName, "数据目录配置无效："+err.Error())
		os.Exit(2)
	}
	lock, err := platform.LockDataDir(paths.Root)
	if errors.Is(err, platform.ErrAlreadyRunning) {
		platform.Alert(app.ProductName, "Image-Client 已在运行，请切换到已打开的窗口。\n同时打开两个会中断正在进行的生成任务。")
		return
	}
	// Any other locking failure (unusual file system) starts without the lock,
	// as before; directory problems surface during setup.
	defer lock.Release()

	webviewDir := webviewDataPath(paths)
	backend, err := app.New(productVersion(), paths, webviewDir)
	if err != nil {
		platform.Alert(app.ProductName, "启动失败："+err.Error())
		os.Exit(1)
	}

	assets, _ := fs.Sub(dist, "dist")
	err = wails.Run(&options.App{
		Title:            app.ProductName,
		Width:            1280,
		Height:           800,
		MinWidth:         960,
		MinHeight:        640,
		BackgroundColour: &options.RGBA{R: 2, G: 6, B: 23, A: 255},
		AssetServer: &assetserver.Options{
			Assets:     assets,
			Middleware: backend.AssetMiddleware(enforceCSP),
		},
		OnStartup:  backend.Startup,
		OnShutdown: backend.Shutdown,
		Bind:       []any{backend.Bridge()},
		Windows: &windows.Options{
			WebviewUserDataPath: webviewDir,
		},
	})
	if err != nil {
		slog.Error("application.exit", "status", "error", "error", err.Error())
		platform.Alert(app.ProductName, "窗口启动失败："+err.Error())
		os.Exit(1)
	}
}

func productVersion() string {
	var project struct {
		Info struct {
			ProductVersion string `json:"productVersion"`
		} `json:"info"`
	}
	if json.Unmarshal(projectFile, &project) != nil || project.Info.ProductVersion == "" {
		return "0.0.0"
	}
	return project.Info.ProductVersion
}

// webviewDataPath keeps WebView2 state where the Tauri build kept it, except
// for isolated runs (IMAGE_CLIENT_DATA_DIR), whose profile lives inside their
// own data root so they never share browser state with the real app.
func webviewDataPath(paths platform.Paths) string {
	if platform.OverrideActive() {
		return filepath.Join(paths.Root, "webview2", "main")
	}
	if local, err := os.UserCacheDir(); err == nil {
		return filepath.Join(local, "com.imageclient.desktop")
	}
	return ""
}
