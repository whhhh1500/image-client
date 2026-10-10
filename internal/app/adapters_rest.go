package app

import (
	"context"
	"errors"

	"imageclient/internal/config"
	"imageclient/internal/gen"
	"imageclient/internal/media"
	"imageclient/internal/restapi"
	"imageclient/internal/store"
)

// Adapters from the REST API's backend interfaces to gen and media.

type restImages struct{ gen *gen.Service }

func (r restImages) GenerateImage(ctx context.Context, req restapi.ImageRequest, dir string) ([]store.AssetRef, error) {
	return r.gen.GenerateImage(ctx, gen.ImageRequest{
		Prompt:        req.Prompt,
		Model:         req.Model,
		Size:          req.Size,
		Quality:       req.Quality,
		Background:    req.Background,
		Count:         req.N,
		ReferencePath: req.ReferencePath,
	}, dir, nil)
}

type restVideos struct{ gen *gen.Service }

func (v restVideos) GenerateVideo(ctx context.Context, req restapi.VideoRequest, dir string) ([]store.AssetRef, error) {
	return v.gen.GenerateVideo(ctx, gen.VideoRequest{
		Model: req.Model, Prompt: req.Prompt, DurationS: req.DurationS,
		AspectRatio: req.AspectRatio, Resolution: req.Resolution, Mode: req.Mode,
		Images: req.Images, Videos: req.Videos, Audios: req.Audios,
	}, dir)
}

func (v restVideos) CheckPublicHTTPSURL(raw string) error {
	return gen.ValidatePublicHTTPSURLs([]string{raw}, "url")
}

type restText struct {
	gen    *gen.Service
	config *config.Store
}

func (t restText) Complete(ctx context.Context, req restapi.TextRequest) (restapi.TextCompletion, error) {
	cfg := t.config.Get()
	endpoint, ok := gen.LLMEndpoint(cfg)
	if !ok {
		return restapi.TextCompletion{}, errors.New("未配置文本 LLM")
	}
	model := req.Model
	if model == "" {
		model = cfg.LLMModel
	}
	result, err := t.gen.CompleteTextResult(ctx, gen.TextRequest{
		Endpoint: endpoint, Model: model, System: req.System, User: req.Input, Operation: req.Operation,
	})
	if err != nil {
		return restapi.TextCompletion{}, err
	}
	return restapi.TextCompletion{Content: result.Content, Completed: result.Completed}, nil
}

func (t restText) RunAgent(ctx context.Context, req restapi.AgentRequest) (string, error) {
	tools := make([]gen.AgentTool, len(req.Tools))
	for i, tool := range req.Tools {
		tools[i] = gen.AgentTool{Name: tool.Name, Description: tool.Description, System: tool.System}
	}
	return t.gen.RunAgent(ctx, gen.AgentRequest{System: req.System, User: req.Input, Model: req.Model, Tools: tools})
}

func (restText) StripThinking(text string) string { return gen.StripThinking(text) }

type restCatalog struct{ gen *gen.Service }

func (c restCatalog) ImageModels(ctx context.Context) ([]string, error) {
	return c.gen.ImageModels(ctx), nil
}
func (c restCatalog) VideoModels(ctx context.Context) ([]string, error) {
	return c.gen.VideoModels(ctx)
}
func (restCatalog) VideoModelCapabilities() any { return gen.VideoModelCapabilities() }
func (restCatalog) Providers() any              { return gen.Providers() }

func (restCatalog) SetActiveProvider(id string) error {
	_, err := gen.SetActiveProvider(id)
	return err
}

type restFiles struct{ media *media.Service }

func (f restFiles) SaveText(_ context.Context, label, text, model string) (store.AssetRef, error) {
	return f.media.SaveText(label, text, model)
}

func (f restFiles) SaveMedia(_ context.Context, kind, extension string, data []byte) (store.AssetRef, error) {
	return f.media.SaveMedia(kind, extension, data)
}
