package config

import (
	"strings"
	"testing"
)

func TestSaveRequestValidate(t *testing.T) {
	cases := []struct {
		name    string
		req     SaveRequest
		wantErr string // substring; empty means valid
	}{
		{"empty request is valid", SaveRequest{}, ""},
		{"https endpoints are valid and trimmed", SaveRequest{
			ImageAPIURL: " https://api.example.com/v1/images ",
			VideoAPIURL: "http://127.0.0.1:9000",
			LLMAPIURL:   "https://llm.example.com",
		}, ""},
		{"ftp scheme rejected", SaveRequest{ImageAPIURL: "ftp://files.example.com"}, "image_api_url 必须是 HTTP(S) 地址"},
		{"scheme without host rejected", SaveRequest{VideoAPIURL: "https://"}, "video_api_url 必须是 HTTP(S) 地址"},
		{"malformed URL rejected with field", SaveRequest{LLMAPIURL: "http://[::1"}, "llm_api_url 地址无效"},
		{"model at 200 runes accepted", SaveRequest{ImageAPIModel: strings.Repeat("中", 200)}, ""},
		{"model over 200 runes rejected", SaveRequest{ImageAPIModel: strings.Repeat("m", 201)}, "image_api_model 超过 200 个字符"},
		{"video model over 200 runes rejected", SaveRequest{VideoAPIModel: strings.Repeat("m", 201)}, "video_api_model 超过 200 个字符"},
		{"output dir at 4096 accepted", SaveRequest{OutputDir: strings.Repeat("d", 4096)}, ""},
		{"output dir over 4096 rejected", SaveRequest{OutputDir: strings.Repeat("d", 4097)}, "output_dir 超过 4096 个字符"},
		{"key at 16384 accepted", SaveRequest{LLMAPIKey: strings.Repeat("k", 16384)}, ""},
		{"key over 16384 rejected", SaveRequest{LLMAPIKey: strings.Repeat("k", 16385)}, "llm_api_key 超过 16384 个字符"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.req.Validate()
			if tc.wantErr == "" {
				if err != nil {
					t.Fatalf("Validate: %v", err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("Validate err = %v, want containing %q", err, tc.wantErr)
			}
		})
	}
}

func mergeBase() Settings {
	return Settings{
		ImageAPIURL: "https://img.example.com", ImageAPIKey: "img-key", ImageModel: "gpt-image-2",
		VideoAPIURL: "https://vid.example.com", VideoAPIKey: "vid-key", VideoModel: "kling-video-v3",
		LLMAPIURL: "https://llm.example.com", LLMAPIKey: "llm-key", LLMModel: "gemini-3.7-flash",
		OutputDir: "exports", Source: "db",
	}
}

func TestSaveRequestMerge(t *testing.T) {
	t.Run("empty fields keep stored values", func(t *testing.T) {
		cur := mergeBase()
		next, err := SaveRequest{}.Merge(cur)
		if err != nil {
			t.Fatal(err)
		}
		if next != cur {
			t.Fatalf("merge of empty request changed settings: %+v", next)
		}
	})

	t.Run("values are trimmed", func(t *testing.T) {
		next, err := SaveRequest{
			ImageAPIModel: "  new-model  ",
			OutputDir:     "  /abs/out  ",
			LLMAPIKey:     "  llm-2  ",
		}.Merge(mergeBase())
		if err != nil {
			t.Fatal(err)
		}
		if next.ImageModel != "new-model" || next.OutputDir != "/abs/out" || next.LLMAPIKey != "llm-2" {
			t.Fatalf("trimmed values = model %q dir %q key %q", next.ImageModel, next.OutputDir, next.LLMAPIKey)
		}
	})

	t.Run("whitespace-only fields are treated as empty", func(t *testing.T) {
		cur := mergeBase()
		next, err := SaveRequest{ImageAPIModel: "   ", ImageAPIKey: "  "}.Merge(cur)
		if err != nil {
			t.Fatal(err)
		}
		if next.ImageModel != cur.ImageModel || next.ImageAPIKey != cur.ImageAPIKey {
			t.Fatalf("whitespace overwrote stored values: %+v", next)
		}
	})

	t.Run("clear connections drops image and video but not llm", func(t *testing.T) {
		next, err := SaveRequest{ClearConnections: true}.Merge(mergeBase())
		if err != nil {
			t.Fatal(err)
		}
		if next.ImageAPIURL != "" || next.ImageAPIKey != "" || next.VideoAPIURL != "" || next.VideoAPIKey != "" {
			t.Fatalf("image/video connections not cleared: %+v", next)
		}
		if next.LLMAPIURL != "https://llm.example.com" || next.LLMAPIKey != "llm-key" {
			t.Fatalf("llm connection must survive ClearConnections: %+v", next)
		}
	})

	t.Run("changed URL without new key is refused and nothing changes", func(t *testing.T) {
		cur := mergeBase()
		next, err := SaveRequest{ImageAPIURL: "https://attacker.example.com"}.Merge(cur)
		if err == nil || !strings.Contains(err.Error(), "已变更") {
			t.Fatalf("err = %v, want 已变更 refusal", err)
		}
		if next != cur {
			t.Fatalf("refused merge must return the unchanged settings, got %+v", next)
		}
	})

	t.Run("changed llm URL without new key is refused", func(t *testing.T) {
		_, err := SaveRequest{LLMAPIURL: "https://other-llm.example.com", LLMAPIKey: "   "}.Merge(mergeBase())
		if err == nil || !strings.Contains(err.Error(), "llm_api_url 已变更") {
			t.Fatalf("err = %v, want llm_api_url 已变更", err)
		}
	})

	t.Run("changed URL with new key succeeds", func(t *testing.T) {
		next, err := SaveRequest{ImageAPIURL: "https://img2.example.com", ImageAPIKey: "img-key-2"}.Merge(mergeBase())
		if err != nil {
			t.Fatal(err)
		}
		if next.ImageAPIURL != "https://img2.example.com" || next.ImageAPIKey != "img-key-2" {
			t.Fatalf("url/key = %q/%q", next.ImageAPIURL, next.ImageAPIKey)
		}
	})

	t.Run("first-time URL without key succeeds", func(t *testing.T) {
		cur := mergeBase()
		cur.VideoAPIURL, cur.VideoAPIKey = "", ""
		next, err := SaveRequest{VideoAPIURL: "https://vid-new.example.com"}.Merge(cur)
		if err != nil {
			t.Fatalf("first-time URL must not require a key: %v", err)
		}
		if next.VideoAPIURL != "https://vid-new.example.com" {
			t.Fatalf("video url = %q", next.VideoAPIURL)
		}
	})

	t.Run("unchanged URL without key keeps stored key", func(t *testing.T) {
		next, err := SaveRequest{ImageAPIURL: "https://img.example.com"}.Merge(mergeBase())
		if err != nil {
			t.Fatal(err)
		}
		if next.ImageAPIKey != "img-key" {
			t.Fatalf("stored key lost: %q", next.ImageAPIKey)
		}
	})

	t.Run("clear connections then new URL needs no old key", func(t *testing.T) {
		next, err := SaveRequest{ClearConnections: true, ImageAPIURL: "https://img3.example.com"}.Merge(mergeBase())
		if err != nil {
			t.Fatalf("after clearing, a new URL carries no stored key: %v", err)
		}
		if next.ImageAPIURL != "https://img3.example.com" || next.ImageAPIKey != "" {
			t.Fatalf("url/key = %q/%q", next.ImageAPIURL, next.ImageAPIKey)
		}
	})
}
