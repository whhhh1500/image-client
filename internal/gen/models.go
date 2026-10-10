package gen

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"slices"
	"strings"

	"imageclient/internal/config"
	"imageclient/internal/platform"
)

// A model catalog is small; anything bigger is not one.
const maxCatalogBytes = 16 << 20

// getCatalog fetches a /v1/models document.
func (s *Service) getCatalog(ctx context.Context, endpoint, key string) (*http.Response, error) {
	req, err := http.NewRequest(http.MethodGet, endpoint, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+key)
	return platform.Do(ctx, s.clientFor(endpoint), req, platform.Timeouts{Total: s.catalogTimeout})
}

func readCatalog(resp *http.Response) (any, error) {
	raw, err := readBody(resp, maxCatalogBytes, "模型目录", nil)
	if err != nil {
		return nil, err
	}
	return decodeJSON(raw)
}

// ImageModels lists the gateway's models, falling back to the known image
// models when the catalog cannot be read or is empty.
func (s *Service) ImageModels(ctx context.Context) []string {
	cfg := s.cfg.Get()
	endpoint := config.ModelsEndpoint(cfg.ImageAPIURL)
	if endpoint == "" {
		return slices.Clone(config.ImageModels)
	}
	resp, err := s.getCatalog(ctx, endpoint, cfg.ImageAPIKey)
	if err != nil {
		return slices.Clone(config.ImageModels)
	}
	if !isSuccess(resp) {
		drain(resp)
		return slices.Clone(config.ImageModels)
	}
	catalog, err := readCatalog(resp)
	if err != nil {
		return slices.Clone(config.ImageModels)
	}
	if ids := dataIDs(catalog); len(ids) > 0 {
		return ids
	}
	return slices.Clone(config.ImageModels)
}

// VideoModels lists the video service's models. Before a video API is
// configured the offline catalogue stands in; once it is, the service is
// authoritative and failures are reported.
func (s *Service) VideoModels(ctx context.Context) ([]string, error) {
	cfg := s.cfg.Get()
	endpoint := config.ModelsEndpoint(cfg.VideoAPIURL)
	if endpoint == "" || strings.TrimSpace(cfg.VideoAPIKey) == "" {
		return slices.Clone(config.VideoModels), nil
	}
	resp, err := s.getCatalog(ctx, endpoint, cfg.VideoAPIKey)
	if err != nil {
		return nil, fmt.Errorf("读取视频模型目录失败: %v", err)
	}
	if !isSuccess(resp) {
		drain(resp)
		return nil, fmt.Errorf("视频模型目录返回 %s，请检查视频 API Key、权限和 Base URL", statusLine(resp.StatusCode))
	}
	catalog, err := readCatalog(resp)
	if err != nil {
		return nil, fmt.Errorf("解析视频模型目录失败: %v", err)
	}
	ids := dataIDs(catalog)
	if len(ids) == 0 {
		return nil, errors.New("视频模型目录为空，请确认使用的是视频 API Key，并检查权限和 Base URL")
	}
	return ids, nil
}

// dataIDs reads data[].id of an OpenAI catalog.
func dataIDs(catalog any) []string {
	object, _ := catalog.(map[string]any)
	entries, _ := object["data"].([]any)
	ids := []string{}
	for _, entry := range entries {
		item, _ := entry.(map[string]any)
		if id, ok := item["id"].(string); ok {
			ids = append(ids, id)
		}
	}
	return ids
}

// FetchModels reads the catalog for the address and key the settings page
// holds, before they are saved. kind (image, video or llm) names the stored
// connection whose key may stand in for a blank one.
func (s *Service) FetchModels(ctx context.Context, apiURL, apiKey, kind string) ([]string, error) {
	cfg := s.cfg.Get()
	storedURL, storedKey, err := storedConnection(cfg, kind)
	if err != nil {
		return nil, err
	}
	target, key, err := catalogCredentials(apiURL, apiKey, storedURL, storedKey)
	if err != nil {
		return nil, err
	}
	endpoint := config.ModelsEndpointFor(target)
	if endpoint == "" {
		return nil, fmt.Errorf("无法从地址推导模型目录（需要 http(s) 地址）: %s", target)
	}
	resp, err := s.getCatalog(ctx, endpoint, key)
	if err != nil {
		return nil, fmt.Errorf("读取模型目录失败: %v", err)
	}
	if !isSuccess(resp) {
		drain(resp)
		return nil, fmt.Errorf("模型目录返回 %s，请检查 API Key、权限和地址：%s", statusLine(resp.StatusCode), endpoint)
	}
	catalog, err := readCatalog(resp)
	if err != nil {
		return nil, fmt.Errorf("解析模型目录失败: %v", err)
	}
	models := parseModelIDs(catalog)
	if len(models) == 0 {
		slog.Warn("settings.models_empty", "kind", kind, "endpoint", platform.SafeURL(endpoint))
		return nil, fmt.Errorf("模型目录为空，请确认该地址是 OpenAI 兼容网关：%s", endpoint)
	}
	slog.Info("settings.models_fetched", "kind", kind, "count", len(models), "endpoint", platform.SafeURL(endpoint))
	return models, nil
}

func storedConnection(cfg config.Settings, kind string) (string, string, error) {
	switch kind {
	case "image":
		return cfg.ImageAPIURL, cfg.ImageAPIKey, nil
	case "video":
		return cfg.VideoAPIURL, cfg.VideoAPIKey, nil
	case "llm":
		return cfg.LLMAPIURL, cfg.LLMAPIKey, nil
	}
	return "", "", fmt.Errorf("未知的模型类型: %s", kind)
}

// catalogCredentials picks the address and key for a catalog request. A
// blank key may reuse the stored one only while the address is unchanged:
// otherwise this request would send the stored key to a newly typed host.
func catalogCredentials(incomingURL, incomingKey, storedURL, storedKey string) (string, string, error) {
	target := strings.TrimSpace(incomingURL)
	if target == "" {
		target = strings.TrimSpace(storedURL)
	}
	if target == "" {
		return "", "", errors.New("请先填写 API 地址")
	}
	if key := strings.TrimSpace(incomingKey); key != "" {
		return target, key, nil
	}
	storedKey = strings.TrimSpace(storedKey)
	if storedKey == "" {
		return "", "", errors.New("请先填写 API Key")
	}
	if strings.TrimRight(strings.TrimSpace(storedURL), "/") != strings.TrimRight(target, "/") {
		return "", "", errors.New("当前地址与已保存地址不同：请填写该地址对应的 API Key，不能沿用已保存的旧 Key")
	}
	return target, storedKey, nil
}

// parseModelIDs reads model ids from an OpenAI catalog ({"data": [...]})
// and looser shapes ({"models": [...]}, a bare array), whose items are ids or
// objects with an id or name. Ids are trimmed and deduplicated.
func parseModelIDs(catalog any) []string {
	object, _ := catalog.(map[string]any)
	entries, ok := object["data"].([]any)
	if !ok {
		entries, ok = object["models"].([]any)
	}
	if !ok {
		entries, _ = catalog.([]any)
	}
	seen := map[string]bool{}
	ids := []string{}
	for _, entry := range entries {
		id, ok := entry.(string)
		if !ok {
			item, _ := entry.(map[string]any)
			if id, ok = item["id"].(string); !ok {
				id, _ = item["name"].(string)
			}
		}
		if id = strings.TrimSpace(id); id != "" && !seen[id] {
			seen[id] = true
			ids = append(ids, id)
		}
	}
	return ids
}
