package restapi

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"net/http"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/google/uuid"

	"imageclient/internal/store"
)

type imageBody struct {
	Prompt        string  `json:"prompt"`
	Size          *string `json:"size"`
	Quality       *string `json:"quality"`
	Background    *string `json:"background"`
	ReferencePath *string `json:"referencePath"`
	Model         *string `json:"model"`
	ProjectID     *string `json:"projectId"`
	N             *uint32 `json:"n"` // images in one request, 1..maxImageBatch
}

func (imageBody) checkFields(f fieldSet) error { return f.require("prompt") }

func (b imageBody) validate() error {
	if err := firstError(
		requiredPrompt(b.Prompt, "prompt"),
		optionalText(b.Size, "size", 64),
		optionalText(b.Quality, "quality", 32),
		optionalText(b.Background, "background", 32),
		optionalText(b.ReferencePath, "referencePath", 4_096),
	); err != nil {
		return err
	}
	if b.N != nil && (*b.N < 1 || *b.N > maxImageBatch) {
		return fmt.Errorf("n 必须是 1 到 %d 之间的整数", maxImageBatch)
	}
	if err := firstError(
		optionalText(b.Model, "model", maxModelChars),
		optionalText(b.ProjectID, "projectId", maxProjectIDChars),
	); err != nil {
		return err
	}
	if b.Quality != nil && !slices.Contains([]string{"low", "medium", "high"}, *b.Quality) {
		return errors.New("quality 必须是 low、medium 或 high")
	}
	if b.Background != nil && !slices.Contains([]string{"auto", "transparent", "opaque"}, *b.Background) {
		return errors.New("background 必须是 auto、transparent 或 opaque")
	}
	return nil
}

func (s *Server) generateImage(w http.ResponseWriter, r *http.Request) {
	var body imageBody
	if !readJSON(w, r, maxJSONBody, &body) {
		return
	}
	if err := body.validate(); err != nil {
		fail(w, err)
		return
	}
	cfg := s.cfg.Get()
	model := valueOr(body.Model, cfg.ImageModel)
	req := ImageRequest{
		Prompt: body.Prompt, Model: model, Size: valueOr(body.Size, ""), Quality: valueOr(body.Quality, ""),
		Background: valueOr(body.Background, ""), ReferencePath: valueOr(body.ReferencePath, ""),
	}
	if body.N != nil {
		req.N = int(*body.N)
	}
	assets, err := s.images.GenerateImage(r.Context(), req, filepath.Join(s.cfg.OutputPath(cfg), "图片", "api"))
	if err != nil {
		fail(w, err)
		return
	}
	source := "API文生图"
	if strings.TrimSpace(req.ReferencePath) != "" {
		source = "API图生图"
	}
	materials := []any{}
	if body.ReferencePath != nil {
		materials = append(materials, map[string]any{"kind": "file", "label": "API参考图", "path": *body.ReferencePath})
	}
	params := map[string]any{
		"prompt":        body.Prompt,
		"referencePath": body.ReferencePath,
		"size":          body.Size,
		"quality":       body.Quality,
		"background":    body.Background,
		"n":             body.N,
		"model":         model,
		"provenance":    store.GeneratedProvenance(body.Prompt, body.Prompt, nil, materials, []any{}),
		"origin":        "rest_api",
	}
	assets = nonNil(assets)
	if !s.record(w, r, "rest_image_generation", assets, store.AssetMeta{Source: source, Model: model, ProjectID: body.ProjectID, Params: params}) {
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"assets": assets})
}

type videoBody struct {
	Prompt      string  `json:"prompt"`
	DurationS   *uint32 `json:"durationS"`
	DurationAlt *uint32 `json:"duration_s"` // accepted alias
	AspectRatio *string `json:"aspectRatio"`
	Resolution  *string `json:"resolution"`
	Model       *string `json:"model"`
	Mode        *string `json:"mode"`
	// Reference media: public HTTPS URLs.
	Images    []string `json:"images"`
	Videos    []string `json:"videos"`
	Audios    []string `json:"audios"`
	ProjectID *string  `json:"projectId"`
}

func (videoBody) checkFields(f fieldSet) error {
	if err := f.require("prompt"); err != nil {
		return err
	}
	if _, ok := f["durationS"]; ok {
		if _, alias := f["duration_s"]; alias {
			return errors.New("duplicate field `durationS`")
		}
	}
	return f.notNull("images", "videos", "audios")
}

func (b *videoBody) validate(checkURL func(string) error) error {
	if err := firstError(
		requiredPrompt(b.Prompt, "prompt"),
		optionalText(b.AspectRatio, "aspectRatio", 32),
		optionalText(b.Resolution, "resolution", 32),
		optionalText(b.Model, "model", maxModelChars),
		optionalText(b.Mode, "mode", 32),
	); err != nil {
		return err
	}
	lists := []struct {
		field  string
		values []string
	}{{"images", b.Images}, {"videos", b.Videos}, {"audios", b.Audios}}
	for _, list := range lists {
		if len(list.values) > maxReferenceItems {
			return fmt.Errorf("%s 最多允许 %d 项", list.field, maxReferenceItems)
		}
		for _, value := range list.values {
			if err := optionalText(&value, list.field, 4_096); err != nil {
				return err
			}
		}
	}
	for _, list := range lists {
		for _, value := range list.values {
			if checkURL(value) != nil {
				return fmt.Errorf("%s必须是公网 HTTPS URL", list.field)
			}
		}
	}
	if err := optionalText(b.ProjectID, "projectId", maxProjectIDChars); err != nil {
		return err
	}
	if d := b.duration(); d != nil && (*d < 1 || *d > 3_600) {
		return errors.New("durationS 必须在 1 到 3600 秒之间")
	}
	return nil
}

func (b *videoBody) duration() *uint32 {
	if b.DurationS != nil {
		return b.DurationS
	}
	return b.DurationAlt
}

func (s *Server) generateVideo(w http.ResponseWriter, r *http.Request) {
	var body videoBody
	if !readJSON(w, r, maxJSONBody, &body) {
		return
	}
	body.Images, body.Videos, body.Audios = nonNil(body.Images), nonNil(body.Videos), nonNil(body.Audios)
	if err := body.validate(s.videos.CheckPublicHTTPSURL); err != nil {
		fail(w, err)
		return
	}
	cfg := s.cfg.Get()
	model := valueOr(body.Model, cfg.VideoModel)
	duration := 5
	if d := body.duration(); d != nil {
		duration = int(*d)
	}
	req := VideoRequest{
		Model: model, Prompt: body.Prompt, DurationS: duration,
		AspectRatio: valueOr(body.AspectRatio, ""), Resolution: valueOr(body.Resolution, ""), Mode: valueOr(body.Mode, ""),
		Images: body.Images, Videos: body.Videos, Audios: body.Audios,
	}
	assets, err := s.videos.GenerateVideo(r.Context(), req, filepath.Join(s.cfg.OutputPath(cfg), "视频", "api"))
	if err != nil {
		fail(w, err)
		return
	}
	assets = nonNil(assets)
	source := "API参考素材视频"
	if len(body.Images)+len(body.Videos)+len(body.Audios) == 0 {
		source = "API文生视频"
	}
	materials := []any{}
	for _, list := range []struct {
		label  string
		values []string
	}{{"API视频参考图 URL", body.Images}, {"API视频参考视频 URL", body.Videos}, {"API视频参考音频 URL", body.Audios}} {
		for _, path := range list.values {
			materials = append(materials, map[string]any{"kind": "file", "label": list.label, "path": path})
		}
	}
	taskIDs := []string{}
	for _, asset := range assets {
		if id, ok := strings.CutPrefix(asset.ID, "zzone:"); ok {
			taskIDs = append(taskIDs, id)
		}
	}
	params := map[string]any{
		"prompt":          body.Prompt,
		"duration_s":      duration,
		"aspectRatio":     body.AspectRatio,
		"resolution":      body.Resolution,
		"mode":            body.Mode,
		"images":          body.Images,
		"videos":          body.Videos,
		"audios":          body.Audios,
		"model":           model,
		"providerTaskIds": taskIDs,
		"provenance":      store.GeneratedProvenance(body.Prompt, body.Prompt, nil, materials, []any{}),
		"origin":          "rest_api",
		"workflowStatus":  "direct_api_unreviewed",
	}
	if !s.record(w, r, "rest_video_generation", assets, store.AssetMeta{Source: source, Model: model, ProjectID: body.ProjectID, Params: params}) {
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"assets": assets})
}

type saveTextBody struct {
	Label     string  `json:"label"`
	Text      string  `json:"text"`
	Model     *string `json:"model"`
	ProjectID *string `json:"projectId"`
}

func (saveTextBody) checkFields(f fieldSet) error { return f.require("label", "text") }

func (b saveTextBody) validate() error {
	if err := requiredText(b.Label, "label", 100); err != nil {
		return err
	}
	if strings.ContainsAny(b.Label, `/\`) || b.Label == "." || b.Label == ".." {
		return errors.New("label 不能包含路径分隔符")
	}
	if b.Text == "" || len(b.Text) > maxTextAssetBytes {
		return errors.New("text 必须非空且不超过 20 MiB")
	}
	return firstError(
		optionalText(b.Model, "model", maxModelChars),
		optionalText(b.ProjectID, "projectId", maxProjectIDChars),
	)
}

func (s *Server) saveTextAsset(w http.ResponseWriter, r *http.Request) {
	var body saveTextBody
	if !readJSON(w, r, maxJSONBody, &body) {
		return
	}
	if err := body.validate(); err != nil {
		fail(w, err)
		return
	}
	asset, err := s.files.SaveText(r.Context(), body.Label, body.Text, valueOr(body.Model, ""))
	if err != nil {
		fail(w, err)
		return
	}
	params := map[string]any{
		"text":         body.Text,
		"title":        body.Label,
		"documentType": "document",
		"documentId":   uuid.NewString(),
		"version":      1,
		"changeType":   "generated",
		"provenance":   store.GeneratedProvenance(body.Text, body.Text, nil, []any{}, []any{}),
		"updatedAt":    time.Now().UnixMilli(),
		"origin":       "rest_api",
	}
	if !s.record(w, r, "rest_text_asset_save", []store.AssetRef{asset}, store.AssetMeta{Source: body.Label, Model: body.Model, ProjectID: body.ProjectID, Params: params}) {
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"asset": asset})
}

type saveMediaBody struct {
	Kind       string  `json:"kind"`
	Extension  string  `json:"extension"`
	DataBase64 string  `json:"dataBase64"`
	Label      *string `json:"label"`
	Prompt     *string `json:"prompt"`
	Model      *string `json:"model"`
	ProjectID  *string `json:"projectId"`
}

func (saveMediaBody) checkFields(f fieldSet) error {
	return f.require("kind", "extension", "dataBase64")
}

// decode validates the request and returns the media bytes.
func (b saveMediaBody) decode() ([]byte, error) {
	if b.Kind != "image" && b.Kind != "video" {
		return nil, errors.New("kind 必须是 image 或 video")
	}
	if b.DataBase64 == "" || len(b.DataBase64) > maxMediaBody {
		return nil, errors.New("dataBase64 不能为空且解码后不得超过 512 MiB")
	}
	if err := firstError(
		optionalText(b.Label, "label", 100),
		optionalText(b.Model, "model", maxModelChars),
		optionalText(b.ProjectID, "projectId", maxProjectIDChars),
	); err != nil {
		return nil, err
	}
	// Standard alphabet, canonical padding, no line breaks.
	if i := strings.IndexAny(b.DataBase64, "\r\n"); i >= 0 {
		return nil, fmt.Errorf("无效 base64: illegal base64 data at input byte %d", i)
	}
	data, err := base64.StdEncoding.Strict().DecodeString(b.DataBase64)
	if err != nil {
		return nil, fmt.Errorf("无效 base64: %w", err)
	}
	return data, nil
}

func (s *Server) saveMediaAsset(w http.ResponseWriter, r *http.Request) {
	var body saveMediaBody
	if !readJSON(w, r, maxMediaBody, &body) {
		return
	}
	data, err := body.decode()
	if err != nil {
		fail(w, err)
		return
	}
	body.DataBase64 = "" // release the encoded copy before the file is written
	asset, err := s.files.SaveMedia(r.Context(), body.Kind, body.Extension, data)
	if err != nil {
		fail(w, err)
		return
	}
	source := "API视频资产"
	if body.Kind == "image" {
		source = "API图片资产"
	}
	source = valueOr(body.Label, source)
	prompt := valueOr(body.Prompt, "")
	params := map[string]any{
		"prompt":     body.Prompt,
		"model":      body.Model,
		"provenance": store.GeneratedProvenance(prompt, prompt, nil, []any{}, []any{}),
		"origin":     "rest_api",
	}
	if !s.record(w, r, "rest_media_asset_save", []store.AssetRef{asset}, store.AssetMeta{Source: source, Model: body.Model, ProjectID: body.ProjectID, Params: params}) {
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"asset": asset})
}

// record writes produced assets to the history and tells the window to
// reload it. On failure it answers the request and returns false. The files
// already exist, so a client hanging up now must not keep them out of the
// history.
func (s *Server) record(w http.ResponseWriter, r *http.Request, operation string, assets []store.AssetRef, meta store.AssetMeta) bool {
	if err := s.db.PersistAssets(context.WithoutCancel(r.Context()), assets, meta); err != nil {
		fail(w, err)
		return false
	}
	ids := make([]string, len(assets))
	for i, asset := range assets {
		ids[i] = asset.ID
	}
	s.events.Emit(store.HistoryChangedEvent, s.feed.Publish(operation, ids))
	return true
}
