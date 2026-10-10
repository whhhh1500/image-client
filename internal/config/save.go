package config

import (
	"fmt"
	"log/slog"
	"net/url"
	"strings"
	"unicode/utf8"
)

// SaveRequest is a settings update from the frontend or the REST API. Empty
// fields keep the stored value, so a caller may push a partial update.
type SaveRequest struct {
	ImageAPIURL   string `json:"imageApiUrl"`
	ImageAPIKey   string `json:"imageApiKey"`
	ImageAPIModel string `json:"imageApiModel"`
	VideoAPIURL   string `json:"videoApiUrl"`
	VideoAPIKey   string `json:"videoApiKey"`
	VideoAPIModel string `json:"videoApiModel"`
	LLMAPIURL     string `json:"llmApiUrl"`
	LLMAPIKey     string `json:"llmApiKey"`
	LLMAPIModel   string `json:"llmApiModel"`
	OutputDir     string `json:"outputDir"`
	// ClearConnections drops the stored image/video endpoints and keys; without
	// it, deleting the last API profile could never clear them.
	ClearConnections bool `json:"clearConnections"`
}

// Validate bounds every field and requires HTTP(S) endpoints.
func (r SaveRequest) Validate() error {
	for _, f := range []struct{ name, value string }{
		{"image_api_url", r.ImageAPIURL}, {"video_api_url", r.VideoAPIURL}, {"llm_api_url", r.LLMAPIURL},
	} {
		value := strings.TrimSpace(f.value)
		if value == "" {
			continue
		}
		u, err := url.Parse(value)
		if err != nil {
			return fmt.Errorf("%s 地址无效: %v", f.name, err)
		}
		if (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
			return fmt.Errorf("%s 必须是 HTTP(S) 地址", f.name)
		}
	}
	for _, f := range []struct {
		name, value string
		max         int
	}{
		{"image_api_model", r.ImageAPIModel, 200}, {"video_api_model", r.VideoAPIModel, 200}, {"llm_api_model", r.LLMAPIModel, 200},
		{"output_dir", r.OutputDir, 4096},
		{"image_api_key", r.ImageAPIKey, 16384}, {"video_api_key", r.VideoAPIKey, 16384}, {"llm_api_key", r.LLMAPIKey, 16384},
	} {
		if utf8.RuneCountInString(f.value) > f.max {
			return fmt.Errorf("%s 超过 %d 个字符", f.name, f.max)
		}
	}
	return nil
}

// Merge applies r to cur. Changing a service's address requires its key in
// the same request: otherwise an address-only update (the REST API has no
// authentication) would send the stored key to a new host.
func (r SaveRequest) Merge(cur Settings) (Settings, error) {
	next := cur
	if r.ClearConnections {
		next.ImageAPIURL, next.ImageAPIKey = "", ""
		next.VideoAPIURL, next.VideoAPIKey = "", ""
	}
	services := []struct {
		name                  string
		inURL, inKey, inModel string
		url, key, model       *string
	}{
		{"image_api_url", r.ImageAPIURL, r.ImageAPIKey, r.ImageAPIModel, &next.ImageAPIURL, &next.ImageAPIKey, &next.ImageModel},
		{"video_api_url", r.VideoAPIURL, r.VideoAPIKey, r.VideoAPIModel, &next.VideoAPIURL, &next.VideoAPIKey, &next.VideoModel},
		{"llm_api_url", r.LLMAPIURL, r.LLMAPIKey, r.LLMAPIModel, &next.LLMAPIURL, &next.LLMAPIKey, &next.LLMModel},
	}
	for _, s := range services {
		incoming, stored := strings.TrimSpace(s.inURL), strings.TrimSpace(*s.url)
		changed := incoming != "" && stored != "" && stored != incoming
		if changed && strings.TrimSpace(*s.key) != "" && strings.TrimSpace(s.inKey) == "" {
			return cur, fmt.Errorf("%s 已变更：请同时提交该服务的新 Key，不能沿用已保存的旧 Key（避免密钥被发往新地址）", s.name)
		}
	}
	for _, s := range services {
		setIfPresent(s.url, s.inURL)
		setIfPresent(s.key, s.inKey)
		setIfPresent(s.model, s.inModel)
	}
	setIfPresent(&next.OutputDir, r.OutputDir)
	return next, nil
}

func setIfPresent(dst *string, value string) {
	if v := strings.TrimSpace(value); v != "" {
		*dst = v
	}
}

// Save validates, merges and installs r, marking the settings as coming from
// source ("db" for the frontend, "api" for the REST API).
func (s *Store) Save(r SaveRequest, source string) (Status, error) {
	if err := r.Validate(); err != nil {
		return Status{}, err
	}
	next, err := r.Merge(s.Get())
	if err != nil {
		return Status{}, err
	}
	next.Source = source
	if err := s.Replace(next); err != nil {
		return Status{}, err
	}
	status := s.Status()
	slog.Info("configuration.updated", "status", status)
	return status, nil
}
