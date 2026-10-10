package app

import (
	"context"

	"imageclient/internal/comic"
	"imageclient/internal/gen"
	"imageclient/internal/media"
	"imageclient/internal/store"
)

// The domain packages declare what they need from each other as small
// interfaces or function fields; these adapters satisfy them with gen and
// comic, so no domain package imports another's internals.

// comicText routes comic's chat requests to the LLM client.
type comicText struct{ gen *gen.Service }

func (t comicText) Complete(ctx context.Context, req comic.TextRequest) (comic.Completion, error) {
	messages := make([]gen.Message, len(req.Messages))
	for i, m := range req.Messages {
		messages[i] = gen.Message{"role": m.Role, "content": m.Content}
	}
	result, err := t.gen.Chat(ctx, gen.ChatRequest{
		Endpoint:  gen.Endpoint{URL: req.Endpoint, Key: req.APIKey},
		Model:     req.Model,
		Messages:  messages,
		Operation: req.Operation,
	})
	if err != nil {
		return comic.Completion{}, err
	}
	return comic.Completion{Content: result.Content, FinishReason: result.FinishReason, Completed: result.Completed}, nil
}

// comicImages renders comic pages with the settings the job was accepted with.
type comicImages struct{ gen *gen.Service }

func (g comicImages) Generate(ctx context.Context, req comic.ImageRequest) ([]store.AssetRef, error) {
	refs := make([]gen.ImageReference, len(req.References))
	for i, r := range req.References {
		weight, order := r.Weight, r.SortOrder
		refs[i] = gen.ImageReference{Path: r.Path, Role: r.Role, Weight: &weight, SortOrder: &order}
	}
	return g.gen.GenerateImageWith(ctx, req.Settings, gen.ImageRequest{
		Prompt:     req.Prompt,
		Size:       req.Size,
		Quality:    req.Quality,
		Count:      1,
		References: refs,
	}, req.OutputDir, nil)
}

// comicCatalog lists a project's comic catalog for the packages that resolve
// "comic-md://" sources.
func (a *App) comicCatalog(ctx context.Context, projectID string) ([]comic.CatalogEntry, error) {
	return comic.CatalogList(ctx, a.db, projectID)
}

func (a *App) mediaComicCatalog(ctx context.Context, projectID string) ([]media.ComicMedia, error) {
	entries, err := a.comicCatalog(ctx, projectID)
	if err != nil {
		return nil, err
	}
	out := make([]media.ComicMedia, len(entries))
	for i, e := range entries {
		out[i] = media.ComicMedia{SourceURI: e.SourceURI, Kind: e.Kind, Path: catalogPath(e)}
	}
	return out, nil
}

func (a *App) genComicCatalog(ctx context.Context, projectID string) ([]gen.CatalogEntry, error) {
	entries, err := a.comicCatalog(ctx, projectID)
	if err != nil {
		return nil, err
	}
	out := make([]gen.CatalogEntry, len(entries))
	for i, e := range entries {
		out[i] = gen.CatalogEntry{SourceURI: e.SourceURI, Kind: e.Kind, Path: catalogPath(e)}
	}
	return out, nil
}

func catalogPath(e comic.CatalogEntry) string {
	if e.Path == nil {
		return ""
	}
	return *e.Path
}
