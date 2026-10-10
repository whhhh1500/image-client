package gen

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/google/uuid"
	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/media"
	"imageclient/internal/platform"
	"imageclient/internal/store"
)

const (
	maxVideoBytes = 512 << 20
	// Local images travel inline as data URLs; together they stay bounded.
	maxLocalImages     = 30
	maxLocalImageBytes = 50 << 20
)

// videoTiming paces the task lifecycle. The gateway documents polling after
// 3-5 s and then every 5-10 s.
type videoTiming struct {
	firstPoll    time.Duration
	pollInterval time.Duration
	pollDeadline time.Duration
	backoffUnit  time.Duration // transient failures wait 1..6 units
	create       time.Duration
	poll         time.Duration
}

var defaultVideoTiming = videoTiming{
	firstPoll:    5 * time.Second,
	pollInterval: 7 * time.Second,
	pollDeadline: 30 * time.Minute,
	backoffUnit:  5 * time.Second,
	create:       120 * time.Second,
	poll:         60 * time.Second,
}

// defaultVideoDownload fetches a finished, already billed video: a stalled or
// dropped connection resumes from the bytes received instead of losing them.
var defaultVideoDownload = platform.DownloadPolicy{
	Attempts:   4,
	Timeouts:   platform.Timeouts{Header: 60 * time.Second, Idle: 60 * time.Second, Total: 20 * time.Minute},
	RetryDelay: 5 * time.Second,
	Limit:      maxVideoBytes,
	Label:      "视频",
}

// VideoModelCapability is what the client knows a video model accepts.
type VideoModelCapability struct {
	ID                    string   `json:"id"`
	Label                 string   `json:"label"`
	Modes                 []string `json:"modes"`
	MinDurationS          int      `json:"minDurationS"`
	MaxDurationS          int      `json:"maxDurationS"`
	DurationOptions       []int    `json:"durationOptions"`
	Resolutions           []string `json:"resolutions"`
	AspectRatios          []string `json:"aspectRatios"`
	MaxImages             int      `json:"maxImages"`
	MaxVideos             int      `json:"maxVideos"`
	MaxAudios             int      `json:"maxAudios"`
	MaxTotalReferences    *int     `json:"maxTotalReferences"`
	ReferenceImageCount   *int     `json:"referenceImageCount"`
	MaxReferenceDurationS *int     `json:"maxReferenceDurationS"`
	Note                  string   `json:"note"`
}

func secondsRange(lo, hi int) []int {
	out := make([]int, 0, hi-lo+1)
	for d := lo; d <= hi; d++ {
		out = append(out, d)
	}
	return out
}

func intPtr(v int) *int { return &v }

var commonRatios = []string{"16:9", "9:16", "1:1"}

// VideoModelCapabilities lists the verified video models. A model missing
// here is refused rather than guessed at.
func VideoModelCapabilities() []VideoModelCapability {
	minimax := func(id, label, resolution string) VideoModelCapability {
		return VideoModelCapability{ID: id, Label: label, Modes: []string{"text", "reference"},
			MinDurationS: 5, MaxDurationS: 15, DurationOptions: secondsRange(5, 15), Resolutions: []string{resolution},
			AspectRatios: commonRatios, MaxImages: 5, MaxVideos: 3, MaxAudios: 1,
			Note: "MiniMax H3 多模态视频；最多 5 图、3 视频、1 音频。"}
	}
	kling := func(id, label string, resolutions ...string) VideoModelCapability {
		return VideoModelCapability{ID: id, Label: label, Modes: []string{"text", "first_frame", "reference"},
			MinDurationS: 3, MaxDurationS: 15, DurationOptions: secondsRange(3, 15), Resolutions: resolutions,
			AspectRatios: commonRatios, MaxImages: 9, MaxVideos: 3, MaxAudios: 3,
			Note: "按秒生成；清晰度与时长必须使用模型支持值。"}
	}
	grok := func(id, label string, maxReferenceDuration *int) VideoModelCapability {
		return VideoModelCapability{ID: id, Label: label, Modes: []string{"text", "first_frame", "reference"},
			MinDurationS: 1, MaxDurationS: 15, DurationOptions: secondsRange(1, 15), Resolutions: []string{"480p", "720p", "1080p"},
			AspectRatios: commonRatios, MaxImages: 7, ReferenceImageCount: intPtr(7), MaxReferenceDurationS: maxReferenceDuration,
			Note: "首帧模式 1 图；参考图模式必须恰好 7 图。"}
	}
	drama := func(id, label string, resolutions ...string) VideoModelCapability {
		return VideoModelCapability{ID: id, Label: label, Modes: []string{"text", "reference"},
			MinDurationS: 5, MaxDurationS: 15, DurationOptions: []int{5, 10, 15}, Resolutions: resolutions,
			AspectRatios: []string{"16:9", "9:16", "1:1", "4:3", "3:4", "21:9"}, MaxImages: 9, MaxVideos: 3, MaxAudios: 3,
			MaxTotalReferences: intPtr(12), Note: "支持多模态参考；使用视频或音频参考时必须同时提供图片。"}
	}
	standard := func(id, label string) VideoModelCapability {
		return VideoModelCapability{ID: id, Label: label, Modes: []string{"text", "first_frame"},
			MinDurationS: 1, MaxDurationS: 15, DurationOptions: secondsRange(1, 15), Resolutions: []string{"480p", "720p"},
			AspectRatios: commonRatios, MaxImages: 1, Note: "具体能力以当前 API Key 返回结果为准。"}
	}
	seedance := func(id, label, note string, resolutions ...string) VideoModelCapability {
		return VideoModelCapability{ID: id, Label: label, Modes: []string{"text", "reference"},
			MinDurationS: 4, MaxDurationS: 30, DurationOptions: secondsRange(4, 30), Resolutions: resolutions,
			AspectRatios: commonRatios, MaxImages: 30, MaxVideos: 10, MaxAudios: 10, Note: note}
	}
	return []VideoModelCapability{
		seedance("video-ds-2.5", "即梦 2.5 · 720p", "多模态长镜头；单次可直接生成 4–30 秒。", "720p"),
		seedance("video-ds-2.5-480", "即梦 2.5 · 480p", "多模态快速版本；适合多图、多视频和音频参考。", "480p"),
		minimax("minimax-h3", "MiniMax H3 · 2K", "2K"),
		minimax("minimax-h3-2k", "MiniMax H3 · 2K", "2K"),
		minimax("minimax-h3-4k", "MiniMax H3 · 4K", "4k"),
		kling("kling-video-v3", "可灵 V3", "720p", "1080p", "4k"),
		kling("kling-video-v3-omni", "可灵 V3 Omni", "720p", "1080p", "4k"),
		kling("kling-video-v3-turbo", "可灵 V3 Turbo", "720p", "1080p"),
		grok("grok-imagine-video", "Grok Imagine Video", intPtr(10)),
		grok("grok-imagine-video-1.5-preview", "Grok Imagine Video 1.5 Preview", nil),
		drama("drama-video-v2", "Drama Video V2", "480p", "720p", "1080p"),
		drama("drama-video-v2-fast", "Drama Video V2 Fast", "480p", "720p"),
		standard("video-ds-2.0", "即梦 2.0"),
		standard("video-ds-2.0-fast", "即梦 2.0 Fast"),
		standard("as-sd2.0-fast", "即梦 2.0 Fast"),
		seedance("seedance2.5", "即梦 2.5", "模型标识以当前 API Key 返回的目录为准。", "480p", "720p"),
		seedance("seedance2.5-A", "即梦 2.5 · A 渠道", "网关 A 渠道的即梦 2.5；按秒计费。", "480p", "720p"),
	}
}

func capabilityFor(model string) (VideoModelCapability, error) {
	for _, c := range VideoModelCapabilities() {
		if c.ID == model {
			return c, nil
		}
	}
	return VideoModelCapability{}, fmt.Errorf("当前客户端没有视频模型 %s 的能力定义，请更新客户端或改用已验证模型", model)
}

// VideoRequest is one shot. Empty AspectRatio, Resolution and Mode mean "not
// set": the gateway defaults (16:9, 720p) and a mode inferred from the images.
type VideoRequest struct {
	Model       string
	Prompt      string
	DurationS   int
	AspectRatio string
	Resolution  string
	Mode        string // text | first_frame | reference
	// Images are public HTTPS URLs or verified local image data URLs; videos
	// and audios are public HTTPS URLs.
	Images, Videos, Audios []string
}

func (r VideoRequest) mode() string {
	if strings.TrimSpace(r.Mode) != "" {
		return r.Mode
	}
	if len(r.Images) == 0 {
		return "text"
	}
	return "first_frame"
}

// validate checks the shot against the model's capability. Durations the
// model cannot produce in one go fail: a shot is never split or stitched.
func (r VideoRequest) validate() error {
	c, err := capabilityFor(r.Model)
	if err != nil {
		return err
	}
	mode := r.mode()
	if !slices.Contains(c.Modes, mode) {
		return fmt.Errorf("模型 %s 不支持生成模式 %s", r.Model, mode)
	}
	if err := validateVideoImages(r.Images); err != nil {
		return err
	}
	if err := ValidatePublicHTTPSURLs(r.Videos, "参考视频"); err != nil {
		return err
	}
	if err := ValidatePublicHTTPSURLs(r.Audios, "参考音频"); err != nil {
		return err
	}
	images, videos, audios := len(r.Images), len(r.Videos), len(r.Audios)
	if images > c.MaxImages || videos > c.MaxVideos || audios > c.MaxAudios {
		return fmt.Errorf("参考素材超过模型上限（图片 %d/%d, 视频 %d/%d, 音频 %d/%d）",
			images, c.MaxImages, videos, c.MaxVideos, audios, c.MaxAudios)
	}
	if c.MaxTotalReferences != nil && images+videos+audios > *c.MaxTotalReferences {
		return fmt.Errorf("%s 的参考素材总数最多为 %d 项", c.Label, *c.MaxTotalReferences)
	}
	switch {
	case mode == "text" && images+videos+audios > 0:
		return errors.New("纯文本模式不能携带参考素材，请切换生成模式")
	case mode == "first_frame" && images != 1:
		return errors.New("首帧模式需要且只能提供 1 张参考图片")
	case mode == "reference" && images+videos+audios == 0:
		return errors.New("多素材参考模式至少需要一个参考素材")
	}
	if mode == "reference" && c.ReferenceImageCount != nil && images != *c.ReferenceImageCount {
		return fmt.Errorf("%s 的参考图模式必须恰好提供 %d 张图片", c.Label, *c.ReferenceImageCount)
	}
	if r.Model == "drama-video-v2" && videos+audios > 0 && images == 0 {
		return errors.New("Drama Video 使用视频或音频参考时，必须同时提供至少一张参考图")
	}
	if r.Resolution != "" && len(c.Resolutions) > 0 && !slices.Contains(c.Resolutions, r.Resolution) {
		return fmt.Errorf("%s 不支持分辨率 %s", c.Label, r.Resolution)
	}
	if r.AspectRatio != "" && len(c.AspectRatios) > 0 && !slices.Contains(c.AspectRatios, r.AspectRatio) {
		return fmt.Errorf("%s 不支持画幅 %s", c.Label, r.AspectRatio)
	}
	maxDuration := c.MaxDurationS
	if mode == "reference" && c.MaxReferenceDurationS != nil {
		maxDuration = *c.MaxReferenceDurationS
	}
	return validateDuration(r.DurationS, c.MinDurationS, maxDuration, c.DurationOptions)
}

func validateDuration(duration, lo, hi int, options []int) error {
	if duration < lo || duration > hi {
		return fmt.Errorf("当前模型单镜时长必须为 %d–%d 秒；不会自动拆分 %d 秒镜头", lo, hi, duration)
	}
	if len(options) > 0 && !slices.Contains(options, duration) {
		values := make([]string, len(options))
		for i, o := range options {
			values[i] = strconv.Itoa(o)
		}
		return fmt.Errorf("当前模型不支持 %d 秒，可选值：[%s]；不会自动拆分镜头", duration, strings.Join(values, ", "))
	}
	return nil
}

// ValidatePublicHTTPSURLs requires every value to be an HTTPS URL on a
// public host, so the gateway is never pointed at a private network.
func ValidatePublicHTTPSURLs(values []string, label string) error {
	for _, value := range values {
		if !isPublicHTTPSURL(value) {
			return fmt.Errorf("%s必须是公网 HTTPS URL", label)
		}
	}
	return nil
}

func validateVideoImages(values []string) error {
	var total int
	for _, value := range values {
		if strings.HasPrefix(value, "data:") {
			n, err := localImageDataURLSize(value)
			if err != nil {
				return err
			}
			if total += n; total > maxLocalImageBytes {
				return errors.New("本地图片数据总大小不能超过 50 MiB")
			}
		} else if !isPublicHTTPSURL(value) {
			return errors.New("参考图片必须是公网 HTTPS URL 或有效的本地图片数据")
		}
	}
	return nil
}

// localImageDataURLSize verifies a PNG, JPEG or WebP data URL whose declared
// type matches its content and returns the decoded size.
func localImageDataURLSize(value string) (int, error) {
	invalid := errors.New("本地图片数据格式无效")
	var encoded, expected string
	for _, p := range []struct{ prefix, format string }{
		{"data:image/png;base64,", "png"}, {"data:image/jpeg;base64,", "jpg"}, {"data:image/webp;base64,", "webp"},
	} {
		if rest, ok := strings.CutPrefix(value, p.prefix); ok {
			encoded, expected = rest, p.format
			break
		}
	}
	if expected == "" {
		return 0, invalid
	}
	tooLarge := errors.New("本地图片数据超过 50 MiB 大小限制")
	if encoded == "" || len(encoded) > (maxLocalImageBytes+2)/3*4 {
		return 0, tooLarge
	}
	data, err := base64.StdEncoding.Strict().DecodeString(encoded)
	if err != nil {
		return 0, invalid
	}
	if len(data) > maxLocalImageBytes {
		return 0, tooLarge
	}
	format, _, err := verifiedLocalImage(data)
	if err != nil || format != expected {
		return 0, invalid
	}
	return len(data), nil
}

// isPublicHTTPSURL accepts https URLs without credentials whose host is a
// public domain or a public IPv4 address. IPv4 shorthand ("127.1",
// "0x7f000001") is read the way browsers read it.
func isPublicHTTPSURL(value string) bool {
	u, err := url.Parse(strings.TrimSpace(value))
	if err != nil || u.Scheme != "https" || u.Host == "" {
		return false
	}
	if u.User != nil {
		if _, hasPassword := u.User.Password(); u.User.Username() != "" || hasPassword {
			return false
		}
	}
	if strings.HasPrefix(u.Host, "[") {
		return false // IPv6 literals never pass as public hosts
	}
	host := strings.ToLower(u.Hostname())
	if host == "" {
		return false
	}
	if host == "localhost" || strings.HasSuffix(host, ".localhost") || strings.HasSuffix(host, ".local") ||
		strings.HasSuffix(host, ".internal") || strings.HasSuffix(host, ".lan") {
		return false
	}
	if addr, isIP, ok := parseHostIPv4(host); isIP {
		return ok && isPublicIPv4(addr)
	}
	return strings.Contains(host, ".")
}

// parseHostIPv4 applies the URL standard's IPv4 host parsing: a host whose
// last label is numeric is an IPv4 address in decimal, octal or hex parts,
// or invalid. isIP reports whether the host is meant as an address.
func parseHostIPv4(host string) (addr netip.Addr, isIP, ok bool) {
	parts := strings.Split(host, ".")
	if len(parts) > 1 && parts[len(parts)-1] == "" {
		parts = parts[:len(parts)-1]
	}
	last := parts[len(parts)-1]
	if last == "" {
		return netip.Addr{}, false, false
	}
	if _, numeric := ipv4Number(last); !numeric && strings.Trim(last, "0123456789") != "" {
		return netip.Addr{}, false, false
	}
	if len(parts) > 4 {
		return netip.Addr{}, true, false
	}
	numbers := make([]uint64, len(parts))
	for i, part := range parts {
		n, valid := ipv4Number(part)
		if !valid {
			return netip.Addr{}, true, false
		}
		numbers[i] = n
	}
	for _, n := range numbers[:len(numbers)-1] {
		if n > 255 {
			return netip.Addr{}, true, false
		}
	}
	lastNumber := numbers[len(numbers)-1]
	if lastNumber >= 1<<(8*(5-len(numbers))) {
		return netip.Addr{}, true, false
	}
	ipv4 := lastNumber
	for i, n := range numbers[:len(numbers)-1] {
		ipv4 += n << (8 * (3 - i))
	}
	return netip.AddrFrom4([4]byte{byte(ipv4 >> 24), byte(ipv4 >> 16), byte(ipv4 >> 8), byte(ipv4)}), true, true
}

// ipv4Number parses one IPv4 part: decimal, 0x-prefixed hex or 0-prefixed octal.
func ipv4Number(part string) (uint64, bool) {
	if part == "" {
		return 0, false
	}
	base := 10
	switch {
	case len(part) >= 2 && (part[:2] == "0x" || part[:2] == "0X"):
		part, base = part[2:], 16
	case len(part) >= 2 && part[0] == '0':
		part, base = part[1:], 8
	}
	if part == "" {
		return 0, true
	}
	n, err := strconv.ParseUint(part, base, 64)
	if err != nil {
		var numErr *strconv.NumError
		if errors.As(err, &numErr) && errors.Is(numErr.Err, strconv.ErrRange) {
			return 1 << 32, true // too large for any position
		}
		return 0, false
	}
	return n, true
}

func isPublicIPv4(addr netip.Addr) bool {
	b := addr.As4()
	switch {
	case b[0] == 0, b[0] == 10, b[0] == 127,
		b[0] == 169 && b[1] == 254,
		b[0] == 172 && b[1]&0xf0 == 16,
		b[0] == 192 && b[1] == 168,
		b[0] == 100 && b[1] >= 64 && b[1] <= 127,
		b[0] == 192 && b[1] == 0 && b[2] == 2,
		b[0] == 198 && b[1] == 51 && b[2] == 100,
		b[0] == 203 && b[1] == 0 && b[2] == 113,
		b[0] >= 224 && b[0] <= 239,
		b == [4]byte{255, 255, 255, 255}:
		return false
	}
	return true
}

// videoBody is the POST /videos body. In first-frame mode the image fixes the
// geometry, and several models reject an extra aspect_ratio.
func videoBody(r VideoRequest) map[string]any {
	resolution := r.Resolution
	if resolution == "" {
		resolution = "720p"
	}
	body := map[string]any{
		"model": r.Model, "prompt": r.Prompt, "seconds": strconv.Itoa(r.DurationS), "resolution": resolution,
	}
	if r.mode() != "first_frame" {
		ratio := r.AspectRatio
		if ratio == "" {
			ratio = "16:9"
		}
		body["aspect_ratio"] = ratio
	}
	if len(r.Images) > 0 {
		body["images"] = r.Images
	}
	if len(r.Videos) > 0 {
		body["videos"] = r.Videos
	}
	if len(r.Audios) > 0 {
		body["audios"] = r.Audios
	}
	if strings.HasPrefix(r.Model, "grok-imagine-video") {
		body["video_mode"] = r.mode()
	}
	return body
}

// GenerateVideo produces exactly one video asset for one shot: it submits a
// task, polls it to completion and downloads the result into outDir.
func (s *Service) GenerateVideo(ctx context.Context, req VideoRequest, outDir string) ([]store.AssetRef, error) {
	cfg := s.cfg.Get()
	if !cfg.VideoReady() {
		return nil, errors.New("未配置视频 API（请在设置中填写视频地址与 Key）")
	}
	task := &videoTask{s: s, endpoint: cfg.VideoAPIURL, key: cfg.VideoAPIKey, requestID: uuid.NewString()}
	started := time.Now()
	if err := req.validate(); err != nil {
		return nil, err
	}
	slog.Info("video.request.start", "requestId", task.requestID, "model", req.Model,
		"endpoint", platform.SafeURL(cfg.VideoAPIURL), "durationS", req.DurationS,
		"aspectRatio", optional(req.AspectRatio), "resolution", optional(req.Resolution), "mode", req.mode(),
		"referenceCounts", map[string]any{"images": len(req.Images), "videos": len(req.Videos), "audios": len(req.Audios)},
		"promptStats", platform.TextStats(req.Prompt))

	data, err := task.run(ctx, req)
	if err != nil {
		slog.Error("video.request.end", "requestId", task.requestID, "status", "error",
			"durationMs", time.Since(started).Milliseconds(), "error", platform.ErrorText(err))
		return nil, err
	}
	// The folder is created only for a finished video, so failed or rejected
	// requests do not leave empty run folders behind.
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		return nil, fmt.Errorf("创建目录失败：%v", err)
	}
	path := filepath.Join(outDir, "video_"+uuid.NewString()+".mp4")
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return nil, fmt.Errorf("保存视频失败：%v", err)
	}
	duration := float64(req.DurationS)
	slog.Info("video.request.end", "requestId", task.requestID, "status", "success",
		"durationMs", time.Since(started).Milliseconds(), "assetCount", 1, "bytes", len(data), "path", path)
	return []store.AssetRef{{
		ID: "zzone:" + task.id, Kind: "video", Path: path, DurationS: &duration, Format: "mp4",
	}}, nil
}

// videoTask is one asynchronous provider task.
type videoTask struct {
	s             *Service
	endpoint, key string
	requestID     string
	id            string
}

const segmentIndex = 0 // a shot is one segment; logs keep the field

func (t *videoTask) run(ctx context.Context, req VideoRequest) ([]byte, error) {
	if err := t.create(ctx, req); err != nil {
		return nil, err
	}
	if err := t.wait(ctx); err != nil {
		return nil, err
	}
	return t.download(ctx)
}

func (t *videoTask) send(ctx context.Context, method, target string, body []byte, timeout time.Duration) (*http.Response, error) {
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	req, err := http.NewRequest(method, target, reader)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+t.key)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	return platform.Do(ctx, t.s.clientFor(t.endpoint), req, platform.Timeouts{Total: timeout})
}

// create submits the task (POST /v1/videos) and keeps its id.
func (t *videoTask) create(ctx context.Context, req VideoRequest) error {
	body, err := encodeJSON(videoBody(req))
	if err != nil {
		return err
	}
	started := time.Now()
	resp, err := t.send(ctx, http.MethodPost, t.endpoint, body, t.s.videoTiming.create)
	if err != nil {
		return fmt.Errorf("创建视频任务失败：%v", err)
	}
	text, _ := readBody(resp, 1<<20, "视频创建响应", nil)
	slog.Info("video.task.create_response", "requestId", t.requestID, "segmentIndex", segmentIndex,
		"status", resp.StatusCode, "durationMs", time.Since(started).Milliseconds())
	if !isSuccess(resp) {
		if reason := providerErrorText(string(text)); reason != "" {
			return fmt.Errorf("创建视频任务返回 %s: %s", statusLine(resp.StatusCode), reason)
		}
		return fmt.Errorf("创建视频任务返回 %s", statusLine(resp.StatusCode))
	}
	created, err := decodeJSON(text)
	if err != nil {
		return errors.New("解析创建响应失败")
	}
	object, _ := created.(map[string]any)
	var value any
	for _, key := range []string{"id", "task_id", "taskId"} {
		if v, ok := object[key]; ok {
			value = v
			break
		}
	}
	id, ok := value.(string)
	if !ok {
		return errors.New("创建响应缺少任务 id")
	}
	if !validTaskID(id) {
		return errors.New("创建响应中的任务 id 格式无效")
	}
	t.id = id
	return nil
}

func validTaskID(id string) bool {
	if id == "" || len(id) > 256 {
		return false
	}
	for _, c := range id {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || strings.ContainsRune("-_.:", c)) {
			return false
		}
	}
	return true
}

// taskURL is "<endpoint>/<id><suffix>", extending the path so a query string
// on the configured address stays a query string.
func taskURL(base, id, suffix string) string {
	u, err := url.Parse(base)
	if err != nil || u.Scheme == "" || u.Host == "" {
		return strings.TrimRight(base, "/") + "/" + id + suffix
	}
	u.Path = strings.TrimRight(u.Path, "/") + "/" + id + suffix
	u.RawPath = ""
	return u.String()
}

// wait polls until the task completes. The task exists and is billed, so
// transient failures and unreadable answers keep polling rather than failing
// the shot and inviting a paid resubmission.
func (t *videoTask) wait(ctx context.Context) error {
	timing := t.s.videoTiming
	target := taskURL(t.endpoint, t.id, "")
	if err := sleep(ctx, timing.firstPoll); err != nil {
		return err
	}
	deadline := time.Now().Add(timing.pollDeadline)
	failures := 0
	retry := func(fields ...any) error {
		failures++
		steps := backoffSteps(failures)
		fields = append([]any{"requestId", t.requestID, "segmentIndex", segmentIndex, "taskId", t.id}, fields...)
		slog.Warn("video.task.poll_retry", append(fields, "retryInS", steps*5)...)
		return sleep(ctx, time.Duration(steps)*timing.backoffUnit)
	}
	for polls := 1; ; polls++ {
		pollStarted := time.Now()
		resp, err := t.send(ctx, http.MethodGet, target, nil, timing.poll)
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			if !time.Now().Before(deadline) {
				return fmt.Errorf("视频任务轮询超时（task_id=%s）：最后一次网络错误：%v", t.id, err)
			}
			if err := retry("pollCount", polls, "reason", "network", "error", platform.ErrorText(err)); err != nil {
				return err
			}
			continue
		}
		body, readErr := readBody(resp, 1<<20, "视频轮询响应", nil)
		if !isSuccess(resp) {
			if isTransientStatus(resp.StatusCode) && time.Now().Before(deadline) {
				if err := retry("pollCount", polls, "reason", "http", "httpStatus", resp.StatusCode); err != nil {
					return err
				}
				continue
			}
			return fmt.Errorf("查询任务返回 %s（task_id=%s）", statusLine(resp.StatusCode), t.id)
		}
		var parsed any
		reason := ""
		if readErr != nil {
			reason = readErr.Error()
		} else if parsed, err = decodeJSON(body); err != nil {
			reason = "响应不是 JSON"
		}
		if reason != "" {
			if !time.Now().Before(deadline) {
				return fmt.Errorf("解析任务状态失败（task_id=%s）：%s", t.id, reason)
			}
			if err := retry("pollCount", polls, "reason", "unreadable", "error", reason); err != nil {
				return err
			}
			continue
		}
		state, _ := parsed.(map[string]any) // a JSON non-object has no status
		failures = 0
		status, _ := state["status"].(string)
		slog.Debug("video.task.poll", "requestId", t.requestID, "segmentIndex", segmentIndex, "taskId", t.id,
			"pollCount", polls, "taskStatus", status, "httpStatus", resp.StatusCode,
			"durationMs", time.Since(pollStarted).Milliseconds())
		switch status {
		case "completed", "succeeded", "success":
			return nil
		case "failed", "error", "cancelled", "expired":
			message, ok := providerErrorMessage(state)
			if !ok {
				message = "服务端未返回失败原因"
			}
			return fmt.Errorf("视频任务失败（%s）：%s", status, redactDataURLs(message))
		}
		if !time.Now().Before(deadline) {
			return fmt.Errorf("视频任务超时（task_id=%s，status=%s）", t.id, status)
		}
		if err := sleep(ctx, timing.pollInterval); err != nil {
			return err
		}
	}
}

// download fetches the finished video from /content.
func (t *videoTask) download(ctx context.Context) ([]byte, error) {
	policy := t.s.videoDownload
	policy.Header = http.Header{"Authorization": {"Bearer " + t.key}}
	started := time.Now()
	data, err := platform.Download(ctx, t.s.clientFor(t.endpoint), taskURL(t.endpoint, t.id, "/content"), policy, nil)
	if err != nil {
		return nil, fmt.Errorf("%w（task_id=%s）", err, t.id)
	}
	if !media.ValidVideo(data, "mp4") {
		return nil, errors.New("视频接口返回的内容不是有效 MP4")
	}
	slog.Info("video.download", "requestId", t.requestID, "segmentIndex", segmentIndex, "taskId", t.id,
		"durationMs", time.Since(started).Milliseconds(), "bytes", len(data))
	return data, nil
}

func isTransientStatus(status int) bool {
	switch status {
	case 408, 425, 429, 500, 502, 503, 504:
		return true
	}
	return status >= 520 && status <= 529
}

// backoffSteps is how many backoff units (5 s) to wait after the n-th
// consecutive transient failure: 1..6, so 5 s growing to 30 s.
func backoffSteps(n int) int { return min(max(n, 1), 6) }

// providerErrorMessage finds the provider's failure reason.
func providerErrorMessage(state map[string]any) (string, bool) {
	var candidates []any
	if nested, ok := state["error"].(map[string]any); ok {
		if message, ok := nested["message"]; ok {
			candidates = append(candidates, message)
		}
	}
	for _, key := range []string{"error", "failure_reason", "failReason", "message"} {
		if value, ok := state[key]; ok {
			candidates = append(candidates, value)
		}
	}
	for _, candidate := range candidates {
		switch v := candidate.(type) {
		case string:
			if text := strings.TrimSpace(v); text != "" {
				return text, true
			}
		case map[string]any, []any:
			if text, err := encodeJSON(v); err == nil {
				return string(text), true
			}
		}
	}
	return "", false
}

// redactDataURLs hides inline image data a provider echoes back.
func redactDataURLs(value string) string {
	var out strings.Builder
	rest := value
	for {
		start := strings.Index(rest, "data:image/")
		if start < 0 {
			break
		}
		out.WriteString(rest[:start])
		payload := rest[start:]
		marker := strings.Index(payload, ";base64,")
		if marker < 0 {
			out.WriteString(payload)
			return out.String()
		}
		encoded := payload[marker+len(";base64,"):]
		end := strings.IndexFunc(encoded, func(r rune) bool {
			return !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || strings.ContainsRune("+/=-_", r))
		})
		if end < 0 {
			end = len(encoded)
		}
		out.WriteString("[已隐藏本地图片数据]")
		rest = encoded[end:]
	}
	out.WriteString(rest)
	return out.String()
}

// localImageSource names a desktop image for a video request by asset id or
// comic catalog URI, never by path, so the frontend cannot point the backend
// at arbitrary files.
type localImageSource struct {
	AssetID   *string `json:"assetId"`
	SourceURI *string `json:"sourceUri"`
}

// CatalogEntry is one item of a project's comic catalog.
type CatalogEntry struct {
	SourceURI string
	Kind      string // "image" | "text"
	Path      string // "" when the file is missing
}

func localImageSources(node nodeConfig) ([]localImageSource, error) {
	raw, ok := node["local_images"]
	if !ok {
		return nil, nil
	}
	invalid := errors.New("本地图片引用格式无效")
	if len(raw) == 0 || raw[0] != '[' {
		return nil, invalid
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	var sources []localImageSource
	if decoder.Decode(&sources) != nil {
		return nil, invalid
	}
	return sources, nil
}

func nonBlank(s *string) bool { return s != nil && strings.TrimSpace(*s) != "" }

// resolveLocalImages turns local references into bounded, verified image
// data URLs. Every source must belong to projectID.
func (s *Service) resolveLocalImages(ctx context.Context, projectID string, sources []localImageSource) ([]string, error) {
	if len(sources) == 0 {
		return []string{}, nil
	}
	if strings.TrimSpace(projectID) == "" {
		return nil, errors.New("本地图片引用需要项目标识")
	}
	if len(sources) > maxLocalImages {
		return nil, errors.New("本地图片引用最多为 30 张")
	}
	var catalog []CatalogEntry
	if slices.ContainsFunc(sources, func(src localImageSource) bool { return nonBlank(src.SourceURI) }) {
		if s.comicCatalog != nil {
			var err error
			if catalog, err = s.comicCatalog(ctx, projectID); err != nil {
				return nil, err
			}
		}
	}
	var total int64
	images := make([]string, 0, len(sources))
	for _, src := range sources {
		hasAsset, hasURI := nonBlank(src.AssetID), nonBlank(src.SourceURI)
		if hasAsset == hasURI {
			return nil, errors.New("每个本地图片来源必须且只能提供 assetId 或 sourceUri")
		}
		var path string
		var err error
		if hasAsset {
			path, err = s.assetImagePath(ctx, projectID, *src.AssetID)
		} else {
			path, err = catalogImagePath(catalog, strings.TrimSpace(*src.SourceURI))
		}
		if err != nil {
			return nil, err
		}
		data, err := readBoundedImage(path, &total)
		if err != nil {
			return nil, err
		}
		_, mime, err := verifiedLocalImage(data)
		if err != nil {
			return nil, err
		}
		data, mime = compactForUpload(data, mime)
		images = append(images, "data:"+mime+";base64,"+base64.StdEncoding.EncodeToString(data))
	}
	return images, nil
}

func (s *Service) assetImagePath(ctx context.Context, projectID, assetID string) (string, error) {
	if s.db == nil {
		return "", errors.New("读取本地图片资产失败")
	}
	var (
		found                bool
		kind, path, metadata string
	)
	err := s.db.With(ctx, func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, "SELECT kind, path, metadata FROM assets WHERE id = ?", &sqlitex.ExecOptions{
			Args: []any{assetID},
			ResultFunc: func(stmt *sqlite.Stmt) error {
				found = true
				kind, path, metadata = stmt.ColumnText(0), stmt.ColumnText(1), stmt.ColumnText(2)
				return nil
			},
		})
	})
	if err != nil {
		return "", errors.New("读取本地图片资产失败")
	}
	if !found {
		return "", errors.New("本地图片资产不存在")
	}
	var meta struct {
		ProjectID any `json:"projectId"`
	}
	json.Unmarshal([]byte(metadata), &meta)
	if owner, _ := meta.ProjectID.(string); owner != projectID {
		return "", errors.New("本地图片资产不属于当前项目")
	}
	if kind != "image" {
		return "", errors.New("本地视频引用只能使用图片资产")
	}
	return path, nil
}

func catalogImagePath(catalog []CatalogEntry, sourceURI string) (string, error) {
	i := slices.IndexFunc(catalog, func(e CatalogEntry) bool { return e.SourceURI == sourceURI })
	if i < 0 {
		return "", errors.New("漫画图片来源不存在或不属于当前项目")
	}
	if catalog[i].Kind != "image" {
		return "", errors.New("漫画来源不是图片")
	}
	if catalog[i].Path == "" {
		return "", errors.New("漫画图片文件不存在")
	}
	return catalog[i].Path, nil
}

// readBoundedImage reads one local image, keeping all of them together
// within maxLocalImageBytes.
func readBoundedImage(path string, total *int64) ([]byte, error) {
	resolved, err := canonicalPath(path)
	if path == "" || err != nil {
		return nil, errors.New("本地图片文件不存在或不可访问")
	}
	info, err := os.Stat(resolved)
	if err != nil {
		return nil, errors.New("读取本地图片文件失败")
	}
	if !info.Mode().IsRegular() {
		return nil, errors.New("本地图片来源必须是普通文件")
	}
	single := errors.New("单张本地图片不能超过 50 MiB")
	overall := errors.New("本地图片总大小不能超过 50 MiB")
	remaining := maxLocalImageBytes - *total
	if info.Size() > maxLocalImageBytes {
		return nil, single
	}
	if info.Size() > remaining {
		return nil, overall
	}
	file, err := os.Open(resolved)
	if err != nil {
		return nil, errors.New("读取本地图片文件失败")
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, remaining+1))
	if err != nil {
		return nil, errors.New("读取本地图片文件失败")
	}
	if int64(len(data)) > maxLocalImageBytes {
		return nil, single
	}
	if int64(len(data)) > remaining {
		return nil, overall
	}
	*total += int64(len(data))
	return data, nil
}

// verifiedLocalImage accepts decodable PNG, JPEG and WebP images.
func verifiedLocalImage(data []byte) (format, mime string, err error) {
	format = media.DetectImageFormat(data)
	if format == "" {
		return "", "", errors.New("本地图片格式不受支持或文件已损坏")
	}
	switch format {
	case "png":
		mime = "image/png"
	case "jpg":
		mime = "image/jpeg"
	case "webp":
		mime = "image/webp"
	default:
		return "", "", errors.New("本地视频引用仅支持 PNG、JPEG 或 WebP 图片")
	}
	if media.ValidateImage(data) != nil {
		return "", "", errors.New("本地图片格式不受支持或文件已损坏")
	}
	return format, mime, nil
}
