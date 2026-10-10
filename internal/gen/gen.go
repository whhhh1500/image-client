// Package gen talks to the generation services behind the configured
// gateway: images (OpenAI-compatible /images endpoints), videos (the
// asynchronous /videos task API) and text (chat completions, single prompts
// and tool-calling agents). It also lists the models those services offer.
//
// Produced files land under the configured output directory:
// <output>/图片/<run tag> for images and <output>/视频/<run tag> for videos.
package gen

import (
	"bytes"
	"context"
	"crypto/md5"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"path/filepath"
	"strconv"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"imageclient/internal/config"
	"imageclient/internal/ipc"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

// Deps are the collaborators of the generation service.
type Deps struct {
	Paths  platform.Paths
	Config *config.Store
	Events ipc.Emitter

	// DB resolves run_video's local image references given by asset id.
	// Optional: without it those references are rejected.
	DB *store.DB
	// ComicCatalog lists a project's comic catalog, which resolves local image
	// references given by source URI. Optional, like DB.
	ComicCatalog func(ctx context.Context, projectID string) ([]CatalogEntry, error)
}

// Service generates images, videos and text through the configured services.
type Service struct {
	paths        platform.Paths
	cfg          *config.Store
	events       ipc.Emitter
	db           *store.DB
	comicCatalog func(ctx context.Context, projectID string) ([]CatalogEntry, error)

	// Transport settings. They are fields so tests can shorten them.
	clientFor      func(url string) *http.Client // generation and API requests
	downloadClient func() *http.Client           // unauthenticated result downloads
	imageTimeouts  platform.Timeouts
	imageDownload  platform.DownloadPolicy
	llmTimeouts    platform.Timeouts
	catalogTimeout time.Duration
	videoTiming    videoTiming
}

// New builds the service.
func New(d Deps) *Service {
	events := d.Events
	if events == nil {
		events = ipc.Discard
	}
	return &Service{
		paths:          d.Paths,
		cfg:            d.Config,
		events:         events,
		db:             d.DB,
		comicCatalog:   d.ComicCatalog,
		clientFor:      platform.ClientFor,
		downloadClient: platform.SharedClient,
		imageTimeouts:  defaultImageTimeouts,
		imageDownload:  defaultImageDownload,
		llmTimeouts:    defaultLLMTimeouts,
		catalogTimeout: 20 * time.Second,
		videoTiming:    defaultVideoTiming,
	}
}

// RunNodeRequest is a generation node as the frontend sends it. Config is the
// node's loose settings object (prompt, model, size, references, ...).
type RunNodeRequest struct {
	NodeType    string          `json:"nodeType"`
	Category    string          `json:"category"`
	Config      json.RawMessage `json:"config"`
	InputAssets []AssetIn       `json:"inputAssets"`
}

// AssetIn is an input asset of a node; generation reads its inputs from
// Config instead, so these are accepted and ignored.
type AssetIn struct {
	Path string `json:"path"`
	Kind string `json:"kind"`
}

// RunResult is what run_node and run_video return.
type RunResult struct {
	Assets []store.AssetRef `json:"assets"`
}

// ImageProgressEvent is the event the generate panel listens to.
const ImageProgressEvent = "image://progress"

type imageProgressPayload struct {
	ProgressID string `json:"progressId"`
	ImageProgress
}

// Register adds the generation commands the frontend calls.
func (s *Service) Register(r *ipc.Router) {
	r.Handle("run_node", ipc.Typed(s.runNode))
	r.Handle("run_video", ipc.Typed(s.runVideo))
	r.Handle("llm_chat", ipc.Typed(func(ctx context.Context, in struct {
		System string  `json:"system"`
		User   string  `json:"user"`
		Model  *string `json:"model"`
	}) (string, error) {
		model := ""
		if in.Model != nil {
			model = *in.Model
		}
		return s.LLMChat(ctx, in.System, in.User, model)
	}))
	r.Handle("list_video_models", ipc.Typed(func(ctx context.Context, _ struct{}) ([]string, error) {
		return s.VideoModels(ctx)
	}))
	r.Handle("fetch_models", ipc.Typed(func(ctx context.Context, in struct {
		URL    string `json:"url"`
		APIKey string `json:"apiKey"`
		Kind   string `json:"kind"`
	}) ([]string, error) {
		return s.FetchModels(ctx, in.URL, in.APIKey, in.Kind)
	}))
	r.Handle("list_video_model_capabilities", ipc.Typed(func(context.Context, struct{}) ([]VideoModelCapability, error) {
		return VideoModelCapabilities(), nil
	}))
	r.Handle("list_providers", ipc.Typed(func(context.Context, struct{}) ([]ProviderInfo, error) {
		return Providers(), nil
	}))
}

type runNodeArgs struct {
	Req        RunNodeRequest `json:"req"`
	ProgressID *string        `json:"progressId"`
}

func (s *Service) runNode(ctx context.Context, in runNodeArgs) (RunResult, error) {
	cfg := s.cfg.Get()
	node := parseNodeConfig(in.Req.Config)
	prompt, _ := node.str("prompt")
	dir := filepath.Join(s.cfg.OutputPath(cfg), kindFolder("image"), runTag(prompt, time.Now()))

	var onProgress func(ImageProgress)
	if in.ProgressID != nil {
		id := *in.ProgressID
		onProgress = func(p ImageProgress) {
			s.events.Emit(ImageProgressEvent, imageProgressPayload{ProgressID: id, ImageProgress: p})
		}
	}
	switch in.Req.Category {
	case "generate", "ai_process":
	default:
		return RunResult{}, fmt.Errorf("未知节点类别: %s", in.Req.Category)
	}
	// The connection check comes first, as it always has; only then are the
	// node's own settings judged.
	if !cfg.ImageReady() {
		return RunResult{}, errImageNotConfigured
	}
	req, err := imageRequestFromConfig(node)
	if err != nil {
		return RunResult{}, err
	}
	assets, err := s.GenerateImage(ctx, req, dir, onProgress)
	if err != nil {
		return RunResult{}, err
	}
	return RunResult{Assets: assets}, nil
}

type runVideoArgs struct {
	Req RunNodeRequest `json:"req"`
}

func (s *Service) runVideo(ctx context.Context, in runVideoArgs) (RunResult, error) {
	cfg := s.cfg.Get()
	node := parseNodeConfig(in.Req.Config)
	prompt, _ := node.str("prompt")
	dir := filepath.Join(s.cfg.OutputPath(cfg), kindFolder("video"), runTag(prompt, time.Now()))

	images := node.strings("images")
	sources, err := localImageSources(node)
	if err != nil {
		return RunResult{}, err
	}
	if len(sources) > 0 {
		projectID, _ := node.str("project_id")
		resolved, err := s.resolveLocalImages(ctx, projectID, sources)
		if err != nil {
			return RunResult{}, err
		}
		images = append(images, resolved...)
	}
	req := VideoRequest{Prompt: prompt, Images: images, Videos: node.strings("videos"), Audios: node.strings("audios")}
	req.Model, _ = node.str("model")
	if req.Model == "" {
		req.Model = cfg.VideoModel
	}
	req.DurationS = 5
	if d, ok := node.u32("duration_s"); ok {
		req.DurationS = d
	}
	req.AspectRatio, _ = node.str("aspect_ratio")
	req.Resolution, _ = node.str("resolution")
	req.Mode, _ = node.str("mode")
	if strings.TrimSpace(req.Prompt) == "" {
		return RunResult{}, errors.New("缺少提示词")
	}
	assets, err := s.GenerateVideo(ctx, req, dir)
	if err != nil {
		return RunResult{}, err
	}
	return RunResult{Assets: assets}, nil
}

// kindFolder is the output subfolder for a kind of asset.
func kindFolder(kind string) string {
	if kind == "video" {
		return "视频"
	}
	return "图片"
}

// runTag names one run's folder: local time, the prompt's first 24
// alphanumeric characters with every other run collapsed to "-" ("gen" when
// nothing is left), and the first 6 hex digits of the prompt's MD5.
func runTag(prompt string, now time.Time) string {
	words := strings.FieldsFunc(prompt, func(r rune) bool { return !isAlphanumeric(r) })
	clean := strings.Join(words, "-")
	if clean == "" {
		clean = "gen"
	}
	sum := md5.Sum([]byte(prompt))
	return now.Format("20060102-150405") + "_" + truncateRunes(clean, 24) + "_" + hex.EncodeToString(sum[:])[:6]
}

// isAlphanumeric matches Unicode Alphabetic or Numeric characters.
func isAlphanumeric(r rune) bool {
	return unicode.IsLetter(r) || unicode.IsNumber(r) || unicode.Is(unicode.Other_Alphabetic, r)
}

// ProviderInfo describes a generation provider.
type ProviderInfo struct {
	ID           string   `json:"id"`
	Name         string   `json:"name"`
	Active       bool     `json:"active"`
	Capabilities []string `json:"capabilities"`
}

// GatewayProviderID is the only provider: the configured gateway.
const GatewayProviderID = "zzone"

// Providers lists the generation providers.
func Providers() []ProviderInfo {
	return []ProviderInfo{{
		ID:     GatewayProviderID,
		Name:   "ZZone 网关",
		Active: true,
		Capabilities: []string{
			"text_to_image", "image_to_image", "text_to_video", "image_to_video", "text_completion",
		},
	}}
}

// SetActiveProvider selects a provider; only the gateway exists.
func SetActiveProvider(id string) ([]ProviderInfo, error) {
	if id != GatewayProviderID {
		return nil, fmt.Errorf("未知 provider: %s", id)
	}
	return Providers(), nil
}

// GenerateProviderImage generates through the provider a durable job recorded
// when it was created, so a paid retry cannot switch providers.
func (s *Service) GenerateProviderImage(ctx context.Context, providerID string, req ImageRequest, outDir string) ([]store.AssetRef, error) {
	if providerID != GatewayProviderID {
		return nil, errors.New("VISUAL_PROVIDER_UNAVAILABLE")
	}
	return s.GenerateImage(ctx, req, outDir, nil)
}

// nodeConfig is a node's loose settings object. Lookups only accept values of
// the expected JSON type, so a number where a string belongs reads as absent.
type nodeConfig map[string]json.RawMessage

func parseNodeConfig(raw json.RawMessage) nodeConfig {
	var c nodeConfig
	if json.Unmarshal(raw, &c) != nil || c == nil {
		return nodeConfig{}
	}
	return c
}

func (c nodeConfig) str(key string) (string, bool) {
	return jsonString(c[key])
}

func jsonString(raw json.RawMessage) (string, bool) {
	if len(raw) == 0 || raw[0] != '"' {
		return "", false
	}
	var s string
	if json.Unmarshal(raw, &s) != nil {
		return "", false
	}
	return s, true
}

// u32 reads a non-negative integer, keeping its low 32 bits like the
// original backend did.
func (c nodeConfig) u32(key string) (int, bool) {
	raw := c[key]
	if len(raw) == 0 {
		return 0, false
	}
	v, err := strconv.ParseUint(string(raw), 10, 64)
	if err != nil {
		return 0, false
	}
	return int(uint32(v)), true
}

// strings reads an array, keeping its trimmed non-empty strings.
func (c nodeConfig) strings(key string) []string {
	var items []json.RawMessage
	if json.Unmarshal(c[key], &items) != nil {
		return []string{}
	}
	out := make([]string, 0, len(items))
	for _, item := range items {
		if s, ok := jsonString(item); ok {
			if s = strings.TrimSpace(s); s != "" {
				out = append(out, s)
			}
		}
	}
	return out
}

// statusLine renders a status the way users have always seen it, e.g.
// "422 Unprocessable Entity".
func statusLine(code int) string {
	text := http.StatusText(code)
	if text == "" {
		text = "<unknown status code>"
	}
	return strconv.Itoa(code) + " " + text
}

func isSuccess(resp *http.Response) bool { return resp.StatusCode >= 200 && resp.StatusCode < 300 }

// readErrorText reads an error response of up to 1 MiB; anything unreadable
// or larger yields "" rather than a fragment.
func readErrorText(resp *http.Response) string {
	defer resp.Body.Close()
	body, err := platform.ReadLimited(resp.Body, nil, 1<<20, "错误响应", nil)
	if err != nil {
		return ""
	}
	return strings.ToValidUTF8(string(body), "\uFFFD")
}

// providerErrorText is what users see of an error response. OpenAI-compatible
// gateways wrap the reason in JSON ({"error":{"message":…}}, {"error":"…"} or
// {"message":…}); showing just that reads far better than the raw body,
// which the request logs keep in full. Other bodies are shown as they are.
func providerErrorText(text string) string {
	var body struct {
		Error   json.RawMessage `json:"error"`
		Message string          `json:"message"`
	}
	if json.Unmarshal([]byte(text), &body) == nil {
		var nested struct {
			Message string `json:"message"`
		}
		var plain string
		for _, candidate := range []string{
			func() string { json.Unmarshal(body.Error, &nested); return nested.Message }(),
			func() string { json.Unmarshal(body.Error, &plain); return plain }(),
			body.Message,
		} {
			if candidate = strings.TrimSpace(candidate); candidate != "" {
				return truncateRunes(candidate, 400)
			}
		}
	}
	return truncateRunes(text, 400)
}

// drain discards the rest of a body so the connection can be reused.
func drain(resp *http.Response) {
	io.Copy(io.Discard, io.LimitReader(resp.Body, 1<<20))
	resp.Body.Close()
}

// readBody reads a whole response under limit; an oversized body names the
// limit in its message.
func readBody(resp *http.Response, limit int64, label string, onProgress func(int64)) ([]byte, error) {
	body, err := platform.ReadAllLimited(resp, limit, label, onProgress)
	var tooLarge *platform.TooLargeError
	if errors.As(err, &tooLarge) {
		return nil, fmt.Errorf("%s（%s）", tooLarge.Error(), limitLabel(limit))
	}
	return body, err
}

func limitLabel(limit int64) string {
	if limit >= 1<<20 {
		return fmt.Sprintf("%d MiB", limit>>20)
	}
	return fmt.Sprintf("%d KiB", limit>>10)
}

func truncateRunes(s string, n int) string {
	if utf8.RuneCountInString(s) <= n {
		return s
	}
	return string([]rune(s)[:n])
}

// sleep waits d unless ctx ends first.
func sleep(ctx context.Context, d time.Duration) error {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// contentLength is the declared body size, or nil when unknown.
func contentLength(resp *http.Response) *int64 {
	if resp.ContentLength < 0 {
		return nil
	}
	n := resp.ContentLength
	return &n
}

// encodeJSON is json.Marshal without HTML escaping, so text reaches the
// services exactly as written.
func encodeJSON(v any) ([]byte, error) {
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// optional maps "" to a JSON null in logs.
func optional(s string) any {
	if s == "" {
		return nil
	}
	return s
}
