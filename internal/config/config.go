// Package config holds the API endpoints, keys and selected models. Keys stay
// in the backend; the frontend only sees URLs, ready flags and model names.
//
// The frontend's SQLite settings table is the editable source and is pushed
// here with save_config. A snapshot is persisted to backend-config.json so the
// REST API works before the window has loaded; .env is the last fallback.
package config

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"

	"github.com/joho/godotenv"

	"imageclient/internal/platform"
)

// DefaultOutputDir stands for the data root's assets directory.
const DefaultOutputDir = "$DEFAULT_ASSETS"

const (
	defaultImageModel = "gpt-image-2"
	defaultVideoModel = "kling-video-v3"
	defaultLLMModel   = "gemini-3.7-flash"
)

// Settings is one consistent configuration. The JSON names are the on-disk
// format of backend-config.json and must not change.
type Settings struct {
	ImageAPIURL string `json:"image_api_url"`
	ImageAPIKey string `json:"image_api_key"`
	ImageModel  string `json:"image_model"`
	VideoAPIURL string `json:"video_api_url"`
	VideoAPIKey string `json:"video_api_key"`
	VideoModel  string `json:"video_model"`
	LLMAPIURL   string `json:"llm_api_url"`
	LLMAPIKey   string `json:"llm_api_key"`
	LLMModel    string `json:"llm_model"`
	OutputDir   string `json:"output_dir"`
	Source      string `json:"source"` // "db" | "env" | "api" | "none"
}

// Status is what the frontend may see.
type Status struct {
	ImageReady  bool    `json:"imageReady"`
	VideoReady  bool    `json:"videoReady"`
	LLMReady    bool    `json:"llmReady"`
	ImageAPIURL *string `json:"imageApiUrl"`
	VideoAPIURL *string `json:"videoApiUrl"`
	ImageModel  string  `json:"imageModel"`
	VideoModel  string  `json:"videoModel"`
	LLMModel    string  `json:"llmModel"`
	OutputDir   string  `json:"outputDir"`
	Source      string  `json:"source"`
}

func (s Settings) ImageReady() bool { return s.ImageAPIURL != "" && s.ImageAPIKey != "" }
func (s Settings) VideoReady() bool { return s.VideoAPIURL != "" && s.VideoAPIKey != "" }
func (s Settings) LLMReady() bool   { return s.LLMAPIURL != "" && s.LLMAPIKey != "" }

// Store guards the live settings; readers take a copy.
type Store struct {
	paths platform.Paths
	mu    sync.RWMutex
	cur   Settings
}

// Load starts from the persisted snapshot, falling back to .env.
func Load(paths platform.Paths) *Store {
	s := &Store{paths: paths}
	s.cur = s.initial()
	return s
}

// Get returns the current settings.
func (s *Store) Get() Settings {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.cur
}

// Replace persists next, then installs it; a failed write changes nothing.
func (s *Store) Replace(next Settings) error {
	next.OutputDir = s.normalizeOutput(next.OutputDir)
	if err := s.persist(next); err != nil {
		return err
	}
	s.mu.Lock()
	s.cur = next
	s.mu.Unlock()
	return nil
}

// Status describes s for the frontend.
func (s *Store) Status() Status {
	cur := s.Get()
	optional := func(v string) *string {
		if v == "" {
			return nil
		}
		return &v
	}
	return Status{
		ImageReady: cur.ImageReady(), VideoReady: cur.VideoReady(), LLMReady: cur.LLMReady(),
		ImageAPIURL: optional(cur.ImageAPIURL), VideoAPIURL: optional(cur.VideoAPIURL),
		ImageModel: cur.ImageModel, VideoModel: cur.VideoModel, LLMModel: cur.LLMModel,
		OutputDir: s.OutputPath(cur), Source: cur.Source,
	}
}

func (s *Store) initial() Settings {
	path := s.paths.BackendConfig()
	if loaded, err := readSnapshot(path); err == nil {
		loaded.OutputDir = s.normalizeOutput(loaded.OutputDir)
		if loaded.Source == "" {
			loaded.Source = "none"
		}
		return loaded
	} else if !os.IsNotExist(err) {
		slog.Warn("configuration.backend_snapshot_invalid", "path", path, "error", err.Error())
	}
	if envFile := findEnvFile(); envFile != "" {
		_ = godotenv.Load(envFile) // never overrides variables already set
	}
	cfg := Settings{
		ImageAPIURL: env("image-api-url"), ImageAPIKey: env("image-api-key"), ImageModel: orDefault(env("image-api-model"), defaultImageModel),
		VideoAPIURL: env("video-api-url"), VideoAPIKey: env("video-api-key"), VideoModel: orDefault(env("video-api-model"), defaultVideoModel),
		LLMAPIURL: env("llm-api-url"), LLMAPIKey: env("llm-api-key"), LLMModel: orDefault(env("llm-api-model"), defaultLLMModel),
		OutputDir: s.normalizeOutput(env("output-dir")),
		Source:    "env",
	}
	if cfg.ImageAPIURL == "" && cfg.VideoAPIURL == "" {
		cfg.Source = "none"
	}
	return cfg
}

func readSnapshot(path string) (Settings, error) {
	info, err := os.Stat(path)
	if err != nil {
		return Settings{}, err
	}
	if info.Size() > 1<<20 {
		return Settings{}, fmt.Errorf("后端配置文件超过 1 MiB 大小限制")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return Settings{}, fmt.Errorf("读取后端配置失败: %w", err)
	}
	var cfg Settings
	if err := json.Unmarshal(data, &cfg); err != nil {
		return Settings{}, fmt.Errorf("解析后端配置失败: %w", err)
	}
	return cfg, nil
}

func (s *Store) persist(cfg Settings) error {
	data, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return fmt.Errorf("序列化后端配置失败: %w", err)
	}
	if err := platform.WriteFileAtomic(s.paths.BackendConfig(), data); err != nil {
		return fmt.Errorf("提交后端配置失败: %w", err)
	}
	return nil
}

// env reads NAME as given or upper-cased with underscores.
func env(name string) string {
	if v, ok := os.LookupEnv(name); ok {
		return v
	}
	return os.Getenv(strings.ToUpper(strings.ReplaceAll(name, "-", "_")))
}

func orDefault(v, fallback string) string {
	if v == "" {
		return fallback
	}
	return v
}

func findEnvFile() string {
	var candidates []string
	if cwd, err := os.Getwd(); err == nil {
		candidates = append(candidates, filepath.Join(cwd, ".env"), filepath.Join(cwd, "..", ".env"), filepath.Join(cwd, "..", "..", ".env"))
	}
	if exe, err := os.Executable(); err == nil {
		dir := filepath.Dir(exe)
		candidates = append(candidates, filepath.Join(dir, ".env"), filepath.Join(dir, "..", "..", ".env"), filepath.Join(dir, "..", "..", "..", ".env"))
	}
	for _, path := range candidates {
		if _, err := os.Stat(path); err == nil {
			return path
		}
	}
	return ""
}

// normalizeOutput maps empty, default and the assets dir itself to the
// default marker, and drops paths written on another OS (a synced settings
// table can carry a Windows path to macOS and vice versa).
func (s *Store) normalizeOutput(raw string) string {
	value := strings.TrimSpace(raw)
	if value == "" || value == DefaultOutputDir || filepath.Clean(value) == filepath.Clean(s.paths.Assets()) {
		return DefaultOutputDir
	}
	if foreignAbsolute(value, runtime.GOOS == "windows") {
		slog.Warn("configuration.foreign_output_path", "configuredPath", value, "fallback", s.paths.Assets())
		return DefaultOutputDir
	}
	return value
}

func foreignAbsolute(value string, windows bool) bool {
	if windows {
		return strings.HasPrefix(value, "/") && !strings.HasPrefix(value, "//")
	}
	return len(value) >= 3 && isASCIILetter(value[0]) && value[1] == ':' && (value[2] == '\\' || value[2] == '/')
}

func isASCIILetter(b byte) bool { return b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z' }

// OutputPath resolves cfg's output directory: the default marker is the
// assets dir, "~/" expands against the real home and relative paths live
// under the data root.
func (s *Store) OutputPath(cfg Settings) string {
	value := s.normalizeOutput(cfg.OutputDir)
	if value == DefaultOutputDir {
		return s.paths.Assets()
	}
	if rest, ok := strings.CutPrefix(value, "~/"); ok {
		return filepath.Join(userHome(s.paths.Root), rest)
	}
	if rest, ok := strings.CutPrefix(value, `~\`); ok {
		return filepath.Join(userHome(s.paths.Root), rest)
	}
	if filepath.IsAbs(value) {
		return value
	}
	return filepath.Join(s.paths.Root, value)
}

// userHome deliberately ignores IMAGE_CLIENT_DATA_DIR: an isolated data root
// must not redirect "~/" paths.
func userHome(fallback string) string {
	if home, err := os.UserHomeDir(); err == nil {
		return home
	}
	return fallback
}

// ModelsEndpoint derives ".../v1/models" from a configured generation URL.
func ModelsEndpoint(apiURL string) string {
	idx := strings.Index(apiURL, "/v1/")
	if apiURL == "" || idx < 0 {
		return ""
	}
	return apiURL[:idx+len("/v1/")] + "models"
}

// ModelsEndpointFor also accepts a bare base URL the user is still typing:
// "https://host" and "https://host/v1" both become "https://host/v1/models".
func ModelsEndpointFor(apiURL string) string {
	trimmed := strings.TrimSpace(apiURL)
	u, err := url.Parse(trimmed)
	if trimmed == "" || err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		return ""
	}
	if idx := strings.Index(trimmed, "/v1/"); idx >= 0 {
		return trimmed[:idx+len("/v1/")] + "models"
	}
	u.RawQuery, u.Fragment = "", ""
	base := strings.TrimRight(u.String(), "/")
	if strings.HasSuffix(base, "/v1") {
		return base + "/models"
	}
	return base + "/v1/models"
}

// Fallback catalogues, used when a gateway's /v1/models cannot be read.
var (
	ImageModels = []string{
		"gpt-image-2", "gemini-3.1-flash-image", "gemini-2.5-flash-image", "gemini-3-pro-image",
		"gemini-2.5-flash-image-preview", "gemini-3-pro-image-preview", "gemini-3.1-flash-image-preview",
		"grok-imagine-image",
	}
	VideoModels = []string{
		"video-ds-2.5", "video-ds-2.5-480", "minimax-h3", "minimax-h3-2k", "minimax-h3-4k",
		"grok-imagine-video", "grok-imagine-video-1.5-preview", "kling-video-v3", "kling-video-v3-omni",
		"kling-video-v3-turbo", "drama-video-v2", "drama-video-v2-fast", "seedance2.5", "as-sd2.0-fast",
		"video-ds-2.0", "video-ds-2.0-fast", "wan3-720p",
	}
	LLMModels = []string{
		"gemini-3.7-flash", "gemini-3.6-flash", "gemini-3.5-flash", "gemini-3-flash",
		"gemini-2.5-flash", "gemini-2.5-flash-lite", "gemini-3.1-flash-lite", "gemini-2.5-pro",
	}
)
