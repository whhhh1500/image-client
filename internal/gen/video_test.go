package gen

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"image/color"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/config"
	"imageclient/internal/store"
)

func shot(model string, duration int) VideoRequest {
	return VideoRequest{Model: model, Prompt: "镜头缓慢推进", DurationS: duration, AspectRatio: "16:9", Resolution: "720p", Mode: "text"}
}

func localPNGDataURL(t *testing.T) string {
	return "data:image/png;base64," + base64.StdEncoding.EncodeToString(pngBytes(t, 1, 1, color.RGBA{1, 2, 3, 255}))
}

func wantError(t *testing.T, err error, fragment string) {
	t.Helper()
	if err == nil || !strings.Contains(err.Error(), fragment) {
		t.Fatalf("err = %v, want it to contain %q", err, fragment)
	}
}

func TestKeepsANativeThirtySecondSeedanceShotSingle(t *testing.T) {
	if err := shot("video-ds-2.5", 30).validate(); err != nil {
		t.Fatal(err)
	}
}

func TestRejectsUnsupportedDurationsInsteadOfSplittingAShot(t *testing.T) {
	wantError(t, validateDuration(31, 4, 30, secondsRange(4, 30)), "不会自动拆分")
	wantError(t, shot("video-ds-2.0", 16).validate(), "不会自动拆分 16 秒镜头")
	r := shot("drama-video-v2", 7)
	r.Mode = "text"
	if err := r.validate(); err == nil || err.Error() != "当前模型不支持 7 秒，可选值：[5, 10, 15]；不会自动拆分镜头" {
		t.Fatalf("err = %v", err)
	}
	// Grok reference mode has its own, shorter ceiling.
	r = shot("grok-imagine-video", 12)
	r.Mode = "reference"
	for i := range 7 {
		r.Images = append(r.Images, "https://example.com/"+string(rune('a'+i))+".png")
	}
	wantError(t, r.validate(), "1–10 秒")
}

func TestGrokReferenceModeRequiresExactlySevenHTTPSImages(t *testing.T) {
	r := shot("grok-imagine-video", 10)
	r.Mode = "reference"
	for i := range 6 {
		r.Images = append(r.Images, "https://example.com/"+string(rune('0'+i))+".png")
	}
	wantError(t, r.validate(), "恰好提供 7 张")
	r.Images = append(r.Images, "https://example.com/6.png")
	if err := r.validate(); err != nil {
		t.Fatal(err)
	}
}

func TestRejectsLocalReferencePaths(t *testing.T) {
	r := shot("grok-imagine-video", 5)
	r.Mode = "first_frame"
	r.Images = []string{`C:\images\ref.png`}
	wantError(t, r.validate(), "HTTPS URL")
}

func TestAcceptsVerifiedLocalDataImagesButNotForVideoOrAudio(t *testing.T) {
	r := shot("kling-video-v3", 5)
	r.Mode = "reference"
	r.Images = []string{localPNGDataURL(t), "https://cdn.example.com/reference.png"}
	if err := r.validate(); err != nil {
		t.Fatal(err)
	}
	r.Videos = []string{localPNGDataURL(t)}
	wantError(t, r.validate(), "公网 HTTPS")

	for value, want := range map[string]string{
		"data:image/gif;base64,R0lGODlh":                    "本地图片数据格式无效",
		"data:image/png;base64,":                            "本地图片数据超过 50 MiB 大小限制",
		"data:image/png;base64,!!!!":                        "本地图片数据格式无效",
		"data:image/jpeg;base64," + localPNGDataURL(t)[22:]: "本地图片数据格式无效", // declared JPEG, actually PNG
	} {
		r := shot("kling-video-v3", 5)
		r.Mode = "first_frame"
		r.Images = []string{value}
		if err := r.validate(); err == nil || err.Error() != want {
			t.Errorf("%.30s: err = %v, want %s", value, err, want)
		}
	}
}

func TestSendsTheLocalImageDataURLInTheNativeJSONRequest(t *testing.T) {
	var captured map[string]any
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewDecoder(r.Body).Decode(&captured)
		io.WriteString(w, `{"id":"task_123"}`)
	}))
	defer server.Close()
	env := newTestService(t, func(s *config.Settings) { s.VideoAPIURL, s.VideoAPIKey = server.URL, "local-test-key" })
	r := shot("grok-imagine-video", 1)
	r.Mode = "first_frame"
	dataURL := localPNGDataURL(t)
	r.Images = []string{dataURL}
	if err := r.validate(); err != nil {
		t.Fatal(err)
	}
	task := &videoTask{s: env.s, endpoint: server.URL, key: "local-test-key", requestID: "request"}
	if err := task.create(context.Background(), r); err != nil {
		t.Fatal(err)
	}
	if task.id != "task_123" || captured["images"].([]any)[0] != dataURL {
		t.Fatalf("id = %s, body = %v", task.id, captured)
	}
}

func TestRedactsLocalDataURLsFromProviderFailures(t *testing.T) {
	redacted := redactDataURLs("provider echoed " + localPNGDataURL(t) + " and more")
	if redacted != "provider echoed [已隐藏本地图片数据] and more" {
		t.Fatalf("redacted = %q", redacted)
	}
	if redactDataURLs("data:image/png no marker") != "data:image/png no marker" {
		t.Fatal("text without a payload stays")
	}
}

func TestBuildsMultimodalArraysAndTheGrokMode(t *testing.T) {
	r := shot("grok-imagine-video", 8)
	r.Mode, r.Resolution = "first_frame", ""
	r.Images = []string{"https://example.com/first.png"}
	body := videoBody(r)
	want := map[string]any{"model": "grok-imagine-video", "prompt": "镜头缓慢推进", "seconds": "8", "resolution": "720p",
		"video_mode": "first_frame", "images": []string{"https://example.com/first.png"}}
	if !reflect.DeepEqual(body, want) {
		t.Fatalf("body = %v", body)
	}
	r = VideoRequest{Model: "kling-video-v3", Prompt: "p", DurationS: 5, Videos: []string{"https://x.example/v.mp4"}}
	body = videoBody(r)
	if body["aspect_ratio"] != "16:9" || body["videos"] == nil || body["video_mode"] != nil || r.mode() != "text" {
		t.Fatalf("body = %v", body)
	}
}

func TestRejectsNonPublicReferenceURLs(t *testing.T) {
	for _, url := range []string{
		"http://cdn.example.com/ref.png", "https://localhost/ref.png", "https://127.0.0.1/ref.png",
		"https://192.168.1.2/ref.png", "https://user:secret@cdn.example.com/ref.png", "https://user@cdn.example.com/ref.png",
		"https://127.1/ref.png", "https://0x7f000001/ref.png", "https://2130706433/ref.png", "https://0177.0.0.1/ref.png",
		"https://[::1]/ref.png", "https://[2001:4860::8888]/ref.png", "https://10.0.0.1/", "https://172.20.1.1/",
		"https://100.64.0.1/", "https://169.254.1.1/", "https://224.0.0.1/", "https://0.1.2.3/", "https://192.0.2.1/",
		"https://printer.local/", "https://svc.internal/", "https://nas.lan/", "https://app.localhost/", "https://intranet/",
		"https://256.1.1.1/", "https://1.2.3.4.5/", "ftp://cdn.example.com/x", "not a url", "",
	} {
		r := shot("grok-imagine-video", 5)
		r.Mode = "first_frame"
		r.Images = []string{url}
		wantError(t, r.validate(), "公网 HTTPS")
		if ValidatePublicHTTPSURLs([]string{url}, "images") == nil {
			t.Errorf("%q must be refused", url)
		}
	}
	for _, url := range []string{"https://cdn.example.com/ref.png", " https://CDN.Example.com/a ", "https://8.8.8.8/x", "https://172.32.0.1/", "https://1.2.3/"} {
		if err := ValidatePublicHTTPSURLs([]string{url}, "images"); err != nil {
			t.Errorf("%q: %v", url, err)
		}
	}
	if err := ValidatePublicHTTPSURLs([]string{"http://x.example"}, "videos"); err == nil || err.Error() != "videos必须是公网 HTTPS URL" {
		t.Fatalf("err = %v", err)
	}
}

func TestDramaModelsEnforceTheDocumentedTotalReferenceLimit(t *testing.T) {
	for _, model := range []string{"drama-video-v2", "drama-video-v2-fast"} {
		r := shot(model, 5)
		r.Mode = "reference"
		for i := range 9 {
			r.Images = append(r.Images, "https://cdn.example.com/"+string(rune('a'+i))+".png")
		}
		r.Videos = []string{"https://cdn.example.com/1.mp4", "https://cdn.example.com/2.mp4", "https://cdn.example.com/3.mp4"}
		r.Audios = []string{"https://cdn.example.com/a.mp3"}
		wantError(t, r.validate(), "总数最多为 12")
	}
}

func TestOnlyStandardDramaRequiresAnImageWithAVideoReference(t *testing.T) {
	standard := shot("drama-video-v2", 5)
	standard.Mode = "reference"
	standard.Videos = []string{"https://cdn.example.com/ref.mp4"}
	wantError(t, standard.validate(), "至少一张参考图")
	fast := shot("drama-video-v2-fast", 5)
	fast.Mode = "reference"
	fast.Videos = []string{"https://cdn.example.com/ref.mp4"}
	if err := fast.validate(); err != nil {
		t.Fatal(err)
	}
}

func TestModesResolutionsAndRatiosFollowTheCapability(t *testing.T) {
	r := shot("video-ds-2.0", 5)
	r.Mode = "reference"
	wantError(t, r.validate(), "不支持生成模式 reference")
	r = shot("video-ds-2.0", 5)
	r.Images = []string{"https://cdn.example.com/a.png"}
	wantError(t, r.validate(), "纯文本模式不能携带参考素材")
	r.Mode = ""
	if r.mode() != "first_frame" || r.validate() != nil {
		t.Fatal("images imply first_frame mode")
	}
	r = shot("kling-video-v3", 5)
	r.Mode = "first_frame"
	wantError(t, r.validate(), "首帧模式需要且只能提供 1 张参考图片")
	r.Mode = "reference"
	wantError(t, r.validate(), "多素材参考模式至少需要一个参考素材")
	r = shot("kling-video-v3-turbo", 5)
	r.Resolution = "4k"
	wantError(t, r.validate(), "可灵 V3 Turbo 不支持分辨率 4k")
	r = shot("kling-video-v3", 5)
	r.AspectRatio = "4:3"
	wantError(t, r.validate(), "可灵 V3 不支持画幅 4:3")
	r = shot("minimax-h3", 5)
	r.Mode = "reference"
	r.Audios = []string{"https://cdn.example.com/a.mp3", "https://cdn.example.com/b.mp3"}
	wantError(t, r.validate(), "参考素材超过模型上限（图片 0/5, 视频 0/3, 音频 2/1）")
}

func TestRecognizesLiveMinimaxAliasesAndFailsClosedForUnknownModels(t *testing.T) {
	r := shot("minimax-h3-4k", 5)
	r.Resolution = "4k"
	if err := r.validate(); err != nil {
		t.Fatal(err)
	}
	if err := shot("wan3-720p", 5).validate(); err == nil ||
		err.Error() != "当前客户端没有视频模型 wan3-720p 的能力定义，请更新客户端或改用已验证模型" {
		t.Fatalf("err = %v", err)
	}
}

func TestCapabilitiesSerializeForTheFrontend(t *testing.T) {
	caps := VideoModelCapabilities()
	if len(caps) != 17 {
		t.Fatalf("%d capabilities", len(caps))
	}
	var decoded []map[string]any
	json.Unmarshal([]byte(mustJSON(t, caps)), &decoded)
	first := decoded[0]
	for _, key := range []string{"maxTotalReferences", "referenceImageCount", "maxReferenceDurationS"} {
		if value, ok := first[key]; !ok || value != nil {
			t.Fatalf("%s must be null, got %v", key, first)
		}
	}
	if first["id"] != "video-ds-2.5" || first["minDurationS"] != float64(4) || len(first["durationOptions"].([]any)) != 27 {
		t.Fatalf("first = %v", first)
	}
	grok := decoded[8]
	if grok["id"] != "grok-imagine-video" || grok["referenceImageCount"] != float64(7) || grok["maxReferenceDurationS"] != float64(10) {
		t.Fatalf("grok = %v", grok)
	}
	if decoded[10]["maxTotalReferences"] != float64(12) {
		t.Fatalf("drama = %v", decoded[10])
	}
}

func TestRetriesOnlyTransientPollFailuresWithBoundedBackoff(t *testing.T) {
	for _, code := range []int{408, 425, 429, 500, 502, 503, 504, 520, 524, 529} {
		if !isTransientStatus(code) {
			t.Errorf("%d is transient", code)
		}
	}
	for _, code := range []int{400, 401, 403, 404, 409, 422, 501, 530} {
		if isTransientStatus(code) {
			t.Errorf("%d is not transient", code)
		}
	}
	if backoffSteps(1)*5 != 5 || backoffSteps(3)*5 != 15 || backoffSteps(99)*5 != 30 || backoffSteps(0) != 1 {
		t.Fatal("backoff")
	}
	cases := []struct {
		state map[string]any
		want  string
	}{
		{map[string]any{"error": map[string]any{"message": "额度不足"}}, "额度不足"},
		{map[string]any{"failure_reason": "参考图无效"}, "参考图无效"},
		{map[string]any{"error": map[string]any{"code": "<x>"}}, `{"code":"<x>"}`},
		{map[string]any{"error": " ", "message": "m"}, "m"},
	}
	for _, c := range cases {
		if got, ok := providerErrorMessage(c.state); !ok || got != c.want {
			t.Errorf("%v: got %q", c.state, got)
		}
	}
	if _, ok := providerErrorMessage(map[string]any{"status": "failed"}); ok {
		t.Fatal("no reason")
	}
}

func TestTaskURLsExtendThePathAndKeepTheQueryString(t *testing.T) {
	if got := taskURL("https://gw.example/v1/videos/", "task-1", ""); got != "https://gw.example/v1/videos/task-1" {
		t.Fatal(got)
	}
	if got := taskURL("https://gw.example/v1/videos?tag=x", "task-1", "/content"); got != "https://gw.example/v1/videos/task-1/content?tag=x" {
		t.Fatal(got)
	}
	if got := taskURL("https://gw.example/v1/videos", "a:b.c", ""); got != "https://gw.example/v1/videos/a:b.c" {
		t.Fatal(got)
	}
}

func TestTaskIDsAreValidated(t *testing.T) {
	for _, id := range []string{"task_1", "a-b.c:d", strings.Repeat("a", 256)} {
		if !validTaskID(id) {
			t.Errorf("%q is valid", id)
		}
	}
	for _, id := range []string{"", "a/b", "a b", strings.Repeat("a", 257), "任务"} {
		if validTaskID(id) {
			t.Errorf("%q is invalid", id)
		}
	}
}

// videoGateway scripts the task API: create, a sequence of poll answers, and
// the content.
type videoGateway struct {
	server  *httptest.Server
	mu      sync.Mutex
	polls   []postResponse
	content postResponse
	create  postResponse
	paths   []string
	auth    []string
}

func startVideoGateway(t *testing.T, create postResponse, polls []postResponse, content postResponse) *videoGateway {
	g := &videoGateway{create: create, polls: polls, content: content}
	g.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		g.mu.Lock()
		g.paths = append(g.paths, r.Method+" "+r.URL.Path)
		g.auth = append(g.auth, r.Header.Get("Authorization"))
		answer := g.create
		switch {
		case strings.HasSuffix(r.URL.Path, "/content"):
			answer = g.content
		case r.Method == http.MethodGet:
			answer = g.polls[0]
			if len(g.polls) > 1 {
				g.polls = g.polls[1:]
			}
		}
		g.mu.Unlock()
		w.WriteHeader(answer.status)
		io.WriteString(w, answer.body)
	}))
	t.Cleanup(g.server.Close)
	return g
}

func mp4Bytes() string { return "\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom" }

func TestGenerateVideoRunsTheTaskLifecycle(t *testing.T) {
	gw := startVideoGateway(t, postResponse{200, `{"task_id":"task_1"}`},
		[]postResponse{{200, `{"status":"queued"}`}, {503, "busy"}, {200, "<html>proxy</html>"}, {200, `{"status":"completed"}`}},
		postResponse{200, mp4Bytes()})
	env := newTestService(t, func(s *config.Settings) {
		s.VideoAPIURL, s.VideoAPIKey = gw.server.URL+"/v1/videos?tenant=t", "video-key"
	})
	dir := filepath.Join(env.output, "视频", "run")
	assets, err := env.s.GenerateVideo(context.Background(), VideoRequest{Model: "kling-video-v3", Prompt: "p", DurationS: 5}, dir)
	if err != nil {
		t.Fatal(err)
	}
	encoded := mustJSON(t, assets)
	if len(assets) != 1 || assets[0].ID != "zzone:task_1" || assets[0].Kind != "video" || assets[0].Format != "mp4" ||
		*assets[0].DurationS != 5 || !strings.HasPrefix(filepath.Base(assets[0].Path), "video_") || filepath.Dir(assets[0].Path) != dir {
		t.Fatalf("assets = %s", encoded)
	}
	if data, _ := os.ReadFile(assets[0].Path); string(data) != mp4Bytes() {
		t.Fatal("video bytes")
	}
	want := []string{"POST /v1/videos", "GET /v1/videos/task_1", "GET /v1/videos/task_1", "GET /v1/videos/task_1",
		"GET /v1/videos/task_1", "GET /v1/videos/task_1/content"}
	if !reflect.DeepEqual(gw.paths, want) {
		t.Fatalf("paths = %v", gw.paths)
	}
	for _, auth := range gw.auth {
		if auth != "Bearer video-key" {
			t.Fatalf("auth = %q", auth)
		}
	}
}

func TestVideoFailuresSurfaceTheProviderReason(t *testing.T) {
	dataURL := localPNGDataURL(t)
	cases := []struct {
		create  postResponse
		polls   []postResponse
		content postResponse
		want    string
	}{
		{postResponse{500, "x"}, nil, postResponse{}, "创建视频任务返回 500 Internal Server Error"},
		{postResponse{200, "nope"}, nil, postResponse{}, "解析创建响应失败"},
		{postResponse{200, `{"id":7}`}, nil, postResponse{}, "创建响应缺少任务 id"},
		{postResponse{200, `{"id":"a/b"}`}, nil, postResponse{}, "创建响应中的任务 id 格式无效"},
		{postResponse{200, `{"id":"t"}`}, []postResponse{{404, ""}}, postResponse{}, "查询任务返回 404 Not Found（task_id=t）"},
		{postResponse{200, `{"id":"t"}`}, []postResponse{{200, `{"status":"failed","error":{"message":"bad ` + dataURL + `"}}`}}, postResponse{},
			"视频任务失败（failed）: bad [已隐藏本地图片数据]"},
		{postResponse{200, `{"id":"t"}`}, []postResponse{{200, `{"status":"expired"}`}}, postResponse{}, "视频任务失败（expired）: 服务端未返回失败原因"},
		{postResponse{200, `{"id":"t"}`}, []postResponse{{200, `{"status":"succeeded"}`}}, postResponse{200, "not a video"}, "视频接口返回的内容不是有效 MP4"},
		{postResponse{200, `{"id":"t"}`}, []postResponse{{200, `{"status":"success"}`}}, postResponse{403, ""}, "下载视频返回 403 Forbidden（task_id=t）"},
		{postResponse{200, `{"id":"t"}`}, []postResponse{{200, `{"status":"success"}`}}, postResponse{502, ""}, "下载视频返回 502 Bad Gateway（task_id=t）"},
	}
	for _, c := range cases {
		gw := startVideoGateway(t, c.create, c.polls, c.content)
		env := newTestService(t, func(s *config.Settings) { s.VideoAPIURL, s.VideoAPIKey = gw.server.URL, "k" })
		_, err := env.s.GenerateVideo(context.Background(), VideoRequest{Model: "kling-video-v3", Prompt: "p", DurationS: 5}, t.TempDir())
		if err == nil || err.Error() != c.want {
			t.Errorf("err = %v, want %s", err, c.want)
		}
	}
}

func TestPollingStopsAtTheDeadline(t *testing.T) {
	gw := startVideoGateway(t, postResponse{200, `{"id":"t"}`}, []postResponse{{200, `{"status":"running"}`}}, postResponse{})
	env := newTestService(t, func(s *config.Settings) { s.VideoAPIURL, s.VideoAPIKey = gw.server.URL, "k" })
	env.s.videoTiming.pollDeadline = 30 * time.Millisecond
	_, err := env.s.GenerateVideo(context.Background(), VideoRequest{Model: "kling-video-v3", Prompt: "p", DurationS: 5}, t.TempDir())
	if err == nil || err.Error() != "视频任务超时（task_id=t，status=running）" {
		t.Fatalf("err = %v", err)
	}
}

func TestGenerateVideoChecksTheConnectionFirst(t *testing.T) {
	env := newTestService(t, nil)
	_, err := env.s.GenerateVideo(context.Background(), shot("kling-video-v3", 5), t.TempDir())
	if err == nil || err.Error() != "未配置视频 API（请在设置中填写视频地址与 Key）" {
		t.Fatalf("err = %v", err)
	}
}

func TestRunVideoReadsTheNodeConfig(t *testing.T) {
	gw := startVideoGateway(t, postResponse{200, `{"id":"t"}`}, []postResponse{{200, `{"status":"completed"}`}}, postResponse{200, mp4Bytes()})
	env := newTestService(t, func(s *config.Settings) {
		s.VideoAPIURL, s.VideoAPIKey, s.VideoModel = gw.server.URL, "k", "kling-video-v3"
	})
	run, err := env.s.runVideo(context.Background(), runVideoArgs{RunNodeRequest{Config: json.RawMessage(`{"prompt":"海边","duration_s":6,"images":[" https://cdn.example.com/a.png ",""],"local_images":[]}`)}})
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Dir(run.Assets[0].Path)
	if filepath.Dir(dir) != filepath.Join(env.output, "视频") || !strings.Contains(filepath.Base(dir), "_海边_") || *run.Assets[0].DurationS != 6 {
		t.Fatalf("asset = %s", mustJSON(t, run))
	}
	for config, want := range map[string]string{
		`{"prompt":" "}`:                                  "缺少提示词",
		`{"prompt":"p","local_images":null}`:              "本地图片引用格式无效",
		`{"prompt":"p","local_images":[{"path":"/x"}]}`:   "本地图片引用格式无效",
		`{"prompt":"p","local_images":[{"assetId":"a"}]}`: "本地图片引用需要项目标识",
	} {
		_, err := env.s.runVideo(context.Background(), runVideoArgs{RunNodeRequest{Config: json.RawMessage(config)}})
		if err == nil || err.Error() != want {
			t.Errorf("%s: err = %v", config, err)
		}
	}
}

func openTestDB(t *testing.T) *store.DB {
	t.Helper()
	db, err := store.Open(filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	return db
}

func insertAsset(t *testing.T, db *store.DB, id, kind, path, metadata string) {
	t.Helper()
	err := db.With(context.Background(), func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, "INSERT INTO assets (id, kind, path, created_at, metadata) VALUES (?, ?, ?, 1, ?)",
			&sqlitex.ExecOptions{Args: []any{id, kind, path, metadata}})
	})
	if err != nil {
		t.Fatal(err)
	}
}

func ptr(s string) *string { return &s }

func TestLocalImagesResolveToVerifiedDataURLsOfTheProject(t *testing.T) {
	db := openTestDB(t)
	dir := t.TempDir()
	image := filepath.Join(dir, "reference.png")
	writePNG(t, image, color.RGBA{1, 2, 3, 255})
	comic := filepath.Join(dir, "comic.png")
	writePNG(t, comic, color.RGBA{4, 5, 6, 255})
	oversized := writeFile(t, dir, "oversized.png", strings.Repeat("\x00", maxLocalImageBytes+1))
	insertAsset(t, db, "image-a", "image", image, `{"projectId":"project-a"}`)
	insertAsset(t, db, "other", "image", image, `{"projectId":"project-b"}`)
	insertAsset(t, db, "video", "video", image, `{"projectId":"project-a"}`)
	insertAsset(t, db, "large", "image", oversized, `{"projectId":"project-a"}`)
	insertAsset(t, db, "text", "image", writeFile(t, dir, "a.txt", "hello"), `{"projectId":"project-a"}`)

	env := newTestService(t, nil)
	env.s.db = db
	env.s.comicCatalog = func(_ context.Context, projectID string) ([]CatalogEntry, error) {
		if projectID != "project-a" {
			return nil, nil
		}
		return []CatalogEntry{
			{SourceURI: "comic-md://project-a/work/chapter/image/comic-image", Kind: "image", Path: comic},
			{SourceURI: "comic-md://project-a/work/chapter/text/doc", Kind: "text"},
			{SourceURI: "comic-md://project-a/work/chapter/image/gone", Kind: "image"},
		}, nil
	}
	ctx := context.Background()
	resolved, err := env.s.resolveLocalImages(ctx, "project-a", []localImageSource{
		{AssetID: ptr("image-a")},
		{SourceURI: ptr(" comic-md://project-a/work/chapter/image/comic-image ")},
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(resolved) != 2 || !strings.HasPrefix(resolved[0], "data:image/png;base64,") || strings.Contains(resolved[0], dir) {
		t.Fatalf("resolved = %.60q", resolved)
	}
	for _, c := range []struct {
		sources []localImageSource
		want    string
	}{
		{[]localImageSource{{AssetID: ptr("other")}}, "本地图片资产不属于当前项目"},
		{[]localImageSource{{AssetID: ptr("video")}}, "本地视频引用只能使用图片资产"},
		{[]localImageSource{{AssetID: ptr("missing")}}, "本地图片资产不存在"},
		{[]localImageSource{{AssetID: ptr("large")}}, "单张本地图片不能超过 50 MiB"},
		{[]localImageSource{{AssetID: ptr("text")}}, "本地图片格式不受支持或文件已损坏"},
		{[]localImageSource{{AssetID: ptr("image-a"), SourceURI: ptr("x")}}, "每个本地图片来源必须且只能提供 assetId 或 sourceUri"},
		{[]localImageSource{{AssetID: ptr(" ")}}, "每个本地图片来源必须且只能提供 assetId 或 sourceUri"},
		{[]localImageSource{{SourceURI: ptr("comic-md://project-b/work/chapter/image/comic-image")}}, "漫画图片来源不存在或不属于当前项目"},
		{[]localImageSource{{SourceURI: ptr("comic-md://project-a/work/chapter/text/doc")}}, "漫画来源不是图片"},
		{[]localImageSource{{SourceURI: ptr("comic-md://project-a/work/chapter/image/gone")}}, "漫画图片文件不存在"},
		{make([]localImageSource, 31), "本地图片引用最多为 30 张"},
	} {
		if _, err := env.s.resolveLocalImages(ctx, "project-a", c.sources); err == nil || err.Error() != c.want {
			t.Errorf("err = %v, want %s", err, c.want)
		}
	}
	if _, err := env.s.resolveLocalImages(ctx, " ", []localImageSource{{AssetID: ptr("image-a")}}); err == nil || err.Error() != "本地图片引用需要项目标识" {
		t.Fatalf("err = %v", err)
	}
	if got, err := env.s.resolveLocalImages(ctx, "", nil); err != nil || got == nil || len(got) != 0 {
		t.Fatal("no sources resolve to nothing")
	}
}

func TestFailedVideoRequestsLeaveNoRunFolder(t *testing.T) {
	gw := startVideoGateway(t, postResponse{502, "bad gateway"}, nil, postResponse{200, mp4Bytes()})
	env := newTestService(t, func(s *config.Settings) {
		s.VideoAPIURL, s.VideoAPIKey = gw.server.URL+"/v1/videos", "video-key"
	})
	for _, req := range []VideoRequest{
		{Model: "no-such-model", Prompt: "p", DurationS: 5},  // rejected before any request
		{Model: "kling-video-v3", Prompt: "p", DurationS: 5}, // the gateway refuses the task
	} {
		dir := filepath.Join(env.output, "视频", "run-"+req.Model)
		if _, err := env.s.GenerateVideo(context.Background(), req, dir); err == nil {
			t.Fatalf("%s: expected an error", req.Model)
		}
		if _, err := os.Stat(dir); !os.IsNotExist(err) {
			t.Fatalf("%s: run folder exists after a failed request (err=%v)", req.Model, err)
		}
	}
}
