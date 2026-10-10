package gen

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"mime/multipart"
	"net/http"
	"net/textproto"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/google/uuid"

	"imageclient/internal/config"
	"imageclient/internal/media"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

// MaxImageBatch is the most images one request may ask for; the gateway
// supports four, so larger counts are clamped rather than forwarded.
const MaxImageBatch = 4

const (
	// Per requested image: four inline 4K PNGs exceed a flat 100 MiB cap.
	maxImageResponseBytes = 100 << 20
	// Byte counts are reported at most this often.
	progressInterval = 250 * time.Millisecond
	grokFallbackStep = "网关不接受 Grok 参数，已改用通用参数重新提交"
)

var (
	// Generation can take minutes before the gateway answers; once it does,
	// only silence ends the body read, so a slow link still delivers paid
	// results. Total is the absolute ceiling.
	defaultImageTimeouts = platform.Timeouts{Header: 240 * time.Second, Idle: 60 * time.Second, Total: 30 * time.Minute}
	// data[].url results are plain GETs, so unlike the paid POST they are
	// retried, resuming from the bytes already received.
	defaultImageDownload = platform.DownloadPolicy{
		Attempts:   4,
		Timeouts:   platform.Timeouts{Header: 60 * time.Second, Idle: 60 * time.Second, Total: 15 * time.Minute},
		RetryDelay: time.Second,
		Limit:      50 << 20,
		Label:      "图片",
	}
	errImageNotConfigured = errors.New("未配置图像 API（请在设置中填写图像地址与 Key）")
)

// ImageRequest is one image generation.
type ImageRequest struct {
	Prompt string
	Model  string // empty: the configured image model
	// Size is the UI selection, e.g. "1024x1536 (2:3)" or "2048x2048 (2K)";
	// the annotation picks Grok's aspect ratio and resolution. When empty,
	// Width x Height is used, each defaulting to 1024.
	Size          string
	Width, Height int
	Quality       string // low | medium | high
	Background    string // transparent | opaque | auto
	Count         int    // images to generate, clamped to 1..MaxImageBatch
	// References switch to image-to-image. ReferencePath is the legacy single
	// reference, used only when References is empty.
	References    []ImageReference
	ReferencePath string
}

// ImageReference is a reference image with the job it does for the prompt.
type ImageReference struct {
	Path      string   `json:"path"`
	Role      string   `json:"role"`
	Weight    *float64 `json:"weight,omitempty"`    // (0, 1]; default 1
	SortOrder *int64   `json:"sortOrder,omitempty"` // default: position
}

// ImageProgress is one generation step, shown on the generate button and in
// its run log. Stage is waiting, receiving, downloading, retrying, partial or
// saving.
type ImageProgress struct {
	Stage         string `json:"stage"`
	Message       string `json:"message"`
	ReceivedBytes *int64 `json:"receivedBytes,omitempty"`
	TotalBytes    *int64 `json:"totalBytes,omitempty"`
	// 1-based position of a data[].url result while it downloads.
	Index *int `json:"index,omitempty"`
	Count *int `json:"count,omitempty"`
}

// imageRequestFromConfig reads a run_node config.
func imageRequestFromConfig(node nodeConfig) (ImageRequest, error) {
	prompt, ok := node.str("prompt")
	if !ok {
		return ImageRequest{}, errors.New("缺少提示词 (prompt)")
	}
	if strings.TrimSpace(prompt) == "" {
		return ImageRequest{}, errors.New("提示词为空")
	}
	req := ImageRequest{Prompt: prompt}
	req.Model, _ = node.str("model")
	req.Size, _ = node.str("size")
	req.Width, _ = node.u32("width")
	req.Height, _ = node.u32("height")
	req.Quality, _ = node.str("quality")
	req.Background, _ = node.str("background")
	req.Count, _ = node.u32("n")
	req.ReferencePath, _ = node.str("referencePath")
	refs, err := referencesFromConfig(node["references"])
	if err != nil {
		return ImageRequest{}, err
	}
	req.References = refs
	return req, nil
}

func referencesFromConfig(raw json.RawMessage) ([]ImageReference, error) {
	invalid := errors.New("references 必须是参考图对象数组")
	if len(raw) == 0 || string(raw) == "null" {
		return nil, nil
	}
	if raw[0] != '[' {
		return nil, invalid
	}
	var items []struct {
		Path      *string  `json:"path"`
		Role      *string  `json:"role"`
		Weight    *float64 `json:"weight"`
		SortOrder *int64   `json:"sortOrder"`
	}
	if json.Unmarshal(raw, &items) != nil {
		return nil, invalid
	}
	refs := make([]ImageReference, 0, len(items))
	for _, item := range items {
		if item.Path == nil || item.Role == nil {
			return nil, invalid
		}
		refs = append(refs, ImageReference{Path: *item.Path, Role: *item.Role, Weight: item.Weight, SortOrder: item.SortOrder})
	}
	return refs, nil
}

func (r ImageRequest) dimensions() (int, int) {
	w, h := r.Width, r.Height
	if w == 0 {
		w = 1024
	}
	if h == 0 {
		h = 1024
	}
	return w, h
}

// sizeSelection is the raw selection, annotation included.
func (r ImageRequest) sizeSelection() string {
	if strings.TrimSpace(r.Size) != "" {
		return r.Size
	}
	w, h := r.dimensions()
	return fmt.Sprintf("%dx%d", w, h)
}

// sizeParam is the OpenAI "WxH" size.
func (r ImageRequest) sizeParam() string {
	if base := normalizeSize(r.sizeSelection()); base != "" {
		return base
	}
	w, h := r.dimensions()
	return fmt.Sprintf("%dx%d", w, h)
}

// normalizeSize keeps the "WxH" part of "1024x1024 (1:1)".
func normalizeSize(selection string) string {
	base, _, _ := strings.Cut(selection, "(")
	base = strings.TrimSpace(base)
	if strings.Contains(base, "x") {
		return base
	}
	return ""
}

func clampImageCount(n int) int { return min(max(n, 1), MaxImageBatch) }

// isGrokImageModel reports whether model is served by the Grok adapter,
// whose contract differs from OpenAI's. Matching is per segment, so
// "xai/grok-2-image" matches and "grokking-image" does not.
func isGrokImageModel(model string) bool {
	segments := strings.FieldsFunc(strings.ToLower(strings.TrimSpace(model)), func(r rune) bool {
		return r == '/' || r == ':' || r == '@'
	})
	for _, segment := range segments {
		rest, ok := strings.CutPrefix(segment, "grok")
		if ok && (rest == "" || rest[0] == '-' || rest[0] == '_' || rest[0] >= '0' && rest[0] <= '9') {
			return true
		}
	}
	return false
}

// Ratios the Grok image endpoint documents.
var grokAspectRatios = []struct {
	label string
	value float64
}{
	{"1:1", 1}, {"3:4", 3.0 / 4}, {"4:3", 4.0 / 3}, {"9:16", 9.0 / 16}, {"16:9", 16.0 / 9},
	{"2:3", 2.0 / 3}, {"3:2", 3.0 / 2}, {"9:19.5", 9 / 19.5}, {"19.5:9", 19.5 / 9},
	{"9:20", 9.0 / 20}, {"20:9", 20.0 / 9}, {"1:2", 1.0 / 2}, {"2:1", 2},
}

// grokAspectRatio prefers the selection's "(2:3)" annotation and otherwise
// snaps the pixel size to the closest documented ratio.
func grokAspectRatio(selection string) string {
	if _, rest, ok := strings.Cut(selection, "("); ok {
		if inner, _, ok := strings.Cut(rest, ")"); ok {
			annotation := strings.ToLower(strings.TrimSpace(inner))
			for _, r := range grokAspectRatios {
				if r.label == annotation {
					return r.label
				}
			}
		}
	}
	w, h, ok := pixelSize(selection)
	if !ok {
		return "1:1"
	}
	ratio := float64(w) / float64(h)
	best := grokAspectRatios[0]
	for _, r := range grokAspectRatios[1:] {
		if math.Abs(r.value-ratio) < math.Abs(best.value-ratio) {
			best = r
		}
	}
	return best.label
}

// grokResolution is "1k" or "2k"; larger selections collapse onto 2k.
func grokResolution(selection string) string {
	hinted := strings.ToLower(selection)
	if strings.Contains(hinted, "2k") || strings.Contains(hinted, "4k") || strings.Contains(hinted, "8k") {
		return "2k"
	}
	if w, h, ok := pixelSize(selection); ok && max(w, h) >= 2000 {
		return "2k"
	}
	return "1k"
}

// pixelSize parses the leading "1024x1536" (or "1024×1536") of a selection.
func pixelSize(selection string) (int, int, bool) {
	base, _, _ := strings.Cut(selection, "(")
	base = strings.NewReplacer("×", "x", "X", "x").Replace(strings.TrimSpace(base))
	ws, hs, ok := strings.Cut(base, "x")
	if !ok {
		return 0, 0, false
	}
	w, err1 := strconv.ParseUint(strings.TrimSpace(ws), 10, 32)
	h, err2 := strconv.ParseUint(strings.TrimSpace(hs), 10, 32)
	if err1 != nil || err2 != nil || w == 0 || h == 0 {
		return 0, 0, false
	}
	return int(w), int(h), true
}

// imageBody is the JSON body for POST /images/generations. Grok takes
// aspect_ratio + resolution and rejects size/background.
func imageBody(model, prompt, size, selection, quality, background string, count int) map[string]any {
	if isGrokImageModel(model) {
		body := map[string]any{
			"model": model, "prompt": prompt, "n": count,
			"aspect_ratio":    grokAspectRatio(selection),
			"resolution":      grokResolution(selection),
			"response_format": "b64_json",
		}
		// Grok accepts low/medium/high only; anything else is dropped.
		if quality == "low" || quality == "medium" || quality == "high" {
			body["quality"] = quality
		}
		return body
	}
	return openAIImageBody(model, prompt, size, quality, background, count)
}

// openAIImageBody is the OpenAI-compatible body, also the fallback when a
// gateway rejects the Grok contract.
func openAIImageBody(model, prompt, size, quality, background string, count int) map[string]any {
	body := map[string]any{"model": model, "prompt": prompt, "n": count}
	if size != "" {
		body["size"] = size
	}
	if quality != "" {
		body["quality"] = quality
	}
	if background != "" && background != "auto" {
		body["background"] = background
	}
	return body
}

// editsEndpoint turns ".../images/generations" into ".../images/edits".
func editsEndpoint(apiURL string) (string, error) {
	u, err := url.Parse(apiURL)
	if err == nil && (u.Scheme == "" || u.Host == "") {
		err = errors.New("缺少协议或主机")
	}
	if err != nil {
		return "", fmt.Errorf("图像 API 地址无效: %v", err)
	}
	prefix, ok := strings.CutSuffix(u.Path, "/generations")
	if !ok {
		return "", errors.New("图生图要求图像 API 地址以 /generations 结尾")
	}
	u.Path, u.RawPath = prefix+"/edits", ""
	return u.String(), nil
}

// progressSink receives generation steps; nil drops them.
type progressSink func(ImageProgress)

func (p progressSink) report(ev ImageProgress) {
	if p != nil {
		p(ev)
	}
}

func (p progressSink) step(stage, message string) {
	p.report(ImageProgress{Stage: stage, Message: message})
}

// transfer announces ev, then returns the per-chunk callback that repeats it
// with the bytes received so far, at most every progressInterval.
func (p progressSink) transfer(ev ImageProgress) func(int64) {
	p.report(ev)
	if p == nil {
		return nil
	}
	last := time.Now()
	return func(received int64) {
		if time.Since(last) < progressInterval {
			return
		}
		last = time.Now()
		next := ev
		next.ReceivedBytes = &received
		p(next)
	}
}

// GenerateImage generates images into outDir: text-to-image as a JSON POST
// to the configured endpoint, image-to-image as a multipart POST to its
// /edits sibling. onProgress (optional) receives each step.
func (s *Service) GenerateImage(ctx context.Context, req ImageRequest, outDir string, onProgress func(ImageProgress)) ([]store.AssetRef, error) {
	return s.GenerateImageWith(ctx, s.cfg.Get(), req, outDir, onProgress)
}

// GenerateImageWith is GenerateImage with an explicit settings snapshot, for
// queued work that must keep the endpoint and key it was accepted with.
func (s *Service) GenerateImageWith(ctx context.Context, cfg config.Settings, req ImageRequest, outDir string, onProgress func(ImageProgress)) ([]store.AssetRef, error) {
	if !cfg.ImageReady() {
		return nil, errImageNotConfigured
	}
	if strings.TrimSpace(req.Prompt) == "" {
		return nil, errors.New("提示词为空")
	}
	job := &imageJob{
		s:          s,
		endpoint:   cfg.ImageAPIURL,
		key:        cfg.ImageAPIKey,
		model:      req.Model,
		prompt:     req.Prompt,
		size:       req.sizeParam(),
		selection:  req.sizeSelection(),
		quality:    req.Quality,
		background: req.Background,
		count:      clampImageCount(req.Count),
		outDir:     outDir,
		progress:   onProgress,
		requestID:  uuid.NewString(),
		started:    time.Now(),
	}
	if job.model == "" {
		job.model = cfg.ImageModel
	}
	job.grok = isGrokImageModel(job.model)
	refs, err := s.loadReferences(req, cfg, defaultReferenceLimits)
	if err != nil {
		return nil, err
	}
	job.refs = refs
	job.logStart()
	if len(refs) > 0 {
		return job.imageToImage(ctx)
	}
	return job.textToImage(ctx)
}

type imageJob struct {
	s                                                   *Service
	endpoint, key                                       string
	model, prompt, size, selection, quality, background string
	count                                               int
	grok                                                bool
	refs                                                []reference
	outDir                                              string
	progress                                            progressSink
	requestID                                           string
	started                                             time.Time
}

func (j *imageJob) logStart() {
	mode := "text_to_image"
	if len(j.refs) > 0 {
		mode = "image_to_image"
	}
	roles := make([]string, len(j.refs))
	for i, ref := range j.refs {
		roles[i] = ref.role
	}
	var aspectRatio, resolution any
	if j.grok {
		aspectRatio, resolution = grokAspectRatio(j.selection), grokResolution(j.selection)
	}
	slog.Info("image.request.start", "requestId", j.requestID, "model", j.model,
		"endpoint", platform.SafeURL(j.endpoint), "mode", mode, "size", j.size, "quality", j.quality,
		"background", j.background, "count", j.count, "aspectRatio", aspectRatio, "resolution", resolution,
		"promptStats", platform.TextStats(j.prompt), "hasReference", len(j.refs) > 0,
		"referenceCount", len(j.refs), "referenceRoles", roles)
}

func (j *imageJob) textToImage(ctx context.Context) ([]store.AssetRef, error) {
	body, err := encodeJSON(imageBody(j.model, j.prompt, j.size, j.selection, j.quality, j.background, j.count))
	if err != nil {
		return nil, err
	}
	j.progress.step("waiting", "提交文生图请求，等待模型生成")
	resp, err := j.post(ctx, j.endpoint, "application/json", body, "请求图像接口失败")
	if err != nil {
		return nil, err
	}
	if j.grok && isContractRejection(resp.StatusCode) {
		// A gateway may serve Grok models through an OpenAI-only adapter. The
		// rejected request produced no image, so one OpenAI retry costs nothing.
		slog.Warn("image.request.grok_contract_fallback", "requestId", j.requestID, "status", resp.StatusCode, "mode", "text_to_image")
		drain(resp)
		body, err = encodeJSON(openAIImageBody(j.model, j.prompt, j.size, j.quality, j.background, j.count))
		if err != nil {
			return nil, err
		}
		j.progress.step("waiting", grokFallbackStep)
		if resp, err = j.post(ctx, j.endpoint, "application/json", body, "请求图像接口失败"); err != nil {
			return nil, err
		}
	}
	return j.finish(j.collect(ctx, resp))
}

func (j *imageJob) imageToImage(ctx context.Context) ([]store.AssetRef, error) {
	upload, err := referenceUpload(j.refs)
	if err != nil {
		return nil, err
	}
	prompt := promptWithReferenceRoles(j.prompt, j.refs)
	edits, err := editsEndpoint(j.endpoint)
	if err != nil {
		return nil, err
	}
	j.progress.step("waiting", fmt.Sprintf("提交图生图请求（%d 张参考图），等待模型生成", len(j.refs)))
	body, contentType := j.editForm(upload, prompt, j.grok)
	resp, err := j.post(ctx, edits, contentType, body, "请求图像编辑接口失败")
	if err != nil {
		return nil, err
	}
	if j.grok && isContractRejection(resp.StatusCode) {
		slog.Warn("image.request.grok_contract_fallback", "requestId", j.requestID, "status", resp.StatusCode, "mode", "image_to_image")
		drain(resp)
		j.progress.step("waiting", grokFallbackStep)
		body, contentType = j.editForm(upload, prompt, false)
		if resp, err = j.post(ctx, edits, contentType, body, "请求图像编辑接口失败"); err != nil {
			return nil, err
		}
	}
	return j.finish(j.collect(ctx, resp))
}

// editForm builds the multipart body. The Grok contract takes aspect_ratio,
// resolution and inline data instead of size and background.
func (j *imageJob) editForm(upload upload, prompt string, grokContract bool) ([]byte, string) {
	var buf bytes.Buffer
	w := multipart.NewWriter(&buf)
	w.WriteField("model", j.model)
	w.WriteField("prompt", prompt)
	w.WriteField("n", strconv.Itoa(j.count))
	header := textproto.MIMEHeader{}
	header.Set("Content-Disposition", fmt.Sprintf(`form-data; name="image"; filename=%q`, upload.fileName))
	header.Set("Content-Type", upload.mime())
	part, _ := w.CreatePart(header)
	part.Write(upload.data)
	if grokContract {
		w.WriteField("aspect_ratio", grokAspectRatio(j.selection))
		w.WriteField("resolution", grokResolution(j.selection))
		w.WriteField("response_format", "b64_json")
	} else if j.size != "" {
		w.WriteField("size", j.size)
	}
	if j.quality != "" {
		w.WriteField("quality", j.quality)
	}
	if !grokContract && j.background != "" && j.background != "auto" {
		w.WriteField("background", j.background)
	}
	w.Close()
	return buf.Bytes(), w.FormDataContentType()
}

// isContractRejection: Grok answers an unexpected parameter list with 400 or
// 422, and so does a gateway that proxies Grok behind an OpenAI adapter.
func isContractRejection(status int) bool { return status == 400 || status == 422 }

// post sends one authenticated generation request. Only the wait for
// response headers is bounded here; the body is read under the idle timeout.
func (j *imageJob) post(ctx context.Context, endpoint, contentType string, body []byte, failure string) (*http.Response, error) {
	sent := time.Now()
	req, err := http.NewRequest(http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return nil, j.postFailed(failure, err)
	}
	req.Header.Set("Authorization", "Bearer "+j.key)
	req.Header.Set("Content-Type", contentType)
	resp, err := platform.Do(ctx, j.s.clientFor(j.endpoint), req, j.s.imageTimeouts)
	if err != nil {
		var timeout *platform.TimeoutError
		if errors.As(err, &timeout) {
			// "等待响应超过 240 秒" names the service, as users have always seen it.
			err = errors.New(strings.Replace(timeout.Error(), "等待响应", "等待图像接口响应", 1))
		}
		return nil, j.postFailed(failure, err)
	}
	slog.Debug("image.response.headers", "requestId", j.requestID, "durationMs", time.Since(sent).Milliseconds(), "status", resp.StatusCode)
	return resp, nil
}

func (j *imageJob) postFailed(failure string, err error) error {
	slog.Error("image.request.end", "requestId", j.requestID, "status", "error",
		"durationMs", time.Since(j.started).Milliseconds(), "error", platform.ErrorText(err))
	return fmt.Errorf("%s: %v", failure, err)
}

func (j *imageJob) finish(assets []store.AssetRef, err error) ([]store.AssetRef, error) {
	if err != nil {
		slog.Error("image.request.end", "requestId", j.requestID, "status", "error",
			"durationMs", time.Since(j.started).Milliseconds(), "error", platform.ErrorText(err))
		return nil, err
	}
	var total int64
	for _, asset := range assets {
		if info, err := os.Stat(asset.Path); err == nil {
			total += info.Size()
		}
	}
	slog.Info("image.request.end", "requestId", j.requestID, "status", "success",
		"durationMs", time.Since(j.started).Milliseconds(), "assetCount", len(assets), "totalBytes", total)
	return assets, nil
}

// collect reads the generation response and saves every usable image.
func (j *imageJob) collect(ctx context.Context, resp *http.Response) ([]store.AssetRef, error) {
	if !isSuccess(resp) {
		text := readErrorText(resp)
		return nil, fmt.Errorf("图像接口返回 %s: %s", statusLine(resp.StatusCode), truncateRunes(text, 400))
	}
	onBytes := j.progress.transfer(ImageProgress{Stage: "receiving", Message: "模型已返回，正在接收结果", TotalBytes: contentLength(resp)})
	raw, err := readBody(resp, int64(maxImageResponseBytes)*int64(j.count), "图像接口响应", onBytes)
	if err != nil {
		return nil, err
	}
	var payload any
	if err := json.Unmarshal(raw, &payload); err != nil {
		return nil, fmt.Errorf("解析图像响应失败: %v", err)
	}
	object, _ := payload.(map[string]any)
	entries, _ := object["data"].([]any)
	if len(entries) == 0 {
		return nil, errors.New("图像响应缺少 data")
	}
	entries = entries[:min(len(entries), MaxImageBatch)]

	// Every entry is already paid for, so one bad entry (moderated, expired
	// URL, unknown format) must not discard the others: decode them all
	// first and fail only when none is usable.
	type payloadImage struct {
		data   []byte
		format string
	}
	var (
		images   []payloadImage
		failures []map[string]any
		firstErr error
	)
	for i, entry := range entries {
		slot := imageSlot{index: i + 1, count: len(entries)}
		data, err := j.decodeEntry(ctx, entry, slot)
		format := ""
		if err == nil {
			if format = media.DetectImageFormat(data); format == "" {
				err = errors.New("图像接口返回的内容不是支持的图片格式")
			}
		}
		if err != nil {
			j.progress.step("partial", fmt.Sprintf("%s不可用：%v", slot, err))
			failures = append(failures, map[string]any{"index": i, "error": err.Error()})
			if firstErr == nil {
				firstErr = err
			}
			continue
		}
		images = append(images, payloadImage{data, format})
	}
	if len(images) == 0 {
		return nil, firstErr
	}
	if len(failures) > 0 {
		slog.Warn("image.response.partial", "saved", len(images), "failed", failures)
	}

	j.progress.step("saving", fmt.Sprintf("保存 %d 张图片", len(images)))
	saved := make([]store.AssetRef, 0, len(images))
	for _, img := range images {
		asset, err := media.SaveBytes(j.outDir, "image", img.data, img.format)
		if err != nil {
			return nil, err
		}
		saved = append(saved, asset)
	}
	return saved, nil
}

// imageSlot is the position of one data[] entry, for messages ("第 2/4 张").
type imageSlot struct{ index, count int }

func (s imageSlot) String() string { return fmt.Sprintf("第 %d/%d 张", s.index, s.count) }

// decodeEntry reads one data[] entry: inline base64 or a URL to download.
func (j *imageJob) decodeEntry(ctx context.Context, entry any, slot imageSlot) ([]byte, error) {
	object, _ := entry.(map[string]any)
	if b64, ok := object["b64_json"].(string); ok {
		if b64 == "" {
			return nil, errors.New("模型未返回图像数据（可能不支持该参数，如透明背景）")
		}
		data, err := base64.StdEncoding.DecodeString(b64)
		if err != nil {
			return nil, fmt.Errorf("base64 解码失败: %v", err)
		}
		return data, nil
	}
	if link, ok := object["url"].(string); ok {
		return j.s.downloadImage(ctx, link, j.progress, slot)
	}
	return nil, errors.New("图像响应中没有 b64_json 或 url")
}

// downloadImage fetches a data[].url result without credentials: the bearer
// key belongs to the gateway, not to whatever host serves the result.
func (s *Service) downloadImage(ctx context.Context, link string, progress progressSink, slot imageSlot) ([]byte, error) {
	policy := s.imageDownload
	index, count := slot.index, slot.count
	announced := 0
	var onBytes func(int64)
	return platform.Download(ctx, s.downloadClient(), link, policy, func(ev platform.DownloadEvent) {
		received := ev.Received
		switch {
		case ev.Retry != nil:
			verb := "重试"
			if received > 0 {
				verb = "续传"
			}
			progress.report(ImageProgress{
				Stage: "retrying",
				Message: fmt.Sprintf("%s下载中断：%v；%d 秒后%s（第 %d/%d 次）", slot, ev.Retry.Err,
					int(ev.Retry.Delay/time.Second), verb, ev.Retry.Next, policy.Attempts),
				ReceivedBytes: &received, Index: &index, Count: &count,
			})
		case ev.Attempt != announced:
			// The first event of an attempt announces its transfer.
			announced = ev.Attempt
			var total *int64
			if ev.Total > 0 {
				total = &ev.Total
			}
			onBytes = progress.transfer(ImageProgress{
				Stage: "downloading", Message: fmt.Sprintf("下载%s图片", slot),
				ReceivedBytes: &received, TotalBytes: total, Index: &index, Count: &count,
			})
		case onBytes != nil:
			onBytes(received)
		}
	})
}
