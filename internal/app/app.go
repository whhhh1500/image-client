// Package app assembles the backend: it opens the data directory, database and
// configuration, wires the domain services to the IPC router, and runs the
// startup and shutdown sequence around the Wails window.
package app

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"runtime"
	"sync"
	"time"

	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
	wails "github.com/wailsapp/wails/v2/pkg/runtime"

	"imageclient/internal/config"
	"imageclient/internal/ipc"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

// ProductName is the window title and the name app_info reports.
const ProductName = "Image-Client"

// App holds the long-lived backend state.
type App struct {
	version string
	paths   platform.Paths
	logs    *platform.LogSink
	db      *store.DB
	config  *config.Store
	feed    *store.HistoryFeed
	router  *ipc.Router

	mu            sync.RWMutex
	ctx           context.Context   // the Wails context; nil until the window exists
	legacyStorage map[string]string // Tauri-era localStorage, until the frontend takes it

	closers []func(context.Context) error
}

// New opens everything the window needs. The caller holds the instance lock.
// webviewDir is the WebView2 profile the window will use; Tauri-era UI state
// is read from it before the browser starts.
func New(version string, paths platform.Paths, webviewDir string) (*App, error) {
	if err := paths.Ensure(); err != nil {
		return nil, fmt.Errorf("创建应用目录失败：%w", err)
	}
	logs, err := platform.OpenLogSink(paths.Logs())
	if err != nil {
		return nil, err
	}
	slog.SetDefault(slog.New(logs.Handler()))
	slog.Info("logging.initialized", "directory", paths.Logs(), "format", "jsonl")

	db, err := store.Open(paths.Database())
	if err != nil {
		logs.Close()
		return nil, err
	}
	slog.Info("application.start", "name", ProductName, "version", version, "platform", runtime.GOOS,
		"arch", runtime.GOARCH, "dataDir", paths.Root, "dbPath", paths.Database(), "logsDir", paths.Logs())

	cfg := config.Load(paths)
	slog.Info("configuration.loaded", "imageReady", cfg.Get().ImageReady(), "videoReady", cfg.Get().VideoReady(),
		"llmReady", cfg.Get().LLMReady(), "source", cfg.Get().Source)

	a := &App{
		version: version,
		paths:   paths,
		logs:    logs,
		db:      db,
		config:  cfg,
		feed:    &store.HistoryFeed{},
		router:  ipc.NewRouter(),
	}
	a.registerSystem(a.router)
	a.loadLegacyLocalStorage(webviewDir)
	a.registerLegacyStorage(a.router)
	if err := a.wireServices(); err != nil {
		a.Shutdown(context.Background())
		return nil, err
	}
	return a, nil
}

func (a *App) windowContext() context.Context {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.ctx
}

// Emit sends an event to the window; events before the window exists are dropped.
func (a *App) Emit(event string, payload any) {
	if ctx := a.windowContext(); ctx != nil {
		wails.EventsEmit(ctx, event, payload)
	}
}

// Startup receives the Wails context once the window exists.
func (a *App) Startup(ctx context.Context) {
	a.mu.Lock()
	a.ctx = ctx
	a.mu.Unlock()
}

// onClose registers a shutdown step; steps run in reverse order.
func (a *App) onClose(fn func(context.Context) error) { a.closers = append(a.closers, fn) }

// Shutdown stops background work, then closes the database and the log.
func (a *App) Shutdown(ctx context.Context) {
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	for i := len(a.closers) - 1; i >= 0; i-- {
		if err := a.closers[i](ctx); err != nil {
			slog.Warn("application.shutdown_step_failed", "error", err.Error())
		}
	}
	if err := a.db.Close(); err != nil {
		slog.Warn("database.close_failed", "error", err.Error())
	}
	slog.Info("application.exit", "status", "ok")
	a.logs.Close()
}

// Bridge is the one object bound to the frontend: window.go.app.Bridge.
type Bridge struct{ app *App }

// Bridge returns the object to bind in Wails.
func (a *App) Bridge() *Bridge { return &Bridge{app: a} }

// Invoke runs a backend command for the frontend. Errors reach JavaScript as
// the rejected promise's message.
func (b *Bridge) Invoke(command string, args json.RawMessage) (any, error) {
	started := time.Now()
	ctx := b.app.windowContext()
	if ctx == nil {
		ctx = context.Background()
	}
	result, err := b.app.router.Invoke(ctx, command, args)
	ipc.Logged(command, started, err)
	return result, err
}

// AssetMiddleware serves local files to the page (see files.go).
func (a *App) AssetMiddleware(enforceCSP bool) assetserver.Middleware {
	return assetMiddleware(enforceCSP)
}
