package gen

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"reflect"
	"testing"

	"imageclient/internal/config"
)

func TestReusesAStoredKeyOnlyForAnUnchangedAddress(t *testing.T) {
	storedURL, storedKey := "https://gw.example.com/v1/images/generations", "sk-stored"
	// The form carries the key: it always wins.
	if url, key, err := catalogCredentials("https://other.example.com/v1", "sk-typed", storedURL, storedKey); err != nil ||
		url != "https://other.example.com/v1" || key != "sk-typed" {
		t.Fatal(url, key, err)
	}
	// Blank key + same address (trailing slash tolerated) reuses the stored pair.
	if url, key, err := catalogCredentials(" https://gw.example.com/v1/images/generations/ ", "", storedURL, storedKey); err != nil ||
		url != "https://gw.example.com/v1/images/generations/" || key != storedKey {
		t.Fatal(url, key, err)
	}
	// Blank address falls back to the stored one.
	if url, _, err := catalogCredentials("", "", storedURL, storedKey); err != nil || url != storedURL {
		t.Fatal(url, err)
	}
	for _, c := range []struct{ url, key, storedURL, storedKey, want string }{
		{"https://evil.example.com/v1", "", storedURL, storedKey, "当前地址与已保存地址不同：请填写该地址对应的 API Key，不能沿用已保存的旧 Key"},
		{"", "", "", "", "请先填写 API 地址"},
		{"https://gw.example.com/v1", "", storedURL, "", "请先填写 API Key"},
	} {
		if _, _, err := catalogCredentials(c.url, c.key, c.storedURL, c.storedKey); err == nil || err.Error() != c.want {
			t.Errorf("err = %v, want %s", err, c.want)
		}
	}
	if _, _, err := storedConnection(config.Settings{}, "audio"); err == nil || err.Error() != "未知的模型类型: audio" {
		t.Fatalf("err = %v", err)
	}
}

func TestReadsModelIDsFromOpenAIAndLooseCatalogs(t *testing.T) {
	for raw, want := range map[string][]string{
		`{"data":[{"id":"gpt-image-2"},{"id":"kling-video-v3"},{"id":"gpt-image-2"}]}`: {"gpt-image-2", "kling-video-v3"},
		`{"models":[{"name":"gemini-3.7-flash"},"text-embedding-3"]}`:                  {"gemini-3.7-flash", "text-embedding-3"},
		`[" a ",{"id":7,"name":"b"},{"id":""},null]`:                                   {"a", "b"},
		`{"error":"unauthorized"}`:                                                     {},
	} {
		catalog, _ := decodeJSON([]byte(raw))
		if got := parseModelIDs(catalog); !reflect.DeepEqual(got, want) {
			t.Errorf("%s: %v", raw, got)
		}
	}
}

// catalogServer serves GET /v1/models.
func catalogServer(t *testing.T, status int, body string, auth *string) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/models" {
			http.NotFound(w, r)
			return
		}
		if auth != nil {
			*auth = r.Header.Get("Authorization")
		}
		w.WriteHeader(status)
		io.WriteString(w, body)
	}))
	t.Cleanup(server.Close)
	return server
}

func TestFetchModelsReadsTheCatalogForTheTypedConnection(t *testing.T) {
	var auth string
	server := catalogServer(t, 200, `{"data":[{"id":" a "},{"id":"a"},{"name":"b"}]}`, &auth)
	env := newTestService(t, nil)
	models, err := env.s.FetchModels(context.Background(), server.URL, "typed-key", "image")
	if err != nil || !reflect.DeepEqual(models, []string{"a", "b"}) || auth != "Bearer typed-key" {
		t.Fatalf("models = %v, err = %v, auth = %q", models, err, auth)
	}

	for _, c := range []struct {
		status       int
		body, prefix string
	}{
		{401, "", "模型目录返回 401 Unauthorized，请检查 API Key、权限和地址："},
		{200, `{"data":[]}`, "模型目录为空，请确认该地址是 OpenAI 兼容网关："},
	} {
		server := catalogServer(t, c.status, c.body, nil)
		want := c.prefix + server.URL + "/v1/models"
		if _, err := env.s.FetchModels(context.Background(), server.URL+"/v1", "k", "llm"); err == nil || err.Error() != want {
			t.Errorf("err = %v, want %s", err, want)
		}
	}
	if _, err := env.s.FetchModels(context.Background(), "ftp://x.example", "k", "video"); err == nil ||
		err.Error() != "无法从地址推导模型目录（需要 http(s) 地址）: ftp://x.example" {
		t.Fatalf("err = %v", err)
	}
	if _, err := env.s.FetchModels(context.Background(), server.URL, "", "video"); err == nil || err.Error() != "请先填写 API Key" {
		t.Fatalf("err = %v", err)
	}
}

func TestVideoModelsTrustAConfiguredService(t *testing.T) {
	env := newTestService(t, nil)
	models, err := env.s.VideoModels(context.Background())
	if err != nil || !reflect.DeepEqual(models, config.VideoModels) {
		t.Fatal("an unconfigured service lists the offline catalogue")
	}
	models[0] = "mutated"
	if config.VideoModels[0] == "mutated" {
		t.Fatal("callers get a copy")
	}

	var auth string
	server := catalogServer(t, 200, `{"data":[{"id":"kling-video-v3"},{"name":"ignored"}]}`, &auth)
	env = newTestService(t, func(s *config.Settings) { s.VideoAPIURL, s.VideoAPIKey = server.URL+"/v1/videos", "video-key" })
	models, err = env.s.VideoModels(context.Background())
	if err != nil || !reflect.DeepEqual(models, []string{"kling-video-v3"}) || auth != "Bearer video-key" {
		t.Fatalf("models = %v, err = %v", models, err)
	}
	for _, c := range []struct {
		status int
		body   string
		want   string
	}{
		{403, "", "视频模型目录返回 403 Forbidden，请检查视频 API Key、权限和 Base URL"},
		{200, `{"data":[]}`, "视频模型目录为空，请确认使用的是视频 API Key，并检查权限和 Base URL"},
		{200, `nope`, "解析视频模型目录失败: invalid character 'o' in literal null (expecting 'u')"},
	} {
		server := catalogServer(t, c.status, c.body, nil)
		env := newTestService(t, func(s *config.Settings) { s.VideoAPIURL, s.VideoAPIKey = server.URL+"/v1/videos", "k" })
		if _, err := env.s.VideoModels(context.Background()); err == nil || err.Error() != c.want {
			t.Errorf("err = %v, want %s", err, c.want)
		}
	}
}

func TestImageModelsFallBackToTheKnownCatalogue(t *testing.T) {
	env := newTestService(t, nil)
	if got := env.s.ImageModels(context.Background()); !reflect.DeepEqual(got, config.ImageModels) {
		t.Fatalf("models = %v", got)
	}
	server := catalogServer(t, 200, `{"data":[{"id":"gpt-image-2"},{"id":"grok-imagine-image"}]}`, nil)
	env = newTestService(t, func(s *config.Settings) { s.ImageAPIURL, s.ImageAPIKey = server.URL+"/v1/images/generations", "k" })
	if got := env.s.ImageModels(context.Background()); !reflect.DeepEqual(got, []string{"gpt-image-2", "grok-imagine-image"}) {
		t.Fatalf("models = %v", got)
	}
	for _, c := range []struct {
		status int
		body   string
	}{{500, ""}, {200, `{"data":[]}`}, {200, "nope"}} {
		server := catalogServer(t, c.status, c.body, nil)
		env := newTestService(t, func(s *config.Settings) { s.ImageAPIURL, s.ImageAPIKey = server.URL+"/v1/images/generations", "k" })
		if got := env.s.ImageModels(context.Background()); !reflect.DeepEqual(got, config.ImageModels) {
			t.Errorf("%d %s: models = %v", c.status, c.body, got)
		}
	}
	encoded, _ := json.Marshal(env.s.ImageModels(context.Background()))
	if string(encoded) == "null" {
		t.Fatal("lists are never null")
	}
}
