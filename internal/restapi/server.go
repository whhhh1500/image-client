// Package restapi serves the local HTTP API that scripts and external tools
// use while the app runs (127.0.0.1:8123, base path /api/v1, legacy aliases
// under /api). It exposes configuration, model catalogues, text and agent
// runs, image and video generation and asset saving. Everything it produces
// is recorded in the asset history and announced to the window with a
// history://changed event.
//
// The API has no authentication: it binds to loopback, and only the app's own
// page origins (plus API_CORS_ORIGINS) are granted CORS access.
package restapi

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"mime"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"

	"imageclient/internal/config"
	"imageclient/internal/ipc"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

const (
	// DefaultAddr is where the API listens unless API_HOST / API_PORT say otherwise.
	DefaultAddr = "127.0.0.1:8123"
	// ShutdownTimeout bounds the drain when Shutdown's context has no deadline.
	ShutdownTimeout = 3 * time.Second

	// maxJSONBody is the request body limit of every JSON route but media upload.
	maxJSONBody = 64 << 20
	// maxMediaAssetBytes is the largest media file /assets/media accepts.
	maxMediaAssetBytes = 512 << 20
	// maxMediaBody is the base64 of maxMediaAssetBytes plus the JSON envelope.
	maxMediaBody = (maxMediaAssetBytes/3+1)*4 + 64<<10
)

// Deps are the collaborators of the API server. All are required except
// Version, Addr, Events and Feed.
type Deps struct {
	Config *config.Store
	DB     *store.DB
	// Feed numbers history changes; the window acknowledges them over IPC, so
	// the app passes the feed it shares with those commands.
	Feed *store.HistoryFeed
	// Events receives store.HistoryChangedEvent with a store.HistoryChange.
	Events ipc.Emitter
	Paths  platform.Paths
	// Version is the app version /system/info reports.
	Version string

	Images  ImageGenerator
	Videos  VideoGenerator
	Text    TextModel
	Catalog ModelCatalog
	Files   AssetFiles

	// Addr is the listen address. Empty means API_HOST and API_PORT from the
	// environment, defaulting to DefaultAddr.
	Addr string
}

// Server is the REST API. Create it with New, then Start and Shutdown once.
type Server struct {
	cfg     *config.Store
	db      *store.DB
	feed    *store.HistoryFeed
	events  ipc.Emitter
	paths   platform.Paths
	version string
	images  ImageGenerator
	videos  VideoGenerator
	text    TextModel
	catalog ModelCatalog
	files   AssetFiles

	addr    string
	origins map[string]bool
	mux     *http.ServeMux
	routed  map[string]bool // every routed path, to tell 404 from 405
	handler http.Handler

	mu             sync.Mutex
	started        bool
	srv            *http.Server
	listenAddr     string
	cancelRequests context.CancelFunc
	stopOnce       sync.Once
	stopErr        error
}

// New builds the server; it does not listen until Start.
func New(d Deps) *Server {
	s := &Server{
		cfg: d.Config, db: d.DB, feed: d.Feed, events: d.Events, paths: d.Paths, version: d.Version,
		images: d.Images, videos: d.Videos, text: d.Text, catalog: d.Catalog, files: d.Files,
		addr:    d.Addr,
		origins: corsOrigins(os.Getenv("API_CORS_ORIGINS")),
		mux:     http.NewServeMux(),
		routed:  map[string]bool{},
	}
	if s.feed == nil {
		s.feed = &store.HistoryFeed{}
	}
	if s.events == nil {
		s.events = ipc.Discard
	}
	if s.addr == "" {
		s.addr = envAddr(os.LookupEnv)
	}
	for _, rt := range append(s.routes(), s.legacyRoutes()...) {
		s.mux.HandleFunc(rt.method+" "+rt.path, rt.handle)
		s.routed[rt.path] = true
	}
	s.handler = s.logRequests(s.cors(http.HandlerFunc(s.dispatch)))
	return s
}

// envAddr reads API_HOST (default 127.0.0.1) and API_PORT (default 8123; an
// unparsable port falls back to the default).
func envAddr(lookup func(string) (string, bool)) string {
	host, ok := lookup("API_HOST")
	if !ok || host == "" {
		host = "127.0.0.1"
	}
	port := "8123"
	if value, ok := lookup("API_PORT"); ok {
		if n, err := strconv.ParseUint(value, 10, 16); err == nil {
			port = strconv.FormatUint(n, 10)
		}
	}
	return net.JoinHostPort(host, port)
}

// Start listens and serves in the background; ctx bounds only the listen
// call. A taken port is logged and skipped, as before: the app keeps running
// without the API, and Start still returns nil. Start fails only when called
// twice or after Shutdown.
func (s *Server) Start(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.started {
		return errors.New("REST API 服务已启动或已停止")
	}
	s.started = true

	host, _, _ := net.SplitHostPort(s.addr)
	switch host {
	case "127.0.0.1", "localhost", "::1":
	default:
		slog.Warn("api.server.non_loopback_binding", "host", host, "authentication", "none", "risk", "network_exposure")
	}
	ln, err := (&net.ListenConfig{}).Listen(ctx, "tcp", s.addr)
	if err != nil {
		slog.Error("api.server.bind_failed", "address", s.addr, "error", err.Error())
		return nil
	}
	// Requests outlive the caller's context; only a forced stop cancels them.
	base, cancel := context.WithCancel(context.WithoutCancel(ctx))
	s.cancelRequests = cancel
	s.srv = &http.Server{
		Handler:           s.handler,
		ReadHeaderTimeout: 30 * time.Second,
		IdleTimeout:       2 * time.Minute,
		// No read/write timeouts: uploads reach 700 MB and generations run for minutes.
		BaseContext: func(net.Listener) context.Context { return base },
	}
	s.listenAddr = ln.Addr().String()
	slog.Info("api.server.started", "address", s.listenAddr, "basePath", "/api/v1", "auth", "none")
	go func(srv *http.Server) {
		if err := srv.Serve(ln); !errors.Is(err, http.ErrServerClosed) {
			slog.Error("api.server.stopped", "reason", "serve_error", "error", err.Error())
		}
	}(s.srv)
	return nil
}

// Addr is the address the server listens on, or "" when it is not serving.
func (s *Server) Addr() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.listenAddr
}

// Shutdown stops accepting connections and waits for in-flight requests until
// ctx ends (ShutdownTimeout when ctx has no deadline). Requests still running
// then are cancelled and their connections closed, and the error says so.
// Later and concurrent calls wait for the first drain and return its result.
func (s *Server) Shutdown(ctx context.Context) error {
	s.stopOnce.Do(func() { s.stopErr = s.drain(ctx) })
	return s.stopErr
}

func (s *Server) drain(ctx context.Context) error {
	s.mu.Lock()
	s.started = true // a Start after Shutdown must not serve
	srv, cancelRequests := s.srv, s.cancelRequests
	s.listenAddr = ""
	s.mu.Unlock()
	if srv == nil {
		return nil
	}
	defer cancelRequests()

	timeout := ShutdownTimeout
	if deadline, ok := ctx.Deadline(); ok {
		timeout = time.Until(deadline)
	} else {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, ShutdownTimeout)
		defer cancel()
	}
	err := srv.Shutdown(ctx)
	if err == nil {
		slog.Info("api.server.stopped", "reason", "graceful")
		return nil
	}
	// Drop the stragglers' connections first, so no late answer goes out,
	// then cancel their work.
	srv.Close()
	cancelRequests()
	slog.Warn("api.server.shutdown_timeout", "timeoutMs", timeout.Milliseconds())
	return fmt.Errorf("REST API 未能在时限内处理完进行中的请求：%w", err)
}

// dispatch routes a request. Unknown paths get the JSON 404; a known path
// with another method gets the mux's 405.
func (s *Server) dispatch(w http.ResponseWriter, r *http.Request) {
	if !s.routed[r.URL.EscapedPath()] {
		writeJSON(w, http.StatusNotFound, errorBody("not_found", "接口不存在，请读取 GET /api/v1/system/info"))
		return
	}
	s.mux.ServeHTTP(w, r)
}

// logRequests tags every response with x-request-id and logs its start and end.
func (s *Server) logRequests(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		id := uuid.NewString()
		path := r.URL.EscapedPath()
		var route, contentLength any
		if s.routed[path] {
			route = path
		}
		if n, err := strconv.ParseUint(r.Header.Get("Content-Length"), 10, 64); err == nil {
			contentLength = n
		}
		slog.Info("api.request.start", "requestId", id, "method", r.Method, "path", path, "route", route, "contentLength", contentLength)
		started := time.Now()
		rec := &statusRecorder{ResponseWriter: w, status: http.StatusOK}
		rec.Header().Set("X-Request-Id", id)
		defer func() {
			if p := recover(); p != nil {
				if p != http.ErrAbortHandler {
					slog.Error("api.request.panic", "requestId", id, "path", path, "panic", fmt.Sprint(p))
				}
				panic(http.ErrAbortHandler) // drop the connection, as an aborted Rust handler did
			}
		}()
		next.ServeHTTP(rec, r)
		slog.Info("api.request.end", "requestId", id, "method", r.Method, "path", path, "route", route,
			"status", rec.status, "durationMs", time.Since(started).Milliseconds())
	})
}

type statusRecorder struct {
	http.ResponseWriter
	status int
	wrote  bool
}

func (r *statusRecorder) WriteHeader(code int) {
	if !r.wrote {
		r.status, r.wrote = code, true
	}
	r.ResponseWriter.WriteHeader(code)
}

func (r *statusRecorder) Write(b []byte) (int, error) {
	r.wrote = true
	return r.ResponseWriter.Write(b)
}

func (r *statusRecorder) Unwrap() http.ResponseWriter { return r.ResponseWriter }

// defaultOrigins are the app's own pages: the Vite dev server and the Wails
// page origin (http://wails.localhost on Windows, wails://wails elsewhere).
var defaultOrigins = []string{"http://localhost:1420", "http://127.0.0.1:1420", "http://wails.localhost", "wails://wails"}

// corsOrigins adds the comma-separated API_CORS_ORIGINS to the defaults. A
// wildcard cannot mean "any" in an allowlist and is ignored with a warning.
func corsOrigins(configured string) map[string]bool {
	origins := map[string]bool{}
	for _, origin := range defaultOrigins {
		origins[origin] = true
	}
	for _, origin := range strings.Split(configured, ",") {
		origin = strings.TrimSpace(origin)
		if origin == "*" {
			slog.Warn("api.cors.wildcard_ignored", "variable", "API_CORS_ORIGINS")
			continue
		}
		if origin != "" && validHeaderValue(origin) {
			origins[origin] = true
		}
	}
	return origins
}

func validHeaderValue(v string) bool {
	for i := 0; i < len(v); i++ {
		if b := v[i]; (b < ' ' && b != '\t') || b == 0x7f {
			return false
		}
	}
	return true
}

// cors grants allowlisted origins access. Every OPTIONS request is answered
// here as a preflight, whatever its path.
func (s *Server) cors(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		if origin := r.Header.Get("Origin"); origin != "" && s.origins[origin] {
			h.Set("Access-Control-Allow-Origin", origin)
		}
		h.Add("Vary", "origin,access-control-request-method,access-control-request-headers")
		if r.Method == http.MethodOptions {
			h.Set("Access-Control-Allow-Methods", "GET,POST,PUT,OPTIONS")
			h.Set("Access-Control-Allow-Headers", "*")
			w.WriteHeader(http.StatusOK)
			return
		}
		next.ServeHTTP(w, r)
	})
}

// writeJSON writes v compactly, without HTML escaping.
func writeJSON(w http.ResponseWriter, status int, v any) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		slog.Error("api.response.encode_failed", "error", err.Error())
		reject(w, http.StatusInternalServerError, "响应序列化失败")
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	w.Write(bytes.TrimSuffix(buf.Bytes(), []byte("\n")))
}

func errorBody(code, message string) map[string]any {
	return map[string]any{"error": map[string]any{"code": code, "message": message}}
}

// fail answers 400 with the error message, scrubbed of credentials.
func fail(w http.ResponseWriter, err error) {
	message := platform.ErrorText(err.Error())
	slog.Warn("api.operation.error", "error", message)
	writeJSON(w, http.StatusBadRequest, errorBody("bad_request", message))
}

// reject answers a request whose body could not be read as the route's JSON,
// in plain text like the Rust server's JSON extractor did.
func reject(w http.ResponseWriter, status int, message string) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.WriteHeader(status)
	io.WriteString(w, message)
}

// fieldRules is implemented by request bodies with fields that must be
// present and non-null (plain, non-defaulted fields of the Rust structs).
type fieldRules interface {
	checkFields(fieldSet) error
}

// fieldSet maps the top-level keys of a JSON object to whether each is null.
type fieldSet map[string]jsonNull

type jsonNull bool

func (n *jsonNull) UnmarshalJSON(b []byte) error {
	*n = jsonNull(bytes.Equal(b, []byte("null")))
	return nil
}

// require fails unless every name is present with a non-null value.
func (f fieldSet) require(names ...string) error {
	for _, name := range names {
		null, ok := f[name]
		if !ok {
			return fmt.Errorf("missing field `%s`", name)
		}
		if null {
			return fmt.Errorf("%s: invalid type: null", name)
		}
	}
	return nil
}

// notNull fails when an optional-but-defaulted field is given as null.
func (f fieldSet) notNull(names ...string) error {
	for _, name := range names {
		if f[name] {
			return fmt.Errorf("%s: invalid type: null", name)
		}
	}
	return nil
}

// readJSON decodes the body into dst. On failure it answers like the Rust
// server's JSON extractor: 415 without a JSON content type, 413 over limit,
// 400 for malformed JSON and 422 when the value does not fit dst.
func readJSON(w http.ResponseWriter, r *http.Request, limit int64, dst any) bool {
	if !isJSONContentType(r.Header.Get("Content-Type")) {
		reject(w, http.StatusUnsupportedMediaType, "Expected request with `Content-Type: application/json`")
		return false
	}
	body, err := readBody(w, r, limit)
	if err != nil {
		if _, tooLarge := errors.AsType[*http.MaxBytesError](err); tooLarge {
			reject(w, http.StatusRequestEntityTooLarge, "Failed to buffer the request body: length limit exceeded")
		} else {
			reject(w, http.StatusBadRequest, "Failed to buffer the request body: "+err.Error())
		}
		return false
	}
	// Unmarshal checks the whole document's syntax before decoding, so a
	// syntax error is never reported as a type error.
	if err := json.Unmarshal(body, dst); err != nil {
		if _, syntax := errors.AsType[*json.SyntaxError](err); syntax {
			reject(w, http.StatusBadRequest, "Failed to parse the request body as JSON: "+err.Error())
		} else {
			reject(w, http.StatusUnprocessableEntity, "Failed to deserialize the JSON body into the target type: "+err.Error())
		}
		return false
	}
	if rules, ok := dst.(fieldRules); ok {
		var fields fieldSet
		err := json.Unmarshal(body, &fields)
		if err == nil {
			err = rules.checkFields(fields)
		}
		if err != nil {
			reject(w, http.StatusUnprocessableEntity, "Failed to deserialize the JSON body into the target type: "+err.Error())
			return false
		}
	}
	return true
}

func readBody(w http.ResponseWriter, r *http.Request, limit int64) ([]byte, error) {
	if r.ContentLength > limit {
		return nil, &http.MaxBytesError{Limit: limit}
	}
	size := int64(0)
	if r.ContentLength > 0 {
		size = r.ContentLength
	}
	buf := bytes.NewBuffer(make([]byte, 0, size+bytes.MinRead))
	_, err := buf.ReadFrom(http.MaxBytesReader(w, r.Body, limit))
	return buf.Bytes(), err
}

// isJSONContentType accepts application/json and application/*+json.
func isJSONContentType(value string) bool {
	if value == "" {
		return false
	}
	mediaType, _, err := mime.ParseMediaType(value)
	if err != nil {
		return false
	}
	typ, sub, _ := strings.Cut(mediaType, "/")
	return typ == "application" && (sub == "json" || strings.HasSuffix(sub, "+json"))
}
