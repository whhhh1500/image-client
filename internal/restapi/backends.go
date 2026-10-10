package restapi

import (
	"context"

	"imageclient/internal/store"
)

// The API delegates generation, model listing and asset files to the services
// the window uses too. These interfaces name exactly what it needs; the app
// adapts the generation and media services to them.

// ImageGenerator generates images through the configured image gateway.
type ImageGenerator interface {
	// GenerateImage runs what run_node does for a "textToImage" node in the
	// "generate" category, without progress events: text-to-image, or
	// image-to-image when ReferencePath is set. The files are written into
	// dir, which may not exist yet. The image connection check (URL and key
	// configured) is part of the call.
	GenerateImage(ctx context.Context, req ImageRequest, dir string) ([]store.AssetRef, error)
}

// ImageRequest is one image generation. Empty strings and a zero N mean "not
// given": the gateway then applies its defaults (one image).
type ImageRequest struct {
	Prompt        string
	Model         string
	Size          string
	Quality       string // low | medium | high
	Background    string // auto | transparent | opaque
	ReferencePath string // local file used as the base image
	N             int    // 1..4
}

// VideoGenerator generates videos through the configured video gateway.
type VideoGenerator interface {
	// GenerateVideo runs what run_video does for these settings, writing the
	// files into dir, which may not exist yet. It fails when the video
	// connection is not configured or the model's capabilities reject the
	// request. Assets of provider tasks have ids "zzone:<provider task id>".
	GenerateVideo(ctx context.Context, req VideoRequest, dir string) ([]store.AssetRef, error)
	// CheckPublicHTTPSURL fails unless raw is an https URL whose host is
	// public (not loopback, private, link-local or otherwise internal). Only
	// success matters: the API reports its own message.
	CheckPublicHTTPSURL(raw string) error
}

// VideoRequest is one video generation. Empty strings mean "not given".
type VideoRequest struct {
	Model       string
	Prompt      string
	DurationS   int
	AspectRatio string
	Resolution  string
	Mode        string
	Images      []string // public HTTPS URLs
	Videos      []string
	Audios      []string
}

// TextModel is the configured chat-completion LLM.
type TextModel interface {
	// Complete sends one system + user exchange to the configured LLM
	// (<llm_api_url without trailing "/">/chat/completions with llm_api_key),
	// logging under req.Operation. It fails with "文本接口未返回内容" when the
	// answer is empty. The content is returned as received.
	Complete(ctx context.Context, req TextRequest) (TextCompletion, error)
	// RunAgent runs the tool-calling orchestration agent_run performs for the
	// window (the LLM may call each tool, a sub-prompt with the tool's system
	// prompt) and returns the final answer with thinking stripped. The API
	// has already validated the request and the LLM configuration.
	RunAgent(ctx context.Context, req AgentRequest) (string, error)
	// StripThinking removes <think>…</think> blocks (case-insensitive; an
	// unclosed block drops the rest) and trims the result.
	StripThinking(text string) string
}

// TextRequest is one single-turn completion.
type TextRequest struct {
	Model     string
	System    string
	Input     string
	Operation string // log label, e.g. "rest_text_completion"
}

// TextCompletion is a completion's content and whether the provider finished
// it normally; Completed is false when the answer was cut short (for
// example finish_reason "length").
type TextCompletion struct {
	Content   string
	Completed bool
}

// AgentRequest is one orchestration run.
type AgentRequest struct {
	System string
	Input  string
	Model  string
	Tools  []ToolDef
}

// ModelCatalog lists models and providers.
type ModelCatalog interface {
	// ImageModels lists the image gateway's models (/v1/models), falling back
	// to config.ImageModels when the catalogue cannot be read.
	ImageModels(ctx context.Context) ([]string, error)
	// VideoModels is list_video_models: the video gateway's models, the
	// fallback catalogue without a configured endpoint, and an error when the
	// configured gateway's catalogue cannot be read.
	VideoModels(ctx context.Context) ([]string, error)
	// VideoModelCapabilities is what list_video_model_capabilities returns;
	// it is served as JSON unchanged.
	VideoModelCapabilities() any
	// Providers is what list_providers returns; it is served as JSON unchanged.
	Providers() any
	// SetActiveProvider selects a provider. An unknown id fails with
	// "未知 provider: <id>".
	SetActiveProvider(id string) error
}

// AssetFiles stores produced files under the configured output directory.
type AssetFiles interface {
	// SaveText writes text to <output>/剧本/<YYYYMMDD-HHMMSS>/<uuid>.md and
	// returns the asset (kind "text", format "md"). label and model are only
	// logged.
	SaveText(ctx context.Context, label, text, model string) (store.AssetRef, error)
	// SaveMedia validates and stores media bytes as save_media_asset does:
	// kind "image" (png, jpg, jpeg, webp, gif) or "video" (mp4, webm, mov),
	// content checked against the extension, written to
	// <output>/<图片|视频>/合成/<YYYYMMDD-HHMMSS>/<uuid>.<ext>, at most 512 MiB.
	SaveMedia(ctx context.Context, kind, extension string, data []byte) (store.AssetRef, error)
}
