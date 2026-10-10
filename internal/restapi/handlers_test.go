package restapi

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"imageclient/internal/config"
	"imageclient/internal/store"
)

func TestImageRejectsInvalidInputBeforeProvider(t *testing.T) {
	e := newEnv(t, readySettings)
	e.fake.generateImage = func(context.Context, ImageRequest, string) ([]store.AssetRef, error) {
		return nil, errors.New("provider reached")
	}
	for _, tc := range []struct{ body, message string }{
		{`{"prompt":"   "}`, "prompt 不能为空"},
		{`{"prompt":"a","n":0}`, "n 必须是 1 到 4 之间的整数"},
		{`{"prompt":"a","n":5}`, "n 必须是 1 到 4 之间的整数"},
		{`{"prompt":"a","quality":"ultra"}`, "quality 必须是 low、medium 或 high"},
		{`{"prompt":"a","background":"none"}`, "background 必须是 auto、transparent 或 opaque"},
		{fmt.Sprintf(`{"prompt":"a","size":%q}`, strings.Repeat("猫", 65)), "size 超过 64 个字符"},
		{fmt.Sprintf(`{"prompt":"a","referencePath":%q}`, strings.Repeat("a", 4097)), "referencePath 超过 4096 个字符"},
		{fmt.Sprintf(`{"prompt":"a","model":%q}`, strings.Repeat("m", 201)), "model 超过 200 个字符"},
		{fmt.Sprintf(`{"prompt":"a","projectId":%q}`, strings.Repeat("p", 201)), "projectId 超过 200 个字符"},
		// Limits count characters, not bytes; a long prompt is the provider's call.
		{fmt.Sprintf(`{"prompt":%q,"model":%q,"n":4}`, strings.Repeat("猫", 40_000), strings.Repeat("猫", 200)), "provider reached"},
	} {
		wantError(t, e.do("POST", "/api/image", tc.body), tc.message)
	}
	if len(e.fake.images) != 1 {
		t.Fatalf("provider called %d times, want only for the valid request", len(e.fake.images))
	}
	e.wantNothingRecorded()
}

func TestPromptsAreNotLengthLimited(t *testing.T) {
	if requiredPrompt("   ", "prompt") == nil {
		t.Fatal("blank prompt accepted")
	}
	for _, prompt := range []string{strings.Repeat("猫", 40_000), strings.Repeat("a", 1_000_000)} {
		if err := requiredPrompt(prompt, "prompt"); err != nil {
			t.Fatal(err)
		}
	}
}

func TestErrorResponsesDoNotEchoCredentials(t *testing.T) {
	e := newEnv(t, readySettings)
	e.fake.generateImage = func(context.Context, ImageRequest, string) ([]store.AssetRef, error) {
		return nil, errors.New("upstream rejected Authorization: Bearer TOPSECRET client_secret=TOPSECRET")
	}
	rec := e.do("POST", "/api/v1/media/images/generations", `{"prompt":"猫"}`)
	if rec.Code != 400 || strings.Contains(rec.Body.String(), "TOPSECRET") || strings.Contains(rec.Body.String(), "client_secret") {
		t.Fatalf("unsafe API error: %d %s", rec.Code, rec.Body.String())
	}
}

func TestImageGenerationRecordsHistory(t *testing.T) {
	e := newEnv(t, readySettings)
	e.fake.generateImage = func(context.Context, ImageRequest, string) ([]store.AssetRef, error) {
		return []store.AssetRef{{ID: "img-1", Kind: "image", Path: "a.png"}, {ID: "img-2", Kind: "image", Path: "b.png"}}, nil
	}
	rec := e.do("POST", "/api/v1/media/images/generations",
		`{"prompt":"猫","n":2,"quality":"high","referencePath":"C:/ref.png","projectId":"p1"}`)
	if rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	if assets := jsonOf(t, rec)["assets"].([]any); len(assets) != 2 || assets[0].(map[string]any)["id"] != "img-1" {
		t.Fatalf("assets = %v", assets)
	}
	want := ImageRequest{Prompt: "猫", Model: "gpt-image-2", Quality: "high", ReferencePath: "C:/ref.png", N: 2}
	if !reflect.DeepEqual(e.fake.images, []ImageRequest{want}) {
		t.Fatalf("request = %+v", e.fake.images)
	}
	if dir := filepath.Join(e.paths.Assets(), "图片", "api"); e.fake.imageDirs[0] != dir {
		t.Fatalf("dir = %s, want %s", e.fake.imageDirs[0], dir)
	}

	meta := e.metadata("img-2")
	params := meta["params"].(map[string]any)
	provenance := params["provenance"].(map[string]any)
	if meta["source"] != "API图生图" || meta["model"] != "gpt-image-2" || meta["projectId"] != "p1" ||
		params["prompt"] != "猫" || params["n"] != 2.0 || params["size"] != nil || params["quality"] != "high" ||
		params["referencePath"] != "C:/ref.png" || params["origin"] != "rest_api" {
		t.Fatalf("metadata = %v", meta)
	}
	materials := []any{map[string]any{"kind": "file", "label": "API参考图", "path": "C:/ref.png"}}
	if provenance["originalInput"] != "猫" || provenance["generationInput"] != "猫" || provenance["systemInstruction"] != nil ||
		!reflect.DeepEqual(provenance["sourceMaterials"], materials) || provenance["schemaVersion"] != 1.0 {
		t.Fatalf("provenance = %v", provenance)
	}
	e.wantHistoryChange("rest_image_generation", "img-1", "img-2")

	// Without a reference it is text-to-image and the history says so.
	e.fake.generateImage = func(context.Context, ImageRequest, string) ([]store.AssetRef, error) {
		return []store.AssetRef{{ID: "img-3", Kind: "image", Path: "c.png"}}, nil
	}
	if rec := e.do("POST", "/api/image", `{"prompt":"狗","model":"grok-imagine-image"}`); rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	meta = e.metadata("img-3")
	if meta["source"] != "API文生图" || meta["model"] != "grok-imagine-image" || meta["projectId"] != nil ||
		meta["params"].(map[string]any)["n"] != nil {
		t.Fatalf("metadata = %v", meta)
	}
}

func TestVideoGeneration(t *testing.T) {
	e := newEnv(t, readySettings)
	urls := func(n int) string {
		items := make([]string, n)
		for i := range items {
			items[i] = fmt.Sprintf("%q", fmt.Sprintf("https://cdn.example/%d.png", i))
		}
		return "[" + strings.Join(items, ",") + "]"
	}
	for _, tc := range []struct{ body, message string }{
		{`{"prompt":""}`, "prompt 不能为空"},
		{`{"prompt":"a","images":` + urls(31) + `}`, "images 最多允许 30 项"},
		{fmt.Sprintf(`{"prompt":"a","audios":[%q]}`, "https://x.example/"+strings.Repeat("a", 4100)), "audios 超过 4096 个字符"},
		{`{"prompt":"a","images":["https://cdn.example/a.png"],"videos":["http://cdn.example/v.mp4"]}`, "videos必须是公网 HTTPS URL"},
		{`{"prompt":"a","images":["https://127.0.0.1/a.png"]}`, "images必须是公网 HTTPS URL"},
		{`{"prompt":"a","durationS":0}`, "durationS 必须在 1 到 3600 秒之间"},
		{`{"prompt":"a","duration_s":3601}`, "durationS 必须在 1 到 3600 秒之间"},
		{`{"prompt":"a","mode":"` + strings.Repeat("m", 33) + `"}`, "mode 超过 32 个字符"},
	} {
		wantError(t, e.do("POST", "/api/v1/media/videos/generations", tc.body), tc.message)
	}
	e.wantNothingRecorded()

	e.fake.generateVideo = func(_ context.Context, req VideoRequest, _ string) ([]store.AssetRef, error) {
		return []store.AssetRef{{ID: "zzone:task-1", Kind: "video", Path: "v.mp4"}}, nil
	}
	rec := e.do("POST", "/api/video", `{"prompt":"海浪","duration_s":8,"images":["https://cdn.example/a.png"],"projectId":"p2"}`)
	if rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	want := VideoRequest{Model: "kling-video-v3", Prompt: "海浪", DurationS: 8, Images: []string{"https://cdn.example/a.png"}, Videos: []string{}, Audios: []string{}}
	if !reflect.DeepEqual(e.fake.videos, []VideoRequest{want}) {
		t.Fatalf("request = %+v", e.fake.videos)
	}
	if dir := filepath.Join(e.paths.Assets(), "视频", "api"); e.fake.videoDirs[0] != dir {
		t.Fatalf("dir = %s", e.fake.videoDirs[0])
	}
	meta := e.metadata("zzone:task-1")
	params := meta["params"].(map[string]any)
	materials := params["provenance"].(map[string]any)["sourceMaterials"].([]any)
	if meta["source"] != "API参考素材视频" || meta["projectId"] != "p2" || params["duration_s"] != 8.0 ||
		!reflect.DeepEqual(params["providerTaskIds"], []any{"task-1"}) || !reflect.DeepEqual(params["audios"], []any{}) ||
		params["aspectRatio"] != nil || params["workflowStatus"] != "direct_api_unreviewed" ||
		len(materials) != 1 || materials[0].(map[string]any)["label"] != "API视频参考图 URL" {
		t.Fatalf("metadata = %v", meta)
	}
	e.wantHistoryChange("rest_video_generation", "zzone:task-1")

	// Text-only video with defaults.
	e.fake.generateVideo = func(context.Context, VideoRequest, string) ([]store.AssetRef, error) {
		return []store.AssetRef{{ID: "local-1", Kind: "video", Path: "w.mp4"}}, nil
	}
	if rec := e.do("POST", "/api/v1/media/videos/generations", `{"prompt":"雨","aspectRatio":"9:16"}`); rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	if got := e.fake.videos[1]; got.DurationS != 5 || got.AspectRatio != "9:16" {
		t.Fatalf("request = %+v", got)
	}
	meta = e.metadata("local-1")
	if params := meta["params"].(map[string]any); meta["source"] != "API文生视频" || !reflect.DeepEqual(params["providerTaskIds"], []any{}) {
		t.Fatalf("metadata = %v", meta)
	}
}

// storyboardDoc is a valid one-shot storyboard.
func storyboardDoc(strategy, assets string) string {
	var b strings.Builder
	b.WriteString("# 视频分镜\n\n## 第1镜\n")
	for _, field := range storyboardFields {
		value := "内容"
		switch field {
		case "参考方式":
			value = strategy
		case "参考资产":
			value = assets
		}
		fmt.Fprintf(&b, "### %s\n%s\n\n", field, value)
	}
	return b.String()
}

const writerDoc = "# 视频剧本\n## 改编边界\n边界\n## 人物当前状态\n人物\n## 分场剧本\n场景\n## 编剧自检\n通过"

func TestAgentStepSavesValidatedDocument(t *testing.T) {
	e := newEnv(t, readySettings)
	e.fake.complete = func(context.Context, TextRequest) (TextCompletion, error) {
		return TextCompletion{Content: "<think>planning</think>\n" + storyboardDoc("reference", "character:hero:v1、scene:road:v1"), Completed: true}, nil
	}
	rec := e.do("POST", "/api/v1/agents/storyboard/runs", `{"input":"剧本与锚点","projectId":"p3"}`)
	if rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	body := jsonOf(t, rec)
	result := body["result"].(string)
	if strings.Contains(result, "think") || !strings.Contains(result, "### 参考方式\ntext") || !strings.Contains(result, "### 参考资产\n无") {
		t.Fatalf("result = %q", result)
	}
	shots := []any{1.0}
	if body["workflowStatus"] != "unreviewed" || !reflect.DeepEqual(body["normalization"], map[string]any{"correctedReferenceShots": shots}) {
		t.Fatalf("body = %v", body)
	}
	if req := e.fake.texts[0]; req.System != storyboardPrompt || req.Model != "m-default" || req.Input != "剧本与锚点" || req.Operation != "rest_agent_storyboard" {
		t.Fatalf("completion request = %+v", req)
	}
	if saved := e.fake.saved[0]; saved.label != "分镜" || saved.text != result || saved.model != "m-default" {
		t.Fatalf("saved = %+v", saved)
	}
	id := body["asset"].(map[string]any)["id"].(string)
	meta := e.metadata(id)
	params := meta["params"].(map[string]any)
	if meta["source"] != "分镜" || meta["model"] != "m-default" || meta["projectId"] != "p3" ||
		params["agentId"] != "storyboard" || params["documentType"] != "storyboard" || params["title"] != "分镜" ||
		params["text"] != result || params["version"] != 1.0 || params["changeType"] != "generated" ||
		params["workflowStatus"] != "unreviewed" || params["origin"] != "rest_api" || params["documentId"] == "" ||
		params["provenance"].(map[string]any)["systemInstruction"] != storyboardPrompt ||
		!reflect.DeepEqual(params["normalization"], map[string]any{"correctedReferenceShots": shots}) {
		t.Fatalf("metadata = %v", meta)
	}
	e.wantHistoryChange("rest_agent_storyboard", id)

	// The legacy alias of the writer, with a caller-supplied prompt and model.
	e.fake.complete = func(context.Context, TextRequest) (TextCompletion, error) {
		return TextCompletion{Content: writerDoc, Completed: true}, nil
	}
	rec = e.do("POST", "/api/script", `{"input":"原文","system":"自定义","model":"m-2"}`)
	if rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	body = jsonOf(t, rec)
	if req := e.fake.texts[1]; req.System != "自定义" || req.Model != "m-2" || req.Operation != "rest_agent_writer" {
		t.Fatalf("completion request = %+v", req)
	}
	params = e.metadata(body["asset"].(map[string]any)["id"].(string))["params"].(map[string]any)
	if params["documentType"] != "script" || params["agentId"] != "writer" ||
		!reflect.DeepEqual(body["normalization"], map[string]any{"correctedReferenceShots": []any{}}) {
		t.Fatalf("params = %v body = %v", params, body)
	}
}

func TestAgentStepRejections(t *testing.T) {
	answer := func(content string, completed bool) func(context.Context, TextRequest) (TextCompletion, error) {
		return func(context.Context, TextRequest) (TextCompletion, error) {
			return TextCompletion{Content: content, Completed: completed}, nil
		}
	}
	for _, tc := range []struct {
		name, path, body string
		settings         config.Settings
		complete         func(context.Context, TextRequest) (TextCompletion, error)
		message          string
	}{
		{"blank input", "/api/v1/agents/director/runs", `{"input":" "}`, readySettings, nil, "input 不能为空"},
		{"long system", "/api/v1/agents/director/runs", fmt.Sprintf(`{"input":"a","system":%q}`, strings.Repeat("s", maxTextChars+1)), readySettings, nil, "system 超过 2000000 个字符"},
		{"no llm", "/api/v1/agents/qc/runs", `{"input":"a"}`, config.Settings{Source: "none"}, nil, "未配置文本 LLM"},
		{"truncated", "/api/v1/agents/writer/runs", `{"input":"a"}`, readySettings, answer(writerDoc, false), "文本服务返回不完整结果（可能被截断），未保存文档；请重试或缩短输入"},
		{"refusal", "/api/v1/agents/writer/runs", `{"input":"a"}`, readySettings, answer("The prompt could not be submitted.", true), "文本模型返回了安全策略拒答，拒答内容不会保存为 Agent 文档"},
		{"incomplete", "/api/director", `{"input":"a"}`, readySettings, answer("# 改编规划\n只有标题", true), "Agent 输出缺少固定字段：## 输入边界、## 项目硬约束、## 核心戏剧判断、## 叙事节拍、## 核心视觉母题、## 风险与自检"},
		{"only thinking", "/api/consistency", `{"input":"a"}`, readySettings, answer("<think>unfinished", true), "Agent 没有返回内容"},
		{"provider error", "/api/qc_review", `{"input":"a"}`, readySettings, func(context.Context, TextRequest) (TextCompletion, error) {
			return TextCompletion{}, errors.New("文本接口未返回内容")
		}, "文本接口未返回内容"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			e := newEnv(t, tc.settings)
			e.fake.complete = tc.complete
			wantError(t, e.do("POST", tc.path, tc.body), tc.message)
			if len(e.fake.saved) != 0 {
				t.Fatal("a rejected document was saved")
			}
			e.wantNothingRecorded()
		})
	}
}

func TestTextCompletion(t *testing.T) {
	e := newEnv(t, readySettings)
	e.fake.complete = func(context.Context, TextRequest) (TextCompletion, error) {
		// Plain completions return what arrived, even when cut short.
		return TextCompletion{Content: "<think>x</think>partial", Completed: false}, nil
	}
	rec := e.do("POST", "/api/v1/text/completions", `{"system":"s","input":"i"}`)
	if body := jsonOf(t, rec); rec.Code != 200 || body["result"] != "<think>x</think>partial" {
		t.Fatalf("%d %v", rec.Code, body)
	}
	if rec := e.do("POST", "/api/text", `{"system":"s","input":"i","model":"m-9"}`); rec.Code != 200 {
		t.Fatalf("%d", rec.Code)
	}
	want := []TextRequest{
		{Model: "m-default", System: "s", Input: "i", Operation: "rest_text_completion"},
		{Model: "m-9", System: "s", Input: "i", Operation: "rest_text_completion"},
	}
	if !reflect.DeepEqual(e.fake.texts, want) {
		t.Fatalf("requests = %+v", e.fake.texts)
	}
	wantError(t, e.do("POST", "/api/text", `{"system":"","input":"i"}`), "system 不能为空")
	e.wantNothingRecorded()

	unconfigured := newEnv(t, config.Settings{Source: "none"})
	wantError(t, unconfigured.do("POST", "/api/v1/text/completions", `{"system":"s","input":"i"}`), "未配置文本 LLM")
}

func TestOrchestration(t *testing.T) {
	e := newEnv(t, readySettings)
	tool := `{"name":"writer","description":"写剧本","system":"你是编剧"}`
	for _, tc := range []struct{ body, message string }{
		{`{"system":"s","input":"i","tools":[]}`, "tools 数量必须在 1 到 32 之间"},
		{`{"system":"s","input":"i","tools":[` + tool + `,` + tool + `]}`, "tools.name 重复: writer"},
		{`{"system":"s","input":"i","tools":[{"name":" ","description":"d","system":"s"}]}`, "tools.name 不能为空"},
		{`{"system":"s","input":"i","tools":[{"name":"a","description":"","system":"s"}]}`, "tools.description 不能为空"},
		{`{"system":"s","input":"i","tools":[{"name":"a","description":"d","system":" "}]}`, "tools.system 不能为空"},
		{`{"system":"s","input":"","tools":[` + tool + `]}`, "input 不能为空"},
	} {
		wantError(t, e.do("POST", "/api/v1/agents/orchestrations", tc.body), tc.message)
	}
	e.wantNothingRecorded()

	e.fake.runAgent = func(context.Context, AgentRequest) (string, error) { return "编排结果", nil }
	rec := e.do("POST", "/api/v1/agents/orchestrations", `{"system":"总控","input":"故事","tools":[`+tool+`],"projectId":"p4"}`)
	if rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	body := jsonOf(t, rec)
	want := AgentRequest{System: "总控", Input: "故事", Model: "m-default", Tools: []ToolDef{{Name: "writer", Description: "写剧本", System: "你是编剧"}}}
	if body["result"] != "编排结果" || !reflect.DeepEqual(e.fake.agents, []AgentRequest{want}) {
		t.Fatalf("body = %v agents = %+v", body, e.fake.agents)
	}
	id := body["asset"].(map[string]any)["id"].(string)
	meta := e.metadata(id)
	params := meta["params"].(map[string]any)
	if meta["source"] != "Agent编排" || meta["projectId"] != "p4" || params["documentType"] != "orchestration" ||
		params["title"] != "Agent编排" || params["provenance"].(map[string]any)["systemInstruction"] != "总控" || params["agentId"] != nil {
		t.Fatalf("metadata = %v", meta)
	}
	e.wantHistoryChange("rest_agent_orchestration", id)
}

func TestSaveTextAsset(t *testing.T) {
	e := newEnv(t, readySettings)
	for _, tc := range []struct{ body, message string }{
		{`{"label":"../x","text":"t"}`, "label 不能包含路径分隔符"},
		{`{"label":"a\\b","text":"t"}`, "label 不能包含路径分隔符"},
		{`{"label":"..","text":"t"}`, "label 不能包含路径分隔符"},
		{`{"label":" ","text":"t"}`, "label 不能为空"},
		{`{"label":"a","text":""}`, "text 必须非空且不超过 20 MiB"},
	} {
		wantError(t, e.do("POST", "/api/v1/assets/text", tc.body), tc.message)
	}
	e.wantNothingRecorded()

	rec := e.do("POST", "/api/v1/assets/text", `{"label":"笔记","text":"正文"}`)
	if rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	asset := jsonOf(t, rec)["asset"].(map[string]any)
	meta := e.metadata(asset["id"].(string))
	params := meta["params"].(map[string]any)
	if asset["kind"] != "text" || meta["source"] != "笔记" || meta["model"] != nil || meta["projectId"] != nil ||
		params["documentType"] != "document" || params["text"] != "正文" || params["title"] != "笔记" ||
		params["provenance"].(map[string]any)["originalInput"] != "正文" {
		t.Fatalf("asset = %v metadata = %v", asset, meta)
	}
	if saved := e.fake.saved[0]; saved.label != "笔记" || saved.text != "正文" || saved.model != "" {
		t.Fatalf("saved = %+v", saved)
	}
	e.wantHistoryChange("rest_text_asset_save", asset["id"].(string))
}

func TestSaveMediaAsset(t *testing.T) {
	e := newEnv(t, readySettings)
	png := []byte("\x89PNG\r\n\x1a\nrest")
	encoded := base64.StdEncoding.EncodeToString(png)
	for _, tc := range []struct{ body, message string }{
		{`{"kind":"audio","extension":"mp3","dataBase64":"AA=="}`, "kind 必须是 image 或 video"},
		{`{"kind":"image","extension":"png","dataBase64":""}`, "dataBase64 不能为空且解码后不得超过 512 MiB"},
		{`{"kind":"image","extension":"png","dataBase64":"!!!!"}`, "无效 base64: illegal base64 data at input byte 0"},
		{`{"kind":"image","extension":"png","dataBase64":"AA"}`, "无效 base64: illegal base64 data at input byte 0"},
		{`{"kind":"image","extension":"png","dataBase64":"AA\nAA=="}`, "无效 base64: illegal base64 data at input byte 2"},
		{`{"kind":"image","extension":"png","dataBase64":"AA==","label":"` + strings.Repeat("l", 101) + `"}`, "label 超过 100 个字符"},
	} {
		wantError(t, e.do("POST", "/api/v1/assets/media", tc.body), tc.message)
	}
	e.wantNothingRecorded()

	rec := e.do("POST", "/api/v1/assets/media", `{"kind":"image","extension":"png","dataBase64":"`+encoded+`","prompt":"猫","model":"m"}`)
	if rec.Code != 200 {
		t.Fatalf("%d %s", rec.Code, rec.Body.String())
	}
	if saved := e.fake.saved[0]; saved.kind != "image" || saved.extension != "png" || string(saved.data) != string(png) {
		t.Fatalf("saved = %+v", saved)
	}
	id := jsonOf(t, rec)["asset"].(map[string]any)["id"].(string)
	meta := e.metadata(id)
	params := meta["params"].(map[string]any)
	if meta["source"] != "API图片资产" || meta["model"] != "m" || params["prompt"] != "猫" ||
		params["provenance"].(map[string]any)["originalInput"] != "猫" {
		t.Fatalf("metadata = %v", meta)
	}
	e.wantHistoryChange("rest_media_asset_save", id)

	rec = e.do("POST", "/api/v1/assets/media", `{"kind":"video","extension":"mp4","dataBase64":"AAAA","label":"成片"}`)
	meta = e.metadata(jsonOf(t, rec)["asset"].(map[string]any)["id"].(string))
	if meta["source"] != "成片" || meta["params"].(map[string]any)["prompt"] != nil {
		t.Fatalf("metadata = %v", meta)
	}
}

func TestConfigThroughAPI(t *testing.T) {
	e := newEnv(t, config.Settings{ImageAPIURL: "https://old.example/v1/images/generations", ImageAPIKey: "old-key", ImageModel: "gpt-image-2", Source: "db"})
	full := func(fields map[string]any) string {
		body := map[string]any{"imageApiUrl": "", "imageApiKey": "", "imageApiModel": "", "videoApiUrl": "", "videoApiKey": "",
			"videoApiModel": "", "llmApiUrl": "", "llmApiKey": "", "llmApiModel": "", "outputDir": ""}
		for k, v := range fields {
			body[k] = v
		}
		data, _ := json.Marshal(body)
		return string(data)
	}

	// Moving an endpoint without its key would send the stored key elsewhere.
	wantError(t, e.do("PUT", "/api/v1/system/config", full(map[string]any{"imageApiUrl": "https://new.example/v1/images/generations"})),
		"image_api_url 已变更：请同时提交该服务的新 Key，不能沿用已保存的旧 Key（避免密钥被发往新地址）")
	wantError(t, e.do("PUT", "/api/v1/system/config", full(map[string]any{"videoApiUrl": "ftp://x.example"})), "video_api_url 必须是 HTTP(S) 地址")
	if e.cfg.Get().ImageAPIURL != "https://old.example/v1/images/generations" {
		t.Fatal("a rejected update changed the settings")
	}

	rec := e.do("PUT", "/api/v1/system/config", full(map[string]any{
		"imageApiUrl": "https://new.example/v1/images/generations", "imageApiKey": "new-key", "llmApiUrl": "https://llm.example/v1", "llmApiKey": "lk",
	}))
	status := jsonOf(t, rec)
	if rec.Code != 200 || status["imageReady"] != true || status["llmReady"] != true || status["source"] != "api" ||
		status["imageApiUrl"] != "https://new.example/v1/images/generations" || strings.Contains(rec.Body.String(), "new-key") {
		t.Fatalf("%d %v", rec.Code, status)
	}
	var persisted config.Settings
	data, err := os.ReadFile(e.paths.BackendConfig())
	if err != nil || json.Unmarshal(data, &persisted) != nil || persisted.ImageAPIKey != "new-key" || persisted.Source != "api" {
		t.Fatalf("persisted = %+v (%v)", persisted, err)
	}

	// The legacy alias takes the same body.
	if rec := e.do("POST", "/api/config", full(map[string]any{"imageApiModel": "gemini-3-pro-image"})); rec.Code != 200 || jsonOf(t, rec)["imageModel"] != "gemini-3-pro-image" {
		t.Fatalf("legacy save: %d %s", rec.Code, rec.Body.String())
	}
	if got := jsonOf(t, e.do("GET", "/api/v1/system/config", "")); got["imageModel"] != "gemini-3-pro-image" || got["outputDir"] != e.paths.Assets() {
		t.Fatalf("config = %v", got)
	}
}

func TestLLMConfigThroughAPI(t *testing.T) {
	e := newEnv(t, config.Settings{LLMModel: "m-default", Source: "db"})
	if got := jsonOf(t, e.do("GET", "/api/v1/system/llm", "")); got["ready"] != false || got["url"] != nil || got["model"] != "m-default" || got["source"] != "db" {
		t.Fatalf("llm = %v", got)
	}
	for _, tc := range []struct{ body, message string }{
		{`{"url":"ftp://llm.example","key":"k","model":"m"}`, "LLM 地址必须是 HTTP(S) 地址"},
		{`{"url":"https://llm.example","key":" ","model":"m"}`, "LLM Key 不能为空且不得超过 16384 个字符"},
		{`{"url":"https://llm.example","key":"k","model":""}`, "LLM 模型不能为空且不得超过 200 个字符"},
	} {
		wantError(t, e.do("PUT", "/api/v1/system/llm", tc.body), tc.message)
	}
	rec := e.do("PUT", "/api/v1/system/llm", `{"url":" https://llm.example/v1// ","key":" k ","model":" m-2 "}`)
	want := map[string]any{"ready": true, "url": "https://llm.example/v1", "model": "m-2", "source": "api"}
	if got := jsonOf(t, rec); !reflect.DeepEqual(got, want) {
		t.Fatalf("saved = %v", got)
	}
	if cur := e.cfg.Get(); cur.LLMAPIKey != "k" || cur.LLMAPIURL != "https://llm.example/v1" {
		t.Fatalf("settings = %+v", cur)
	}
	if got := jsonOf(t, e.do("GET", "/api/v1/system/llm", "")); !reflect.DeepEqual(got, want) {
		t.Fatalf("llm = %v", got)
	}
}

func TestCatalogRoutes(t *testing.T) {
	e := newEnv(t, readySettings)
	providers := []any{map[string]any{"id": "zzone", "name": "ZZone 网关", "active": true}}
	if got := jsonOf(t, e.do("GET", "/api/v1/catalog/providers", "")); !reflect.DeepEqual(got["items"], providers) {
		t.Fatalf("providers = %v", got)
	}
	wantError(t, e.do("PUT", "/api/v1/system/provider", `{"id":" "}`), "id 不能为空")
	wantError(t, e.do("PUT", "/api/v1/system/provider", `{"id":"other"}`), "未知 provider: other")
	if got := jsonOf(t, e.do("PUT", "/api/v1/system/provider", `{"id":"zzone"}`)); !reflect.DeepEqual(got["items"], providers) {
		t.Fatalf("set provider = %v", got)
	}

	models := jsonOf(t, e.do("GET", "/api/v1/catalog/models", ""))
	if !reflect.DeepEqual(models["image"], []any{"gpt-image-2", "gemini-3-pro-image"}) ||
		len(models["video"].([]any)) != len(config.VideoModels) || len(models["llm"].([]any)) != len(config.LLMModels) {
		t.Fatalf("models = %v", models)
	}
	video := jsonOf(t, e.do("GET", "/api/v1/catalog/models/video", ""))
	if !reflect.DeepEqual(video["items"], []any{"kling-video-v3"}) || len(video["capabilities"].([]any)) != 1 {
		t.Fatalf("video models = %v", video)
	}
	if llm := jsonOf(t, e.do("GET", "/api/v1/catalog/models/llm", "")); len(llm["items"].([]any)) != len(config.LLMModels) {
		t.Fatalf("llm models = %v", llm)
	}
	e.fake.videoModels = func(context.Context) ([]string, error) { return nil, errors.New("视频模型目录返回 401") }
	wantError(t, e.do("GET", "/api/v1/catalog/models/video", ""), "视频模型目录返回 401")
	e.fake.imageModels = func(context.Context) ([]string, error) { return nil, nil }
	if image := jsonOf(t, e.do("GET", "/api/v1/catalog/models/image", "")); !reflect.DeepEqual(image["items"], []any{}) {
		t.Fatalf("image models = %v", image)
	}

	tools := jsonOf(t, e.do("GET", "/api/v1/catalog/tools", ""))
	var names []string
	for _, item := range tools["tools"].([]any) {
		fn := item.(map[string]any)["function"].(map[string]any)
		names = append(names, fn["name"].(string))
		if fn["endpoint"].(map[string]any)["method"] != "POST" {
			t.Fatalf("tool %v", fn)
		}
	}
	if tools["apiVersion"] != "v1" || strings.Join(names, ",") != "director,writer,storyboard,consistency,qc,generate_image,generate_video" {
		t.Fatalf("tools = %v", tools)
	}
	if legacy := jsonOf(t, e.do("GET", "/api/tools", "")); !reflect.DeepEqual(legacy, tools) {
		t.Fatal("legacy tools differ")
	}
}
