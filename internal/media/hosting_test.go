package media

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
)

func TestPublicHTTPSURL(t *testing.T) {
	for _, value := range []string{
		"http://example.com/upload",
		"https://localhost/upload",
		"https://api.localhost/upload",
		"https://nas.local/upload",
		"https://svc.internal/upload",
		"https://router.lan/upload",
		"https://intranet/upload",
		"https://127.0.0.1/upload",
		"https://10.1.2.3/upload",
		"https://172.16.0.1/upload",
		"https://192.168.1.1/upload",
		"https://169.254.169.254/latest",
		"https://0.0.0.0/upload",
		"https://224.0.0.1/upload",
		"https://255.255.255.255/upload",
		"https://[::1]/upload",
		"https://[2001:db8::1]/upload",
		"https://100.64.0.1/upload",
		"https://100.127.255.255/upload",
		"https://user@example.com/upload",
		"https://user:pass@example.com/upload",
		"https://0x7f.1/upload",     // 127.0.0.1 in URL-standard notation
		"https://2130706433/upload", // 127.0.0.1 as one number
		"https://example.0300/upload",
		"",
		"not a url",
	} {
		if publicHTTPSURL(value) {
			t.Errorf("%q must be rejected", value)
		}
	}
	for _, value := range []string{
		"https://cdn.example.com/upload",
		" https://CDN.Example.com/upload?x=1 ",
		"https://8.8.8.8/upload",
		"https://100.128.0.1/upload",
		"https://example.com./upload",
	} {
		if !publicHTTPSURL(value) {
			t.Errorf("%q must be accepted", value)
		}
	}
}

func hostingInput(endpoint string) hostingSaveInput {
	token := "secret-token"
	return hostingSaveInput{Endpoint: endpoint, FileField: "file", URLField: "payload.url", AuthMode: authBearer, Token: &token}
}

func TestHostingSaveKeepsTheTokenInAppSecretsOnly(t *testing.T) {
	env := newTestEnv(t)
	ctx := context.Background()
	status, err := env.svc.hostingConfig(ctx)
	if err != nil || status.status() != (hostingStatus{FileField: "file", URLField: "url", AuthMode: authBearer}) {
		t.Fatalf("defaults: %+v %v", status, err)
	}
	saved, err := env.svc.saveHosting(ctx, hostingInput(" https://first.example.com/upload "))
	if err != nil || !saved.HasToken || !saved.Configured || saved.Endpoint != "https://first.example.com/upload" {
		t.Fatalf("first save: %+v %v", saved, err)
	}
	stored := env.queryText(t, `SELECT value FROM app_secrets WHERE key='media_hosting.v1'`)
	if len(stored) != 1 || stored[0] != `{"endpoint":"https://first.example.com/upload","fileField":"file","urlField":"payload.url","authMode":"bearer","token":"secret-token"}` {
		t.Fatalf("stored %v", stored)
	}
	if rows := env.queryText(t, `SELECT COUNT(*) FROM settings WHERE key='media_hosting.v1'`); rows[0] != "0" {
		t.Fatal("the hosting secret must not be in settings")
	}

	// Same host, no token: the stored token is kept.
	same := hostingInput("https://FIRST.example.com/other")
	same.Token = nil
	if saved, err := env.svc.saveHosting(ctx, same); err != nil || !saved.HasToken {
		t.Fatalf("same host: %+v %v", saved, err)
	}
	// Another host with an empty token drops it.
	changed := hostingInput("https://second.example.com/upload")
	empty := ""
	changed.Token = &empty
	saved, err = env.svc.saveHosting(ctx, changed)
	if err != nil || saved.HasToken {
		t.Fatalf("changed host: %+v %v", saved, err)
	}
	encoded, _ := json.Marshal(saved)
	if strings.Contains(string(encoded), "secret-token") || strings.Contains(string(encoded), `"token"`) {
		t.Fatalf("status leaks the token: %s", encoded)
	}
	// An explicit clear drops a token even on the same host.
	restored, _ := env.svc.saveHosting(ctx, hostingInput("https://second.example.com/upload"))
	clear := hostingInput("https://second.example.com/upload")
	clear.Token, clear.ClearToken = nil, true
	if saved, err := env.svc.saveHosting(ctx, clear); err != nil || !restored.HasToken || saved.HasToken {
		t.Fatalf("clear: %+v %v", saved, err)
	}

	for _, tc := range []struct {
		mutate func(*hostingSaveInput)
		want   string
	}{
		{func(in *hostingSaveInput) { in.Endpoint = "http://example.com/upload" }, errHostingEndpointPublic},
		{func(in *hostingSaveInput) { in.FileField = "bad field" }, "文件字段无效"},
		{func(in *hostingSaveInput) { in.URLField = "data..url" }, "地址字段无效"},
		{func(in *hostingSaveInput) { in.URLField = "" }, "地址字段无效"},
	} {
		in := hostingInput("https://cdn.example.com/upload")
		tc.mutate(&in)
		if _, err := env.svc.saveHosting(ctx, in); errText(err) != tc.want {
			t.Errorf("error %q, want %q", errText(err), tc.want)
		}
	}

	env.exec(t, `UPDATE app_secrets SET value='{"endpoint":"https://x.example.com"}' WHERE key='media_hosting.v1'`)
	if _, err := env.svc.hostingConfig(ctx); errText(err) != "媒体托管设置无效，请重新保存" {
		t.Fatalf("incomplete stored config: %v", err)
	}
}

func publishAsset(env testEnv, t *testing.T, id, project, kind string, data []byte) string {
	path := writeFile(t, filepath.Join(env.output, id+".png"), data)
	env.exec(t, `INSERT INTO assets(id,kind,path,created_at,metadata) VALUES(?,?,?,1,?)`, id, kind, path, `{"projectId":"`+project+`"}`)
	return path
}

func TestPublishRejectsMissingConfigForeignProjectsAndNonPublicEndpoints(t *testing.T) {
	env := newTestEnv(t)
	ctx := context.Background()
	request := publishRequest{ProjectID: "project-a", ExpectedEndpoint: "https://upload.example.com/media",
		Sources: []publishSource{{AssetID: "asset-a"}}}
	if _, err := env.svc.publishMedia(ctx, request); errText(err) != errHostingEndpointPublic {
		t.Fatalf("unconfigured: %v", err)
	}
	publishAsset(env, t, "asset-a", "project-b", "image", pngBytes(t, testImage(1, 1)))
	if _, err := env.svc.saveHosting(ctx, hostingInput("https://upload.example.com/media")); err != nil {
		t.Fatal(err)
	}
	if _, err := env.svc.publishMedia(ctx, request); errText(err) != "媒体资产不属于当前项目" {
		t.Fatalf("foreign project: %v", err)
	}
	env.exec(t, `UPDATE assets SET metadata='{"projectId":"project-a"}' WHERE id='asset-a'`)
	for _, endpoint := range []string{"http://example.com/upload", "https://localhost/upload"} {
		env.exec(t, `UPDATE app_secrets SET value=? WHERE key='media_hosting.v1'`,
			`{"endpoint":"`+endpoint+`","fileField":"file","urlField":"url","authMode":"bearer","token":"x"}`)
		request.ExpectedEndpoint = endpoint
		if _, err := env.svc.publishMedia(ctx, request); errText(err) != errHostingEndpointPublic {
			t.Fatalf("%s: %v", endpoint, err)
		}
	}
}

func TestPublishRejectsAnEndpointThatChangedAfterThePreview(t *testing.T) {
	env := newTestEnv(t)
	ctx := context.Background()
	if _, err := env.svc.saveHosting(ctx, hostingInput("https://upload.example.com/current")); err != nil {
		t.Fatal(err)
	}
	_, err := env.svc.publishMedia(ctx, publishRequest{ProjectID: "project-a", ExpectedEndpoint: "https://upload.example.com/previewed",
		Sources: []publishSource{{AssetID: "not-read"}}})
	if errText(err) != "媒体托管配置已变更，请重新确认后再发布" {
		t.Fatalf("error = %v", err)
	}
}

func TestResolveSourcesValidatesEverySource(t *testing.T) {
	env := newTestEnv(t)
	ctx := context.Background()
	png := pngBytes(t, testImage(1, 1))
	publishAsset(env, t, "img", "p", "image", png)
	publishAsset(env, t, "txt", "p", "text", []byte("text"))
	publishAsset(env, t, "fake", "p", "video", png)
	comicImage := writeFile(t, filepath.Join(env.output, "comic", "page1.png"), png)
	env.svc.comicCatalog = func(_ context.Context, projectID string) ([]ComicMedia, error) {
		if projectID != "p" {
			return nil, nil
		}
		return []ComicMedia{
			{SourceURI: "comic-md://p/w/ch1/image/1", Kind: "image", Path: comicImage},
			{SourceURI: "comic-md://p/w/ch1/document/2@1", Kind: "text"},
		}, nil
	}
	cases := []struct {
		sources []publishSource
		want    string
	}{
		{nil, "每次只能发布 1 至 8 个媒体文件"},
		{make([]publishSource, 9), "每次只能发布 1 至 8 个媒体文件"},
		{[]publishSource{{}}, "每个媒体来源必须且只能提供 assetId 或 sourceUri"},
		{[]publishSource{{AssetID: "img", SourceURI: "comic-md://x"}}, "每个媒体来源必须且只能提供 assetId 或 sourceUri"},
		{[]publishSource{{AssetID: "missing"}}, "媒体资产不存在"},
		{[]publishSource{{AssetID: "txt"}}, "只能发布图片或视频资产"},
		{[]publishSource{{AssetID: "fake"}}, "媒体文件格式与资产类型不匹配"},
		{[]publishSource{{SourceURI: "comic-md://p/w/ch1/image/9"}}, "漫画媒体来源不存在或不属于当前项目"},
		{[]publishSource{{SourceURI: "comic-md://p/w/ch1/document/2@1"}}, "漫画来源不是可发布的媒体"},
	}
	for _, tc := range cases {
		if _, err := env.svc.resolveSources(ctx, publishRequest{ProjectID: "p", Sources: tc.sources}); errText(err) != tc.want {
			t.Errorf("%+v: error %q, want %q", tc.sources, errText(err), tc.want)
		}
	}
	if _, err := env.svc.resolveSources(ctx, publishRequest{ProjectID: " ", Sources: []publishSource{{AssetID: "img"}}}); errText(err) != "项目不能为空" {
		t.Errorf("blank project: %v", err)
	}
	resolved, err := env.svc.resolveSources(ctx, publishRequest{ProjectID: "p",
		Sources: []publishSource{{AssetID: "img"}, {SourceURI: " comic-md://p/w/ch1/image/1 "}}})
	if err != nil {
		t.Fatal(err)
	}
	if resolved[0].key != "asset:img" || resolved[0].mime != "image/png" || resolved[0].fileName != "img.png" ||
		resolved[1].key != "comic:comic-md://p/w/ch1/image/1" || resolved[1].fileName != "page1.png" || string(resolved[1].data) != string(png) {
		t.Fatalf("resolved %+v", resolved)
	}
}

func TestUploadSendsMultipartAndKeepsTheTokenPrivate(t *testing.T) {
	var auth, contentType string
	var body []byte
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		auth, contentType = r.Header.Get("Authorization"), r.Header.Get("Content-Type")
		body, _ = io.ReadAll(r.Body)
		w.WriteHeader(http.StatusCreated)
		w.Write([]byte(`{"payload":{"url":"https://cdn.example.com/a.png"}}`))
	}))
	defer server.Close()
	cfg := hostingConfig{Endpoint: server.URL, FileField: "media", URLField: "payload.url", AuthMode: authBearer, Token: "secret-token"}
	output := publish(context.Background(), cfg, []resolvedSource{{key: "asset:a", fileName: "upload.png", mime: "image/png", data: []byte("test-image")}}, true)

	if auth != "Bearer secret-token" || !strings.HasPrefix(contentType, "multipart/form-data; boundary=") {
		t.Fatalf("headers: %q %q", auth, contentType)
	}
	for _, want := range []string{`name="media"`, `filename="upload.png"`, "Content-Type: image/png", "test-image"} {
		if !strings.Contains(string(body), want) {
			t.Errorf("body lacks %q:\n%s", want, body)
		}
	}
	result := output.Results[0]
	digest := sha256.Sum256([]byte("test-image"))
	if result.Key != "asset:a" || result.URL != "https://cdn.example.com/a.png" || result.Error != "" ||
		result.SHA256 != hex.EncodeToString(digest[:]) {
		t.Fatalf("result %+v", result)
	}
	encoded, _ := json.Marshal(output)
	if strings.Contains(string(encoded), "secret-token") || strings.Contains(string(encoded), `"error"`) {
		t.Fatalf("output %s", encoded)
	}
}

func TestUploadReportsPerSourceFailures(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch calls.Add(1) {
		case 1:
			w.Write([]byte(`{"url":"https://cdn.example.com/ok.png"}`))
		case 2:
			w.Write([]byte(`{"url":"https://[::1]/nope.png"}`))
		case 3:
			http.Redirect(w, r, "https://cdn.example.com/elsewhere", http.StatusFound)
		case 4:
			w.Write([]byte(`{"data":{}}`))
		case 5:
			w.Write([]byte(`not json`))
		case 6:
			w.Write(make([]byte, maxHostingResponseBytes+1))
		default:
			w.WriteHeader(http.StatusInternalServerError)
		}
	}))
	defer server.Close()
	cfg := hostingConfig{Endpoint: server.URL, FileField: "file", URLField: "url", AuthMode: authRaw}
	sources := make([]resolvedSource, 7)
	for i := range sources {
		sources[i] = resolvedSource{key: "asset:" + string(rune('1'+i)), fileName: "p.png", mime: "image/png", data: []byte("x")}
	}
	output := publish(context.Background(), cfg, sources, true)
	want := []string{"", "媒体托管返回了无效地址", "媒体托管服务返回失败状态", "媒体托管响应未包含媒体地址",
		"媒体托管响应格式无效", "媒体托管响应过大", "媒体托管服务返回失败状态"}
	for i, result := range output.Results {
		if result.Error != want[i] || (want[i] == "") != (result.URL != "") {
			t.Errorf("source %d: %+v, want error %q", i+1, result, want[i])
		}
	}

	// Production uploads re-check the endpoint, and a raw token must be a
	// valid header value.
	if _, _, err := uploadOne(context.Background(), http.DefaultClient, cfg, sources[0], false); errText(err) != "媒体托管端点无效" {
		t.Fatalf("loopback endpoint in production: %v", err)
	}
	cfg.Token = "bad\ntoken"
	if _, _, err := uploadOne(context.Background(), http.DefaultClient, cfg, sources[0], true); errText(err) != "媒体托管凭据无效" {
		t.Fatalf("invalid token: %v", err)
	}
}
