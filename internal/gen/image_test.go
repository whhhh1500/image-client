package gen

import (
	"bufio"
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"image/color"
	"io"
	"mime"
	"mime/multipart"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"imageclient/internal/config"
	"imageclient/internal/store"
)

type postResponse struct {
	status int
	body   string
}

type gatewayRequest struct {
	method, path, query string
	header              http.Header
	body                []byte
}

// gateway is a loopback image gateway: POSTs answer from a queue (default: a
// data[].url result) and GET /asset.png serves the asset.
type gateway struct {
	server   *httptest.Server
	asset    []byte
	mu       sync.Mutex
	posts    []postResponse
	requests []gatewayRequest
	stall    time.Duration // delays POST answers
}

func startGateway(t *testing.T, asset []byte, posts []postResponse) *gateway {
	t.Helper()
	g := &gateway{asset: asset, posts: posts}
	g.server = httptest.NewServer(http.HandlerFunc(g.serve))
	t.Cleanup(g.server.Close)
	return g
}

func (g *gateway) serve(w http.ResponseWriter, r *http.Request) {
	body, _ := io.ReadAll(r.Body)
	g.mu.Lock()
	g.requests = append(g.requests, gatewayRequest{r.Method, r.URL.Path, r.URL.RawQuery, r.Header.Clone(), body})
	answer := postResponse{200, fmt.Sprintf(`{"data":[{"url":"%s/asset.png"}]}`, g.server.URL)}
	if r.Method == http.MethodPost && len(g.posts) > 0 {
		answer, g.posts = g.posts[0], g.posts[1:]
	}
	stall := g.stall
	g.mu.Unlock()
	if r.Method == http.MethodGet {
		w.Header().Set("Content-Type", "image/png")
		w.Header().Set("Content-Length", strconv.Itoa(len(g.asset)))
		w.Write(g.asset)
		return
	}
	if stall > 0 {
		select {
		case <-time.After(stall):
		case <-r.Context().Done():
			return
		}
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(answer.status)
	io.WriteString(w, answer.body)
}

func (g *gateway) configure(s *config.Settings) {
	s.ImageAPIURL = g.server.URL + "/v1/images/generations"
	s.ImageAPIKey = "local-test-key"
}

func (g *gateway) recorded() []gatewayRequest {
	g.mu.Lock()
	defer g.mu.Unlock()
	return append([]gatewayRequest(nil), g.requests...)
}

func (g *gateway) postedJSON(t *testing.T, i int) map[string]any {
	t.Helper()
	var posts []gatewayRequest
	for _, r := range g.recorded() {
		if r.method == http.MethodPost {
			posts = append(posts, r)
		}
	}
	var body map[string]any
	if err := json.Unmarshal(posts[i].body, &body); err != nil {
		t.Fatalf("post %d is not JSON: %v", i, err)
	}
	return body
}

func multipartFields(t *testing.T, r gatewayRequest) (map[string]string, map[string][]string) {
	t.Helper()
	_, params, err := mime.ParseMediaType(r.header.Get("Content-Type"))
	if err != nil {
		t.Fatal(err)
	}
	reader := multipart.NewReader(bytes.NewReader(r.body), params["boundary"])
	fields := map[string]string{}
	files := map[string][]string{}
	for {
		part, err := reader.NextPart()
		if err == io.EOF {
			return fields, files
		}
		if err != nil {
			t.Fatal(err)
		}
		data, _ := io.ReadAll(part)
		if part.FileName() != "" {
			files[part.FormName()] = []string{part.FileName(), part.Header.Get("Content-Type"), string(data[:4])}
		} else {
			fields[part.FormName()] = string(data)
		}
	}
}

func generate(t *testing.T, env *testEnv, req ImageRequest, onProgress func(ImageProgress)) ([]store.AssetRef, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	return env.s.GenerateImage(ctx, req, filepath.Join(env.output, "run"), onProgress)
}

func TestGenerationPostAndAssetGetAreSeparateForPlainAndReferenceRequests(t *testing.T) {
	for _, withReference := range []bool{false, true} {
		gw := startGateway(t, pngBytes(t, 4, 3, color.RGBA{2, 3, 4, 255}), nil)
		env := newTestService(t, gw.configure)
		req := ImageRequest{Prompt: "local test", Model: "local-model"}
		if withReference {
			path := filepath.Join(env.output, "reference.png")
			writePNG(t, path, color.RGBA{8, 9, 10, 255})
			req.References = []ImageReference{{Path: path, Role: "style"}}
		}
		assets, err := generate(t, env, req, nil)
		if err != nil {
			t.Fatal(err)
		}
		if len(assets) != 1 {
			t.Fatalf("assets = %v", assets)
		}
		if _, err := os.Stat(assets[0].Path); err != nil {
			t.Fatal(err)
		}
		requests := gw.recorded()
		if len(requests) != 2 || requests[0].method != http.MethodPost || requests[1].method != http.MethodGet {
			t.Fatalf("requests = %+v", requests)
		}
		if got := requests[0].header.Get("Authorization"); got != "Bearer local-test-key" {
			t.Fatalf("POST authorization = %q", got)
		}
		if requests[1].path != "/asset.png" || requests[1].header.Get("Authorization") != "" {
			t.Fatal("the asset GET must not carry the gateway key")
		}
		if withReference {
			if requests[0].path != "/v1/images/edits" {
				t.Fatalf("path = %s", requests[0].path)
			}
			fields, files := multipartFields(t, requests[0])
			want := map[string]string{"model": "local-model", "prompt": "local test", "n": "1", "size": "1024x1024"}
			if !reflect.DeepEqual(fields, want) {
				t.Fatalf("fields = %v", fields)
			}
			if got := files["image"]; got[0] != "ref.png" || got[1] != "image/png" || got[2] != "\x89PNG" {
				t.Fatalf("image part = %q", got)
			}
		} else if requests[0].path != "/v1/images/generations" {
			t.Fatalf("path = %s", requests[0].path)
		}
	}
}

func TestEditsEndpointKeepsUnrelatedText(t *testing.T) {
	got, err := editsEndpoint("https://example.com/v1/images/generations?tenant=generations")
	if err != nil || got != "https://example.com/v1/images/edits?tenant=generations" {
		t.Fatalf("got %q, %v", got, err)
	}
	if _, err := editsEndpoint("https://example.com/v1/images"); err == nil || err.Error() != "图生图要求图像 API 地址以 /generations 结尾" {
		t.Fatalf("err = %v", err)
	}
	if _, err := editsEndpoint("not a url/generations"); err == nil || !strings.HasPrefix(err.Error(), "图像 API 地址无效") {
		t.Fatalf("err = %v", err)
	}
}

func TestGrokBodiesUseAspectRatioAndResolutionInsteadOfSize(t *testing.T) {
	body := imageBody("grok-imagine-image", "a cat", "1536x1024", "1536x1024 (3:2)", "high", "transparent", 4)
	want := map[string]any{"model": "grok-imagine-image", "prompt": "a cat", "n": 4, "aspect_ratio": "3:2",
		"resolution": "1k", "response_format": "b64_json", "quality": "high"}
	if !reflect.DeepEqual(body, want) {
		t.Fatalf("body = %v", body)
	}
	// An unknown quality is dropped instead of sent as a bad enum.
	body = imageBody("grok-3-image", "a cat", "1024x1024", "1024x1024 (1:1)", "hd", "", 1)
	if body["aspect_ratio"] != "1:1" || body["n"] != 1 || body["quality"] != nil {
		t.Fatalf("body = %v", body)
	}
}

func TestOpenAICompatibleBodiesKeepSizeQualityAndBackground(t *testing.T) {
	body := imageBody("gpt-image-2", "a cat", "1024x1024", "1024x1024 (1:1)", "high", "transparent", 2)
	want := map[string]any{"model": "gpt-image-2", "prompt": "a cat", "n": 2, "size": "1024x1024", "quality": "high", "background": "transparent"}
	if !reflect.DeepEqual(body, want) {
		t.Fatalf("body = %v", body)
	}
	body = imageBody("gemini-3-pro-image", "a cat", "1024x1024", "1024x1024 (1:1)", "", "auto", 1)
	if _, ok := body["background"]; ok {
		t.Fatal("auto background must be omitted")
	}
	if _, ok := body["quality"]; ok {
		t.Fatal("empty quality must be omitted")
	}
}

func TestGrokRatioComesFromTheAnnotationOrThePixels(t *testing.T) {
	for selection, want := range map[string]string{
		"1280x720 (16:9)": "16:9", "720x1280 (9:16)": "9:16", "1024x1536": "2:3", "1024x1024": "1:1",
		"1500x1000": "3:2", "": "1:1", "not-a-size": "1:1", "1024×1536": "2:3", "1024X1536 (bogus)": "2:3",
	} {
		if got := grokAspectRatio(selection); got != want {
			t.Errorf("grokAspectRatio(%q) = %q, want %q", selection, got, want)
		}
	}
}

func TestGrokResolutionCollapsesOversizedSelections(t *testing.T) {
	for selection, want := range map[string]string{
		"1024x1024 (1:1)": "1k", "1280x720 (16:9)": "1k", "2048x2048 (2K)": "2k", "4096x4096 (4K)": "2k", "2000x1000": "2k",
	} {
		if got := grokResolution(selection); got != want {
			t.Errorf("grokResolution(%q) = %q, want %q", selection, got, want)
		}
	}
}

func TestSizeSelectionFallsBackToWidthAndHeight(t *testing.T) {
	cases := []struct {
		req                  ImageRequest
		selection, sizeParam string
	}{
		{ImageRequest{Size: "1024x1536 (2:3)"}, "1024x1536 (2:3)", "1024x1536"},
		{ImageRequest{}, "1024x1024", "1024x1024"},
		{ImageRequest{Width: 800, Height: 600}, "800x600", "800x600"},
		{ImageRequest{Size: "auto", Width: 512}, "auto", "512x1024"},
	}
	for _, c := range cases {
		if c.req.sizeSelection() != c.selection || c.req.sizeParam() != c.sizeParam {
			t.Errorf("%+v: %q %q", c.req, c.req.sizeSelection(), c.req.sizeParam())
		}
	}
}

func TestRequestedCountIsClampedToTheSupportedBatch(t *testing.T) {
	for config, want := range map[string]int{`{}`: 1, `{"n":4}`: 4, `{"n":0}`: 1, `{"n":5}`: 4, `{"n":99}`: 4, `{"n":2.5}`: 1} {
		req, err := imageRequestFromConfig(parseNodeConfig(json.RawMessage(strings.Replace(config, "{", `{"prompt":"x",`, 1))))
		if strings.HasSuffix(config, "{}") {
			req, err = imageRequestFromConfig(parseNodeConfig(json.RawMessage(`{"prompt":"x"}`)))
		}
		if err != nil {
			t.Fatal(err)
		}
		if got := clampImageCount(req.Count); got != want {
			t.Errorf("%s: count = %d, want %d", config, got, want)
		}
	}
	if MaxImageBatch != 4 {
		t.Fatal("the gateway supports four images per request")
	}
}

func TestIsGrokImageModelMatchesEveryGatewayIDShape(t *testing.T) {
	for _, model := range []string{"grok-imagine-image", "grok-imagine-image-2.0", "grok-imagine-image-quality", "  Grok-3-Image ", "xai/grok-2-image", "grok", "grok_x"} {
		if !isGrokImageModel(model) {
			t.Errorf("%q must use the Grok contract", model)
		}
	}
	for _, model := range []string{"grokking-image", "gpt-image-2.5", "gemini-3-pro-image", "nana-banana-pro"} {
		if isGrokImageModel(model) {
			t.Errorf("%q must keep the OpenAI contract", model)
		}
	}
}

func TestOpenAIModelsPostTheOpenAIBodyOverTheWire(t *testing.T) {
	gw := startGateway(t, pngBytes(t, 4, 3, color.RGBA{3, 3, 3, 255}), nil)
	env := newTestService(t, gw.configure)
	assets, err := generate(t, env, ImageRequest{Prompt: "一只猫", Model: "gpt-image-2.5", Size: "1536x1024 (3:2)",
		Quality: "high", Background: "transparent", Count: 2}, nil)
	if err != nil || len(assets) != 1 {
		t.Fatalf("assets = %v, err = %v", assets, err)
	}
	want := map[string]any{"model": "gpt-image-2.5", "prompt": "一只猫", "n": float64(2), "size": "1536x1024",
		"quality": "high", "background": "transparent"}
	if got := gw.postedJSON(t, 0); !reflect.DeepEqual(got, want) {
		t.Fatalf("posted %v", got)
	}
}

func TestAGatewayThatRejectsTheGrokContractGetsAnOpenAIRetry(t *testing.T) {
	png := pngBytes(t, 4, 3, color.RGBA{6, 6, 6, 255})
	inline := fmt.Sprintf(`{"data":[{"b64_json":%q}]}`, base64.StdEncoding.EncodeToString(png))
	gw := startGateway(t, png, []postResponse{{400, `{"error":{"message":"unknown parameter aspect_ratio"}}`}, {200, inline}})
	env := newTestService(t, gw.configure)
	var steps []string
	assets, err := generate(t, env, ImageRequest{Prompt: "一只猫", Model: "grok-imagine-image", Size: "1024x1536 (2:3)"},
		func(p ImageProgress) { steps = append(steps, p.Message) })
	if err != nil || len(assets) != 1 {
		t.Fatalf("assets = %v, err = %v", assets, err)
	}
	if len(gw.recorded()) != 2 {
		t.Fatal("expected the Grok body plus one retry")
	}
	first, retry := gw.postedJSON(t, 0), gw.postedJSON(t, 1)
	if first["aspect_ratio"] != "2:3" || first["size"] != nil {
		t.Fatalf("first = %v", first)
	}
	if retry["size"] != "1024x1536" || retry["aspect_ratio"] != nil || retry["resolution"] != nil {
		t.Fatalf("retry = %v", retry)
	}
	if steps[1] != grokFallbackStep {
		t.Fatalf("steps = %v", steps)
	}
}

func TestGrokImageToImageFallsBackToTheOpenAIForm(t *testing.T) {
	gw := startGateway(t, pngBytes(t, 4, 3, color.RGBA{6, 6, 6, 255}), []postResponse{{422, `{}`}})
	env := newTestService(t, gw.configure)
	path := filepath.Join(env.output, "ref.png")
	writePNG(t, path, color.RGBA{1, 1, 1, 255})
	_, err := generate(t, env, ImageRequest{Prompt: "猫", Model: "grok-imagine-image", Size: "2048x2048 (2K)", Quality: "hd",
		Background: "transparent", References: []ImageReference{{Path: path, Role: "base_image"}}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	requests := gw.recorded()
	first, _ := multipartFields(t, requests[0])
	retry, _ := multipartFields(t, requests[1])
	wantFirst := map[string]string{"model": "grok-imagine-image", "prompt": "猫", "n": "1", "aspect_ratio": "1:1",
		"resolution": "2k", "response_format": "b64_json", "quality": "hd"}
	if !reflect.DeepEqual(first, wantFirst) {
		t.Fatalf("first = %v", first)
	}
	wantRetry := map[string]string{"model": "grok-imagine-image", "prompt": "猫", "n": "1", "size": "2048x2048",
		"quality": "hd", "background": "transparent"}
	if !reflect.DeepEqual(retry, wantRetry) {
		t.Fatalf("retry = %v", retry)
	}
}

func TestGrokTextToImagePostsGrokParametersOverTheWire(t *testing.T) {
	gw := startGateway(t, pngBytes(t, 4, 3, color.RGBA{7, 8, 9, 255}), nil)
	env := newTestService(t, gw.configure)
	if _, err := generate(t, env, ImageRequest{Prompt: "一只猫", Model: "grok-3-image", Size: "1024x1536 (2:3)",
		Quality: "high", Background: "transparent"}, nil); err != nil {
		t.Fatal(err)
	}
	want := map[string]any{"model": "grok-3-image", "prompt": "一只猫", "n": float64(1), "aspect_ratio": "2:3",
		"resolution": "1k", "response_format": "b64_json", "quality": "high"}
	if got := gw.postedJSON(t, 0); !reflect.DeepEqual(got, want) {
		t.Fatalf("posted %v", got)
	}
	if gw.recorded()[0].path != "/v1/images/generations" {
		t.Fatal("text-to-image posts to the configured endpoint")
	}
}

func inlineBody(t *testing.T, entries ...string) string {
	t.Helper()
	items := make([]string, len(entries))
	for i, e := range entries {
		items[i] = fmt.Sprintf(`{"b64_json":%q}`, e)
	}
	return `{"data":[` + strings.Join(items, ",") + `]}`
}

func TestEveryReturnedImageIsSavedAndNIsForwarded(t *testing.T) {
	encoded := base64.StdEncoding.EncodeToString(pngBytes(t, 4, 3, color.RGBA{4, 5, 6, 255}))
	gw := startGateway(t, nil, []postResponse{{200, inlineBody(t, encoded, encoded, encoded)}})
	env := newTestService(t, gw.configure)
	assets, err := generate(t, env, ImageRequest{Prompt: "三张", Model: "gpt-image-2", Count: 3}, nil)
	if err != nil {
		t.Fatal(err)
	}
	ids := map[string]bool{}
	for _, a := range assets {
		ids[a.ID] = true
		if _, err := os.Stat(a.Path); err != nil {
			t.Fatal(err)
		}
	}
	if len(assets) != 3 || len(ids) != 3 {
		t.Fatalf("every returned image keeps its own asset: %v", assets)
	}
	if gw.postedJSON(t, 0)["n"] != float64(3) {
		t.Fatal("n must be forwarded")
	}
}

func TestOneUnusableEntryDoesNotDiscardTheRestOfAPaidBatch(t *testing.T) {
	encoded := base64.StdEncoding.EncodeToString(pngBytes(t, 4, 3, color.RGBA{7, 8, 9, 255}))
	gw := startGateway(t, nil, []postResponse{{200, inlineBody(t, encoded, "", encoded)}})
	env := newTestService(t, gw.configure)
	var partial []string
	assets, err := generate(t, env, ImageRequest{Prompt: "三张", Model: "gpt-image-2", Count: 3}, func(p ImageProgress) {
		if p.Stage == "partial" {
			partial = append(partial, p.Message)
		}
	})
	if err != nil || len(assets) != 2 {
		t.Fatalf("the two usable images must still be saved: %v, %v", assets, err)
	}
	if want := []string{"第 2/3 张不可用：模型未返回图像数据（可能不支持该参数，如透明背景）"}; !reflect.DeepEqual(partial, want) {
		t.Fatalf("partial = %v", partial)
	}
}

func TestUnusableResponsesReportTheirCause(t *testing.T) {
	cases := []struct {
		answer postResponse
		want   string
	}{
		{postResponse{200, inlineBody(t, base64.StdEncoding.EncodeToString([]byte("<html>")))}, "图像接口返回的内容不是支持的图片格式"},
		{postResponse{200, `{"data":[]}`}, "图像响应缺少 data"},
		{postResponse{200, `[1]`}, "图像响应缺少 data"},
		{postResponse{200, `{"data":[{"revised_prompt":"x"}]}`}, "图像响应中没有 b64_json 或 url"},
		{postResponse{200, inlineBody(t, "!!!")}, "base64 解码失败: "},
		{postResponse{200, `{"data":`}, "解析图像响应失败: "},
		{postResponse{500, "boom"}, "图像接口返回 500 Internal Server Error: boom"},
		{postResponse{400, strings.Repeat("错", 500)}, "图像接口返回 400 Bad Request: " + strings.Repeat("错", 400)},
	}
	for _, c := range cases {
		gw := startGateway(t, nil, []postResponse{c.answer})
		env := newTestService(t, gw.configure)
		_, err := generate(t, env, ImageRequest{Prompt: "x", Model: "gpt-image-2"}, nil)
		if err == nil || !strings.HasPrefix(err.Error(), c.want) || (strings.HasSuffix(c.want, "错") && err.Error() != c.want) {
			t.Errorf("%v: err = %v, want %q", c.answer.body[:min(len(c.answer.body), 20)], err, c.want)
		}
	}
}

func TestGenerationRequiresAConnectionAndAPrompt(t *testing.T) {
	env := newTestService(t, nil)
	if _, err := generate(t, env, ImageRequest{Prompt: "x"}, nil); err == nil || err.Error() != "未配置图像 API（请在设置中填写图像地址与 Key）" {
		t.Fatalf("err = %v", err)
	}
	gw := startGateway(t, nil, nil)
	env = newTestService(t, gw.configure)
	if _, err := generate(t, env, ImageRequest{Prompt: " \n"}, nil); err == nil || err.Error() != "提示词为空" {
		t.Fatalf("err = %v", err)
	}
	if len(gw.recorded()) != 0 {
		t.Fatal("nothing may be sent")
	}
}

func TestTheWaitForResponseHeadersIsBounded(t *testing.T) {
	gw := startGateway(t, nil, nil)
	gw.stall = 3 * time.Second
	env := newTestService(t, gw.configure)
	env.s.imageTimeouts.Header = 100 * time.Millisecond
	started := time.Now()
	_, err := generate(t, env, ImageRequest{Prompt: "x", Model: "gpt-image-2"}, nil)
	if err == nil || !strings.HasPrefix(err.Error(), "请求图像接口失败: 等待图像接口响应超过 ") || !strings.HasSuffix(err.Error(), " 秒") {
		t.Fatalf("err = %v", err)
	}
	if time.Since(started) > 2*time.Second {
		t.Fatal("the header wait must end the request")
	}
}

func TestLongPromptsAreForwardedVerbatim(t *testing.T) {
	gw := startGateway(t, pngBytes(t, 4, 3, color.RGBA{1, 2, 3, 255}), nil)
	env := newTestService(t, gw.configure)
	prompt := strings.Repeat("猫", 1500)
	if _, err := generate(t, env, ImageRequest{Prompt: prompt, Model: "grok-imagine-image"}, nil); err != nil {
		t.Fatal(err)
	}
	posted := gw.postedJSON(t, 0)
	if posted["prompt"] != prompt || posted["aspect_ratio"] != "1:1" {
		t.Fatal("prompt must be forwarded unchanged")
	}
}

func TestGenerationReportsEachStepToTheProgressSink(t *testing.T) {
	asset := pngBytes(t, 4, 3, color.RGBA{5, 6, 7, 255})
	gw := startGateway(t, asset, nil)
	env := newTestService(t, gw.configure)
	var events []ImageProgress
	if _, err := generate(t, env, ImageRequest{Prompt: "一只猫", Model: "gpt-image-2"}, func(p ImageProgress) { events = append(events, p) }); err != nil {
		t.Fatal(err)
	}
	var stages []string
	for _, e := range events {
		stages = append(stages, e.Stage)
	}
	if want := []string{"waiting", "receiving", "downloading", "saving"}; !reflect.DeepEqual(stages, want) {
		t.Fatalf("stages = %v", stages)
	}
	if events[0].Message != "提交文生图请求，等待模型生成" || events[1].Message != "模型已返回，正在接收结果" {
		t.Fatalf("events = %+v", events)
	}
	download := events[2]
	if download.Message != "下载第 1/1 张图片" || *download.Index != 1 || *download.Count != 1 ||
		*download.TotalBytes != int64(len(asset)) || *download.ReceivedBytes != 0 {
		t.Fatalf("download = %s", mustJSON(t, download))
	}
	if events[3].Message != "保存 1 张图片" {
		t.Fatalf("saving = %+v", events[3])
	}
}

func TestProgressEventsSerializeForTheFrontend(t *testing.T) {
	received, total, index, count := int64(10), int64(40), 2, 4
	event := ImageProgress{Stage: "downloading", Message: "下载第 2/4 张图片", ReceivedBytes: &received, TotalBytes: &total, Index: &index, Count: &count}
	want := `{"stage":"downloading","message":"下载第 2/4 张图片","receivedBytes":10,"totalBytes":40,"index":2,"count":4}`
	if got := mustJSON(t, event); got != want {
		t.Fatalf("event = %s", got)
	}
	if got := mustJSON(t, ImageProgress{Stage: "saving", Message: "保存 1 张图片"}); got != `{"stage":"saving","message":"保存 1 张图片"}` {
		t.Fatalf("step = %s", got)
	}
	payload := mustJSON(t, imageProgressPayload{ProgressID: "p", ImageProgress: ImageProgress{Stage: "saving", Message: "m"}})
	if payload != `{"progressId":"p","stage":"saving","message":"m"}` {
		t.Fatalf("payload = %s", payload)
	}
}

func TestTransferProgressIsThrottled(t *testing.T) {
	var events []ImageProgress
	sink := progressSink(func(p ImageProgress) { events = append(events, p) })
	onBytes := sink.transfer(ImageProgress{Stage: "receiving", Message: "m"})
	for i := range 100 {
		onBytes(int64(i))
	}
	if len(events) != 1 || events[0].ReceivedBytes != nil {
		t.Fatalf("byte updates within %v must be dropped: %d events", progressInterval, len(events))
	}
	if progressSink(nil).transfer(ImageProgress{}) != nil {
		t.Fatal("a nil sink needs no callback")
	}
}

// --- result downloads -------------------------------------------------------

func assetPayload() []byte {
	b := make([]byte, 4096)
	for i := range b {
		b[i] = byte(i)
	}
	return b
}

// rawResponse declares contentLength separately from body, so a step can
// promise more bytes than it sends and simulate a dropped connection.
func rawResponse(status string, headers []string, contentLength int, body []byte) []byte {
	head := fmt.Sprintf("HTTP/1.1 %s\r\nContent-Length: %d\r\nConnection: close\r\n", status, contentLength)
	for _, h := range headers {
		head += h + "\r\n"
	}
	return append([]byte(head+"\r\n"), body...)
}

type scriptStep struct {
	response []byte
	linger   time.Duration
}

// scriptedServer serves one step per connection: write the bytes, stay silent
// for linger, then close. It returns the URL and the request heads, lowercased.
func scriptedServer(t *testing.T, steps []scriptStep) (string, func() []string) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { listener.Close() })
	var mu sync.Mutex
	var heads []string
	go func() {
		for _, step := range steps {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			// Each connection gets its own goroutine: a lingering step must
			// not block the retry that follows it.
			go func() {
				defer conn.Close()
				reader := bufio.NewReader(conn)
				var head strings.Builder
				for {
					line, err := reader.ReadString('\n')
					if err != nil {
						return
					}
					head.WriteString(line)
					if line == "\r\n" {
						break
					}
				}
				mu.Lock()
				heads = append(heads, strings.ToLower(head.String()))
				mu.Unlock()
				conn.Write(step.response)
				time.Sleep(step.linger)
			}()
		}
	}()
	return "http://" + listener.Addr().String() + "/asset.png", func() []string {
		mu.Lock()
		defer mu.Unlock()
		return append([]string(nil), heads...)
	}
}

var firstSlot = imageSlot{index: 1, count: 1}

func download(t *testing.T, url string, sink progressSink) ([]byte, error) {
	t.Helper()
	env := newTestService(t, nil)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	return env.s.downloadImage(ctx, url, sink, firstSlot)
}

func TestAssetDownloadRetriesATransientStatus(t *testing.T) {
	asset := assetPayload()
	url, heads := scriptedServer(t, []scriptStep{
		{rawResponse("503 Service Unavailable", nil, 4, []byte("busy")), 0},
		{rawResponse("200 OK", nil, len(asset), asset), 0},
	})
	got, err := download(t, url, nil)
	if err != nil || !bytes.Equal(got, asset) {
		t.Fatalf("err = %v", err)
	}
	if len(heads()) != 2 {
		t.Fatalf("requests = %d", len(heads()))
	}
}

func TestAssetDownloadResumesFromReceivedBytesAfterADroppedConnection(t *testing.T) {
	asset := assetPayload()
	half := len(asset) / 2
	url, heads := scriptedServer(t, []scriptStep{
		{rawResponse("200 OK", []string{`ETag: "v1"`}, len(asset), asset[:half]), 0},
		{rawResponse("206 Partial Content", []string{fmt.Sprintf("Content-Range: bytes %d-%d/%d", half, len(asset)-1, len(asset))},
			len(asset)-half, asset[half:]), 0},
	})
	var events []ImageProgress
	got, err := download(t, url, func(p ImageProgress) { events = append(events, p) })
	if err != nil || !bytes.Equal(got, asset) {
		t.Fatalf("err = %v", err)
	}
	var retry, resumed *ImageProgress
	for i := range events {
		switch events[i].Stage {
		case "retrying":
			retry = &events[i]
		case "downloading":
			resumed = &events[i]
		}
	}
	if retry == nil || !strings.Contains(retry.Message, "续传（第 2/4 次）") || !strings.HasPrefix(retry.Message, "第 1/1 张下载中断：") ||
		*retry.ReceivedBytes != int64(half) {
		t.Fatalf("retry = %s", mustJSON(t, retry))
	}
	if *resumed.ReceivedBytes != int64(half) || *resumed.TotalBytes != int64(len(asset)) {
		t.Fatalf("resumed = %s", mustJSON(t, resumed))
	}
	requests := heads()
	if len(requests) != 2 || strings.Contains(requests[0], "range:") ||
		!strings.Contains(requests[1], fmt.Sprintf("range: bytes=%d-", half)) || !strings.Contains(requests[1], `if-range: "v1"`) {
		t.Fatalf("requests = %q", requests)
	}
}

func TestAssetDownloadResumesAfterAStalledTransfer(t *testing.T) {
	asset := assetPayload()
	half := len(asset) / 2
	url, heads := scriptedServer(t, []scriptStep{
		// Silent far beyond the idle timeout, well within the total ceiling.
		{rawResponse("200 OK", nil, len(asset), asset[:half]), 2 * time.Second},
		{rawResponse("206 Partial Content", []string{fmt.Sprintf("Content-Range: bytes %d-%d/%d", half, len(asset)-1, len(asset))},
			len(asset)-half, asset[half:]), 0},
	})
	started := time.Now()
	got, err := download(t, url, nil)
	if err != nil || !bytes.Equal(got, asset) {
		t.Fatalf("err = %v", err)
	}
	// The idle timeout, not the server closing the socket, ended attempt one.
	if time.Since(started) > time.Second {
		t.Fatalf("took %v", time.Since(started))
	}
	if !strings.Contains(heads()[1], fmt.Sprintf("range: bytes=%d-", half)) {
		t.Fatal("the retry must resume")
	}
}

func TestAssetDownloadRestartsCleanlyWhenTheServerIgnoresRange(t *testing.T) {
	asset := assetPayload()
	url, _ := scriptedServer(t, []scriptStep{
		{rawResponse("200 OK", nil, len(asset), asset[:len(asset)/2]), 0},
		{rawResponse("200 OK", nil, len(asset), asset), 0},
	})
	got, err := download(t, url, nil)
	if err != nil || !bytes.Equal(got, asset) {
		t.Fatalf("err = %v", err)
	}
}

func TestAssetDownloadDoesNotRetryAnExpiredURL(t *testing.T) {
	url, heads := scriptedServer(t, []scriptStep{
		{rawResponse("403 Forbidden", nil, 0, nil), 0},
		{rawResponse("403 Forbidden", nil, 0, nil), 0},
	})
	_, err := download(t, url, nil)
	if err == nil || !strings.HasPrefix(err.Error(), "下载图片失败: ") || !strings.Contains(err.Error(), "403") {
		t.Fatalf("err = %v", err)
	}
	if len(heads()) != 1 {
		t.Fatalf("requests = %d", len(heads()))
	}
}

func TestAssetDownloadGivesUpAfterTheAttemptBudget(t *testing.T) {
	var steps []scriptStep
	for range fastDownloadPolicy.Attempts {
		steps = append(steps, scriptStep{rawResponse("503 Service Unavailable", nil, 0, nil), 0})
	}
	url, heads := scriptedServer(t, steps)
	_, err := download(t, url, nil)
	if err == nil || !strings.Contains(err.Error(), "已尝试 4 次") {
		t.Fatalf("err = %v", err)
	}
	if len(heads()) != 4 {
		t.Fatalf("requests = %d", len(heads()))
	}
}

func TestAnOversizedInlineResponseNamesTheLimit(t *testing.T) {
	_, err := readBody(&http.Response{Body: io.NopCloser(strings.NewReader("123456")), ContentLength: -1}, 4, "图像接口响应", nil)
	if err == nil || err.Error() != "图像接口响应超过大小限制（0 KiB）" {
		t.Fatalf("err = %v", err)
	}
	if limitLabel(400<<20) != "400 MiB" || limitLabel(1<<10) != "1 KiB" {
		t.Fatal(limitLabel(400 << 20))
	}
}
