package config

import (
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"imageclient/internal/platform"
)

// isolateEnv blanks every variable Load can read. The repository .env is
// loaded by findEnvFile, and godotenv never overrides a variable that is already
// set, so both the hyphen and underscore spellings must be pinned to "".
func isolateEnv(t *testing.T, overrides ...string) {
	t.Helper()
	names := []string{"image-api-url", "image-api-key", "image-api-model", "video-api-url", "video-api-key",
		"video-api-model", "llm-api-url", "llm-api-key", "llm-api-model", "output-dir"}
	for _, name := range names {
		t.Setenv(name, "")
		t.Setenv(strings.ToUpper(strings.ReplaceAll(name, "-", "_")), "")
	}
	for i := 0; i+1 < len(overrides); i += 2 {
		t.Setenv(overrides[i], overrides[i+1])
	}
}

func newTestStore(t *testing.T, overrides ...string) (*Store, platform.Paths) {
	t.Helper()
	isolateEnv(t, overrides...)
	paths := platform.Paths{Root: t.TempDir()}
	return Load(paths), paths
}

func TestModelsEndpoint(t *testing.T) {
	cases := map[string]string{
		"https://host/v1/images/generations": "https://host/v1/models",
		"https://host/v1/":                   "https://host/v1/models",
		"https://host/api/v1/chat":           "https://host/api/v1/models",
		// A bare base URL is handled by ModelsEndpointFor, not here.
		"https://host":    "",
		"https://host/v1": "",
		"ftp://x":         "",
		"":                "",
		"garbage":         "",
	}
	for in, want := range cases {
		if got := ModelsEndpoint(in); got != want {
			t.Errorf("ModelsEndpoint(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestModelsEndpointFor(t *testing.T) {
	cases := map[string]string{
		"https://host/v1/images/generations": "https://host/v1/models",
		"https://host/v1/":                   "https://host/v1/models",
		"https://host/v1":                    "https://host/v1/models",
		"https://host":                       "https://host/v1/models",
		"https://host/":                      "https://host/v1/models",
		"  https://host/v1  ":                "https://host/v1/models",
		"https://host/v1?key=1#frag":         "https://host/v1/models",
		"https://host/base?key=1#frag":       "https://host/base/v1/models",
		"http://127.0.0.1:8123":              "http://127.0.0.1:8123/v1/models",
		"ftp://x":                            "",
		"":                                   "",
		"not a url at all":                   "",
	}
	for in, want := range cases {
		if got := ModelsEndpointFor(in); got != want {
			t.Errorf("ModelsEndpointFor(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestStoreSavePersistsSnakeCaseSnapshot(t *testing.T) {
	st, paths := newTestStore(t)
	status, err := st.Save(SaveRequest{
		ImageAPIURL: "https://img.example.com/v1/images/generations", ImageAPIKey: "img-secret-123", ImageAPIModel: "gpt-image-2",
	}, "db")
	if err != nil {
		t.Fatalf("Save: %v", err)
	}
	if !status.ImageReady || status.VideoReady {
		t.Fatalf("status readiness = image %v video %v", status.ImageReady, status.VideoReady)
	}

	data, err := os.ReadFile(paths.BackendConfig())
	if err != nil {
		t.Fatalf("snapshot not written: %v", err)
	}
	var raw map[string]any
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatal(err)
	}
	for key, want := range map[string]string{
		"image_api_url": "https://img.example.com/v1/images/generations",
		"image_api_key": "img-secret-123",
		"image_model":   "gpt-image-2",
		"source":        "db",
	} {
		if raw[key] != want {
			t.Errorf("snapshot[%q] = %v, want %q", key, raw[key], want)
		}
	}

	reloaded := Load(paths).Get()
	if reloaded.ImageAPIURL != "https://img.example.com/v1/images/generations" || reloaded.ImageAPIKey != "img-secret-123" {
		t.Fatalf("reloaded settings = %+v", reloaded)
	}
}

func TestStatusHidesKeysAndReportsReadiness(t *testing.T) {
	st, _ := newTestStore(t)
	if _, err := st.Save(SaveRequest{VideoAPIURL: "https://vid.example.com", VideoAPIKey: "vid-secret-xyz"}, "db"); err != nil {
		t.Fatal(err)
	}
	status := st.Status()
	if status.ImageAPIURL != nil {
		t.Fatalf("image url must be null when unset, got %q", *status.ImageAPIURL)
	}
	if !status.VideoReady || status.ImageReady || status.LLMReady {
		t.Fatalf("readiness = image %v video %v llm %v", status.ImageReady, status.VideoReady, status.LLMReady)
	}
	body, err := json.Marshal(status)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(body), "vid-secret-xyz") || strings.Contains(string(body), "apiKey") {
		t.Fatalf("status leaks credentials: %s", body)
	}
}

func TestStatusNullsEmptyURLs(t *testing.T) {
	st, _ := newTestStore(t)
	body, err := json.Marshal(st.Status())
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(body), `"imageApiUrl":null`) || !strings.Contains(string(body), `"videoApiUrl":null`) {
		t.Fatalf("empty URLs must serialise as null: %s", body)
	}
}

func TestLoadFallsBackToEnvWithDefaults(t *testing.T) {
	st, _ := newTestStore(t, "image-api-url", "https://env-img.example.com", "image-api-key", "env-key")
	cur := st.Get()
	if cur.ImageAPIURL != "https://env-img.example.com" || cur.ImageAPIKey != "env-key" {
		t.Fatalf("env image settings = %q / %q", cur.ImageAPIURL, cur.ImageAPIKey)
	}
	if cur.ImageModel != "gpt-image-2" || cur.VideoModel != "kling-video-v3" || cur.LLMModel != "gemini-3.7-flash" {
		t.Fatalf("default models = %q %q %q", cur.ImageModel, cur.VideoModel, cur.LLMModel)
	}
	if cur.Source != "env" {
		t.Fatalf("source = %q, want env", cur.Source)
	}
}

func TestLoadIgnoresInvalidSnapshot(t *testing.T) {
	isolateEnv(t)
	paths := platform.Paths{Root: t.TempDir()}
	if err := os.WriteFile(paths.BackendConfig(), []byte("{not json"), 0o600); err != nil {
		t.Fatal(err)
	}
	cur := Load(paths).Get()
	if cur.ImageAPIURL != "" || cur.Source != "none" {
		t.Fatalf("invalid snapshot must not be applied: %+v", cur)
	}
}

func TestOutputPathResolution(t *testing.T) {
	st, paths := newTestStore(t)
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	foreign := "C:\\Users\\other\\out"
	if runtime.GOOS == "windows" {
		foreign = "/home/other/out"
	}

	cases := []struct {
		name string
		dir  string
		want string
	}{
		{"empty means default assets", "", paths.Assets()},
		{"default marker", DefaultOutputDir, paths.Assets()},
		{"assets dir itself", paths.Assets(), paths.Assets()},
		{"relative under data root", "exports/out", filepath.Join(paths.Root, "exports", "out")},
		{"tilde slash under user home", "~/renders", filepath.Join(home, "renders")},
		{"tilde backslash under user home", `~\renders`, filepath.Join(home, "renders")},
		{"foreign absolute path falls back", foreign, paths.Assets()},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := st.OutputPath(Settings{OutputDir: tc.dir}); got != tc.want {
				t.Fatalf("OutputPath(%q) = %q, want %q", tc.dir, got, tc.want)
			}
		})
	}

	native := filepath.Join(t.TempDir(), "native-out")
	if got := st.OutputPath(Settings{OutputDir: native}); got != native {
		t.Fatalf("native absolute path = %q, want %q", got, native)
	}
}

func TestSaveNormalisesOutputDirOnDisk(t *testing.T) {
	st, paths := newTestStore(t)
	if _, err := st.Save(SaveRequest{OutputDir: paths.Assets()}, "db"); err != nil {
		t.Fatal(err)
	}
	if got := st.Get().OutputDir; got != DefaultOutputDir {
		t.Fatalf("assets dir stored as %q, want the default marker", got)
	}
	if got := st.Status().OutputDir; got != paths.Assets() {
		t.Fatalf("status output dir = %q, want %q", got, paths.Assets())
	}
}

func TestSaveRefusedChangeLeavesSettingsAndSnapshotUnchanged(t *testing.T) {
	st, paths := newTestStore(t)
	if _, err := st.Save(SaveRequest{ImageAPIURL: "https://img.example.com", ImageAPIKey: "keep-me"}, "db"); err != nil {
		t.Fatal(err)
	}
	before, _ := os.ReadFile(paths.BackendConfig())

	_, err := st.Save(SaveRequest{ImageAPIURL: "https://elsewhere.example.com"}, "api")
	if err == nil || !strings.Contains(err.Error(), "已变更") {
		t.Fatalf("err = %v, want 已变更", err)
	}
	if got := st.Get(); got.ImageAPIURL != "https://img.example.com" || got.ImageAPIKey != "keep-me" {
		t.Fatalf("refused save changed live settings: %+v", got)
	}
	after, _ := os.ReadFile(paths.BackendConfig())
	if string(before) != string(after) {
		t.Fatal("refused save rewrote the snapshot")
	}
}
