package restapi

import (
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"net/url"
	"strings"
	"unicode/utf8"

	"imageclient/internal/config"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

const (
	maxTextChars      = 2_000_000 // system prompts and inputs
	maxModelChars     = 200
	maxProjectIDChars = 200
	maxImageBatch     = 4
	maxTextAssetBytes = 20 << 20
	maxReferenceItems = 30
)

type route struct {
	method, path string
	domain, name string // catalogue entry; empty for legacy aliases
	handle       http.HandlerFunc
}

// routes is the versioned API in catalogue order.
func (s *Server) routes() []route {
	return []route{
		{"GET", "/api/v1/health", "system", "health", s.health},
		{"GET", "/api/v1/system/info", "system", "system_info", s.systemInfo},
		{"GET", "/api/v1/system/config", "system", "get_config", s.getConfig},
		{"PUT", "/api/v1/system/config", "system", "update_config", s.saveConfig},
		{"GET", "/api/v1/system/llm", "system", "get_llm_config", s.getLLMConfig},
		{"PUT", "/api/v1/system/llm", "system", "update_llm_config", s.saveLLMConfig},
		{"GET", "/api/v1/system/logs", "system", "logs_info", s.logsInfo},
		{"GET", "/api/v1/system/history-sync", "system", "history_sync_status", s.historySync},
		{"PUT", "/api/v1/system/provider", "system", "set_active_provider", s.setProvider},
		{"GET", "/api/v1/catalog/tools", "catalog", "list_tools", s.tools},
		{"GET", "/api/v1/catalog/models", "catalog", "list_models", s.models},
		{"GET", "/api/v1/catalog/models/image", "catalog", "list_image_models", s.imageModels},
		{"GET", "/api/v1/catalog/models/video", "catalog", "list_video_models", s.videoModels},
		{"GET", "/api/v1/catalog/models/llm", "catalog", "list_llm_models", s.llmModels},
		{"GET", "/api/v1/catalog/providers", "catalog", "list_providers", s.providers},
		{"POST", "/api/v1/text/completions", "text", "create_text_completion", s.textCompletion},
		{"POST", "/api/v1/agents/director/runs", "agents", "run_director", s.runStep(directorStep)},
		{"POST", "/api/v1/agents/writer/runs", "agents", "run_writer", s.runStep(writerStep)},
		{"POST", "/api/v1/agents/storyboard/runs", "agents", "run_storyboard", s.runStep(storyboardStep)},
		{"POST", "/api/v1/agents/consistency/runs", "agents", "run_consistency", s.runStep(consistencyStep)},
		{"POST", "/api/v1/agents/qc/runs", "agents", "run_qc", s.runStep(qcStep)},
		{"POST", "/api/v1/agents/orchestrations", "agents", "run_orchestration", s.orchestrate},
		{"POST", "/api/v1/media/images/generations", "media", "generate_image", s.generateImage},
		{"POST", "/api/v1/media/videos/generations", "media", "generate_video_segments", s.generateVideo},
		{"POST", "/api/v1/assets/text", "assets", "save_text_asset", s.saveTextAsset},
		{"POST", "/api/v1/assets/media", "assets", "save_media_asset", s.saveMediaAsset},
	}
}

// legacyRoutes remain for existing automation; new integrations use /api/v1.
func (s *Server) legacyRoutes() []route {
	return []route{
		{method: "GET", path: "/api/health", handle: s.health},
		{method: "GET", path: "/api/config", handle: s.getConfig},
		{method: "POST", path: "/api/config", handle: s.saveConfig},
		{method: "GET", path: "/api/tools", handle: s.tools},
		{method: "POST", path: "/api/text", handle: s.textCompletion},
		{method: "POST", path: "/api/director", handle: s.runStep(directorStep)},
		{method: "POST", path: "/api/script", handle: s.runStep(writerStep)},
		{method: "POST", path: "/api/storyboard", handle: s.runStep(storyboardStep)},
		{method: "POST", path: "/api/consistency", handle: s.runStep(consistencyStep)},
		{method: "POST", path: "/api/qc_review", handle: s.runStep(qcStep)},
		{method: "POST", path: "/api/image", handle: s.generateImage},
		{method: "POST", path: "/api/video", handle: s.generateVideo},
	}
}

// ---- validation -----------------------------------------------------------

func requiredText(value, field string, max int) error {
	trimmed := strings.TrimSpace(value)
	if trimmed == "" {
		return fmt.Errorf("%s 不能为空", field)
	}
	if utf8.RuneCountInString(trimmed) > max {
		return fmt.Errorf("%s 超过 %d 个字符", field, max)
	}
	return nil
}

// requiredPrompt only rejects empty generation prompts: the provider, not
// this app, decides what length a model accepts.
func requiredPrompt(value, field string) error {
	if strings.TrimSpace(value) == "" {
		return fmt.Errorf("%s 不能为空", field)
	}
	return nil
}

func optionalText(value *string, field string, max int) error {
	if value != nil && utf8.RuneCountInString(*value) > max {
		return fmt.Errorf("%s 超过 %d 个字符", field, max)
	}
	return nil
}

// firstError returns the first failed check.
func firstError(errs ...error) error {
	for _, err := range errs {
		if err != nil {
			return err
		}
	}
	return nil
}

func valueOr(p *string, fallback string) string {
	if p == nil {
		return fallback
	}
	return *p
}

func nonNil[T any](items []T) []T {
	if items == nil {
		return []T{}
	}
	return items
}

var errLLMNotConfigured = errors.New("未配置文本 LLM")

// ---- system ---------------------------------------------------------------

func (s *Server) health(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"status": "ok", "service": "image-client-api", "apiVersion": "v1"})
}

// routeInfo is a catalogue entry; fields in key order of the original output.
type routeInfo struct {
	Domain string `json:"domain"`
	Method string `json:"method"`
	Name   string `json:"name"`
	Path   string `json:"path"`
}

func (s *Server) systemInfo(w http.ResponseWriter, _ *http.Request) {
	var catalog []routeInfo
	for _, rt := range s.routes() {
		catalog = append(catalog, routeInfo{Domain: rt.domain, Method: rt.method, Name: rt.name, Path: rt.path})
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"name":               "image-client",
		"version":            s.version,
		"apiVersion":         "v1",
		"authentication":     "none",
		"basePath":           "/api/v1",
		"legacyBasePath":     "/api",
		"domains":            []string{"system", "catalog", "text", "agents", "media", "assets"},
		"historyPersistence": "sqlite",
		"historySynchronization": map[string]any{
			"mode":          "event_driven",
			"event":         store.HistoryChangedEvent,
			"polling":       false,
			"manualRefresh": true,
		},
		"routes": catalog,
	})
}

func (s *Server) logsInfo(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{
		"directory":             s.paths.Logs(),
		"format":                "jsonl",
		"dailyFiles":            true,
		"maxFileBytesExclusive": 10 << 20,
		"retentionDays":         10,
	})
}

func (s *Server) historySync(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, s.feed.Status())
}

func (s *Server) getConfig(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, s.cfg.Status())
}

// saveConfigBody is a full settings update; every string field must be sent
// (empty keeps the stored value).
type saveConfigBody struct {
	config.SaveRequest
}

func (saveConfigBody) checkFields(f fieldSet) error {
	if err := f.require("imageApiUrl", "imageApiKey", "imageApiModel", "videoApiUrl", "videoApiKey",
		"videoApiModel", "llmApiUrl", "llmApiKey", "llmApiModel", "outputDir"); err != nil {
		return err
	}
	return f.notNull("clearConnections")
}

func (s *Server) saveConfig(w http.ResponseWriter, r *http.Request) {
	var body saveConfigBody
	if !readJSON(w, r, maxJSONBody, &body) {
		return
	}
	status, err := s.cfg.Save(body.SaveRequest, "api")
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, http.StatusOK, status)
}

func (s *Server) getLLMConfig(w http.ResponseWriter, _ *http.Request) {
	cur := s.cfg.Get()
	var endpoint any
	if cur.LLMAPIURL != "" {
		endpoint = cur.LLMAPIURL
	}
	writeJSON(w, http.StatusOK, map[string]any{"ready": cur.LLMReady(), "url": endpoint, "model": cur.LLMModel, "source": cur.Source})
}

type llmConfigBody struct {
	URL   string `json:"url"`
	Key   string `json:"key"`
	Model string `json:"model"`
}

func (llmConfigBody) checkFields(f fieldSet) error { return f.require("url", "key", "model") }

func (b llmConfigBody) validate() error {
	u, err := url.Parse(strings.TrimSpace(b.URL))
	if err != nil {
		return fmt.Errorf("LLM 地址无效: %v", err)
	}
	if (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		return errors.New("LLM 地址必须是 HTTP(S) 地址")
	}
	if strings.TrimSpace(b.Key) == "" || utf8.RuneCountInString(b.Key) > 16_384 {
		return errors.New("LLM Key 不能为空且不得超过 16384 个字符")
	}
	if strings.TrimSpace(b.Model) == "" || utf8.RuneCountInString(b.Model) > maxModelChars {
		return errors.New("LLM 模型不能为空且不得超过 200 个字符")
	}
	return nil
}

func (s *Server) saveLLMConfig(w http.ResponseWriter, r *http.Request) {
	var body llmConfigBody
	if !readJSON(w, r, maxJSONBody, &body) {
		return
	}
	if err := body.validate(); err != nil {
		fail(w, err)
		return
	}
	next := s.cfg.Get()
	next.LLMAPIURL = strings.TrimRight(strings.TrimSpace(body.URL), "/")
	next.LLMAPIKey = strings.TrimSpace(body.Key)
	next.LLMModel = strings.TrimSpace(body.Model)
	next.Source = "api"
	if err := s.cfg.Replace(next); err != nil {
		fail(w, err)
		return
	}
	slog.Info("configuration.llm_updated", "url", platform.SafeURL(next.LLMAPIURL), "model", next.LLMModel)
	writeJSON(w, http.StatusOK, map[string]any{"ready": true, "url": next.LLMAPIURL, "model": next.LLMModel, "source": next.Source})
}

// ---- catalog --------------------------------------------------------------

func (s *Server) imageModels(w http.ResponseWriter, r *http.Request) {
	items, err := s.catalog.ImageModels(r.Context())
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"items": nonNil(items)})
}

func (s *Server) videoModels(w http.ResponseWriter, r *http.Request) {
	items, err := s.catalog.VideoModels(r.Context())
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"items": nonNil(items), "capabilities": s.catalog.VideoModelCapabilities()})
}

func (s *Server) llmModels(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"items": config.LLMModels})
}

func (s *Server) models(w http.ResponseWriter, r *http.Request) {
	image, err := s.catalog.ImageModels(r.Context())
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"image": nonNil(image), "video": config.VideoModels, "llm": config.LLMModels})
}

func (s *Server) providers(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"items": s.catalog.Providers()})
}

type providerBody struct {
	ID string `json:"id"`
}

func (providerBody) checkFields(f fieldSet) error { return f.require("id") }

func (s *Server) setProvider(w http.ResponseWriter, r *http.Request) {
	var body providerBody
	if !readJSON(w, r, maxJSONBody, &body) {
		return
	}
	err := requiredText(body.ID, "id", 100)
	if err == nil {
		err = s.catalog.SetActiveProvider(body.ID)
	}
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"items": s.catalog.Providers()})
}

func (s *Server) tools(w http.ResponseWriter, _ *http.Request) {
	tool := func(name, description, path string, properties map[string]any, required ...string) map[string]any {
		return map[string]any{
			"type": "function",
			"function": map[string]any{
				"name":        name,
				"description": description,
				"endpoint":    map[string]any{"method": "POST", "path": path},
				"parameters":  map[string]any{"type": "object", "properties": properties, "required": required},
			},
		}
	}
	str := map[string]any{"type": "string"}
	arr := map[string]any{"type": "array"}
	input := map[string]any{"input": str}
	writeJSON(w, http.StatusOK, map[string]any{
		"name":       "image-client",
		"apiVersion": "v1",
		"tools": []any{
			tool("director", "统筹导演", "/api/v1/agents/director/runs", input, "input"),
			tool("writer", "生成分场剧本", "/api/v1/agents/writer/runs", input, "input"),
			tool("storyboard", "生成 Markdown 视频分镜", "/api/v1/agents/storyboard/runs", input, "input"),
			tool("consistency", "建立固定锚定与剧情状态锚点", "/api/v1/agents/consistency/runs", input, "input"),
			tool("qc", "执行逐镜与内容质量检查", "/api/v1/agents/qc/runs", input, "input"),
			tool("generate_image", "生成图片", "/api/v1/media/images/generations", map[string]any{
				"prompt": str, "referencePath": str, "model": str, "size": str,
				"n": map[string]any{"type": "integer", "description": "生成数量 1-4"},
			}, "prompt"),
			tool("generate_video", "生成视频分段", "/api/v1/media/videos/generations", map[string]any{
				"prompt": str, "durationS": map[string]any{"type": "integer"}, "images": arr, "videos": arr, "audios": arr, "model": str,
			}, "prompt"),
		},
	})
}
