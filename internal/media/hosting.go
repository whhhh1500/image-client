package media

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime/multipart"
	"net"
	"net/http"
	"net/textproto"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/platform"
)

// Media hosting publishes local images and videos to a user-configured public
// HTTPS upload endpoint, only when the user asks. The endpoint's token lives
// in app_secrets, which the frontend's SQL channel cannot read.
const (
	hostingSecretKey         = "media_hosting.v1"
	maxPublishSources        = 8
	maxPublishBytes          = 512 << 20
	maxHostingResponseBytes  = 1 << 20
	hostingTimeout           = 30 * time.Second
	errHostingEndpointPublic = "媒体托管端点必须是公网 HTTPS 地址，且不能包含用户信息"
	errPublishTooLarge       = "媒体文件总大小不能超过 512 MiB"
)

type authMode string

const (
	authBearer authMode = "bearer"
	authRaw    authMode = "raw"
)

func (m *authMode) UnmarshalJSON(b []byte) error {
	var s string
	if err := json.Unmarshal(b, &s); err != nil {
		return err
	}
	if s != string(authBearer) && s != string(authRaw) {
		return fmt.Errorf("未知的认证方式：%q", s)
	}
	*m = authMode(s)
	return nil
}

// hostingConfig is the stored configuration; its JSON is the app_secrets value.
type hostingConfig struct {
	Endpoint  string   `json:"endpoint"`
	FileField string   `json:"fileField"`
	URLField  string   `json:"urlField"`
	AuthMode  authMode `json:"authMode"`
	Token     string   `json:"token"`
}

// hostingStatus is what the frontend sees: never the token itself.
type hostingStatus struct {
	Endpoint   string   `json:"endpoint"`
	FileField  string   `json:"fileField"`
	URLField   string   `json:"urlField"`
	AuthMode   authMode `json:"authMode"`
	HasToken   bool     `json:"hasToken"`
	Configured bool     `json:"configured"`
}

type hostingSaveInput struct {
	Endpoint  string   `json:"endpoint"`
	FileField string   `json:"fileField"`
	URLField  string   `json:"urlField"`
	AuthMode  authMode `json:"authMode"`
	// Token replaces the stored token when non-empty; empty keeps it.
	Token      *string `json:"token"`
	ClearToken bool    `json:"clearToken"`
}

type publishRequest struct {
	ProjectID        string          `json:"projectId"`
	ExpectedEndpoint string          `json:"expectedEndpoint"`
	Sources          []publishSource `json:"sources"`
}

type publishSource struct {
	AssetID   string `json:"assetId"`
	SourceURI string `json:"sourceUri"`
}

type publishResult struct {
	Key    string `json:"key"`
	URL    string `json:"url,omitempty"`
	SHA256 string `json:"sha256,omitempty"`
	Error  string `json:"error,omitempty"`
}

type publishOutput struct {
	Results []publishResult `json:"results"`
}

// resolvedSource is a file ready to upload.
type resolvedSource struct {
	key      string
	fileName string
	mime     string
	data     []byte
}

func (c hostingConfig) status() hostingStatus {
	return hostingStatus{
		Endpoint: c.Endpoint, FileField: c.FileField, URLField: c.URLField, AuthMode: c.AuthMode,
		HasToken: c.Token != "", Configured: c.validate() == nil,
	}
}

func (c hostingConfig) validate() error {
	if !publicHTTPSURL(c.Endpoint) {
		return errors.New(errHostingEndpointPublic)
	}
	if !validFormField(c.FileField) {
		return errors.New("文件字段无效")
	}
	if !validDotPath(c.URLField) {
		return errors.New("地址字段无效")
	}
	return nil
}

func validFieldByte(b byte) bool { return isASCIIAlnum(b) || b == '_' || b == '-' }

func validFormField(field string) bool {
	if field == "" || len(field) > 128 {
		return false
	}
	for i := 0; i < len(field); i++ {
		if !validFieldByte(field[i]) {
			return false
		}
	}
	return true
}

func validDotPath(path string) bool {
	if path == "" || len(path) > 256 {
		return false
	}
	for _, segment := range strings.Split(path, ".") {
		if !validFormField(segment) {
			return false
		}
	}
	return true
}

// hostingConfig reads the stored configuration, or the defaults when none
// was saved.
func (s *Service) hostingConfig(ctx context.Context) (hostingConfig, error) {
	var (
		raw   string
		found bool
	)
	err := s.db.With(ctx, func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, "SELECT value FROM app_secrets WHERE key=?", &sqlitex.ExecOptions{
			Args: []any{hostingSecretKey},
			ResultFunc: func(stmt *sqlite.Stmt) error {
				raw, found = stmt.ColumnText(0), true
				return nil
			},
		})
	})
	if err != nil {
		return hostingConfig{}, fmt.Errorf("读取媒体托管设置失败：%w", err)
	}
	if !found {
		return hostingConfig{FileField: "file", URLField: "url", AuthMode: authBearer}, nil
	}
	// Every field is required, as it was when the value was written.
	var stored struct {
		Endpoint  *string   `json:"endpoint"`
		FileField *string   `json:"fileField"`
		URLField  *string   `json:"urlField"`
		AuthMode  *authMode `json:"authMode"`
		Token     *string   `json:"token"`
	}
	if err := json.Unmarshal([]byte(raw), &stored); err != nil || stored.Endpoint == nil || stored.FileField == nil ||
		stored.URLField == nil || stored.AuthMode == nil || stored.Token == nil {
		return hostingConfig{}, errors.New("媒体托管设置无效，请重新保存")
	}
	return hostingConfig{Endpoint: *stored.Endpoint, FileField: *stored.FileField, URLField: *stored.URLField,
		AuthMode: *stored.AuthMode, Token: *stored.Token}, nil
}

func endpointHost(endpoint string) (string, bool) {
	u, err := url.Parse(strings.TrimSpace(endpoint))
	if err != nil || u.Hostname() == "" {
		return "", false
	}
	return u.Hostname(), true
}

// saveHosting stores a new configuration. Moving the endpoint to another host
// drops the stored token unless a new one comes with it, so a token is never
// sent to a host it was not entered for.
func (s *Service) saveHosting(ctx context.Context, in hostingSaveInput) (hostingStatus, error) {
	previous, err := s.hostingConfig(ctx)
	if err != nil {
		return hostingStatus{}, err
	}
	endpoint := strings.TrimSpace(in.Endpoint)
	before, okBefore := endpointHost(previous.Endpoint)
	after, okAfter := endpointHost(endpoint)
	changedHost := strings.TrimSpace(previous.Endpoint) != endpoint
	if okBefore && okAfter {
		changedHost = !strings.EqualFold(before, after)
	}
	newToken := in.Token != nil && *in.Token != ""
	token := previous.Token
	if in.ClearToken || (changedHost && !newToken) {
		token = ""
	}
	if newToken {
		token = *in.Token
	}
	if in.AuthMode != authBearer && in.AuthMode != authRaw {
		return hostingStatus{}, errors.New("认证方式无效")
	}
	cfg := hostingConfig{
		Endpoint: endpoint, FileField: strings.TrimSpace(in.FileField), URLField: strings.TrimSpace(in.URLField),
		AuthMode: in.AuthMode, Token: token,
	}
	if err := cfg.validate(); err != nil {
		return hostingStatus{}, err
	}
	value, err := marshalJSON(cfg)
	if err != nil {
		return hostingStatus{}, errors.New("保存媒体托管设置失败")
	}
	err = s.db.With(ctx, func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, "INSERT INTO app_secrets(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
			&sqlitex.ExecOptions{Args: []any{hostingSecretKey, string(value)}})
	})
	if err != nil {
		return hostingStatus{}, fmt.Errorf("保存媒体托管设置失败：%w", err)
	}
	return cfg.status(), nil
}

// publishMedia uploads the selected project media one by one; a failed
// upload is reported in its result and does not stop the others.
func (s *Service) publishMedia(ctx context.Context, req publishRequest) (publishOutput, error) {
	cfg, err := s.hostingConfig(ctx)
	if err != nil {
		return publishOutput{}, err
	}
	if err := cfg.validate(); err != nil {
		return publishOutput{}, err
	}
	// The UI showed the user this endpoint; refuse if it changed since.
	if req.ExpectedEndpoint != cfg.Endpoint {
		return publishOutput{}, errors.New("媒体托管配置已变更，请重新确认后再发布")
	}
	sources, err := s.resolveSources(ctx, req)
	if err != nil {
		return publishOutput{}, err
	}
	return publish(ctx, cfg, sources, false), nil
}

func (s *Service) resolveSources(ctx context.Context, req publishRequest) ([]resolvedSource, error) {
	if strings.TrimSpace(req.ProjectID) == "" {
		return nil, errors.New("项目不能为空")
	}
	if len(req.Sources) == 0 || len(req.Sources) > maxPublishSources {
		return nil, errors.New("每次只能发布 1 至 8 个媒体文件")
	}
	var catalog []ComicMedia
	for _, source := range req.Sources {
		if strings.TrimSpace(source.SourceURI) != "" {
			if s.comicCatalog != nil {
				entries, err := s.comicCatalog(ctx, req.ProjectID)
				if err != nil {
					return nil, err
				}
				catalog = entries
			}
			break
		}
	}
	var total int64
	resolved := make([]resolvedSource, 0, len(req.Sources))
	for _, source := range req.Sources {
		hasAsset, hasURI := strings.TrimSpace(source.AssetID) != "", strings.TrimSpace(source.SourceURI) != ""
		if hasAsset == hasURI {
			return nil, errors.New("每个媒体来源必须且只能提供 assetId 或 sourceUri")
		}
		var key, path, kind, format string
		if hasAsset {
			var err error
			path, kind, format, err = s.projectAsset(ctx, source.AssetID, req.ProjectID)
			if err != nil {
				return nil, err
			}
			key = "asset:" + source.AssetID
		} else {
			uri := strings.TrimSpace(source.SourceURI)
			var entry *ComicMedia
			for i := range catalog {
				if catalog[i].SourceURI == uri {
					entry = &catalog[i]
					break
				}
			}
			if entry == nil {
				return nil, errors.New("漫画媒体来源不存在或不属于当前项目")
			}
			if entry.Path == "" {
				return nil, errors.New("漫画来源不是可发布的媒体")
			}
			key, path, kind = "comic:"+uri, entry.Path, entry.Kind
		}
		if kind != "image" && kind != "video" {
			return nil, errors.New("只能发布图片或视频资产")
		}
		canonical, err := canonicalPath(path)
		if err != nil {
			return nil, errors.New("媒体文件不存在或不可访问")
		}
		info, err := os.Stat(canonical)
		if err != nil {
			return nil, errors.New("读取媒体文件失败")
		}
		if !info.Mode().IsRegular() {
			return nil, errors.New("媒体来源必须是普通文件")
		}
		remaining := maxPublishBytes - total
		if info.Size() > remaining {
			return nil, errors.New(errPublishTooLarge)
		}
		data, err := readAtMost(canonical, remaining)
		if err != nil {
			return nil, err
		}
		total += int64(len(data))
		name := filepath.Base(canonical)
		ext := format
		if ext == "" {
			_, ext, _ = splitName(name)
		}
		ext = asciiLower(strings.TrimLeft(ext, "."))
		mime := ""
		switch {
		case kind == "image" && DetectImageFormat(data) != "":
			mime = imageMIME(ext)
		case kind == "video" && ValidVideo(data, ext):
			mime = videoMIME(ext)
		}
		if mime == "" {
			return nil, errors.New("媒体文件格式与资产类型不匹配")
		}
		resolved = append(resolved, resolvedSource{key: key, fileName: name, mime: mime, data: data})
	}
	return resolved, nil
}

// projectAsset looks up an asset that must belong to projectID.
func (s *Service) projectAsset(ctx context.Context, id, projectID string) (path, kind, format string, err error) {
	var (
		found    bool
		metadata string
	)
	err = s.db.With(ctx, func(conn *sqlite.Conn) error {
		return sqlitex.Execute(conn, "SELECT kind,path,metadata,format FROM assets WHERE id=?", &sqlitex.ExecOptions{
			Args: []any{id},
			ResultFunc: func(stmt *sqlite.Stmt) error {
				found = true
				kind, path, metadata, format = stmt.ColumnText(0), stmt.ColumnText(1), stmt.ColumnText(2), stmt.ColumnText(3)
				return nil
			},
		})
	})
	if err != nil {
		return "", "", "", errors.New("读取媒体资产失败")
	}
	if !found {
		return "", "", "", errors.New("媒体资产不存在")
	}
	var meta struct {
		ProjectID any `json:"projectId"`
	}
	if json.Unmarshal([]byte(metadata), &meta) != nil || meta.ProjectID != projectID {
		return "", "", "", errors.New("媒体资产不属于当前项目")
	}
	return path, kind, format, nil
}

func readAtMost(path string, limit int64) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, errors.New("读取媒体文件失败")
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, limit+1))
	if err != nil {
		return nil, errors.New("读取媒体文件失败")
	}
	if int64(len(data)) > limit {
		return nil, errors.New(errPublishTooLarge)
	}
	return data, nil
}

func imageMIME(ext string) string {
	switch ext {
	case "png":
		return "image/png"
	case "jpg", "jpeg":
		return "image/jpeg"
	case "webp":
		return "image/webp"
	case "gif":
		return "image/gif"
	}
	return ""
}

func videoMIME(ext string) string {
	switch ext {
	case "mp4":
		return "video/mp4"
	case "webm":
		return "video/webm"
	case "mov":
		return "video/quicktime"
	}
	return ""
}

// publish uploads each source. allowTestEndpoint lets tests use a loopback
// HTTP server; the returned media URL must be public HTTPS regardless.
func publish(ctx context.Context, cfg hostingConfig, sources []resolvedSource, allowTestEndpoint bool) publishOutput {
	client := *platform.ClientFor(cfg.Endpoint)
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	results := make([]publishResult, 0, len(sources))
	for _, source := range sources {
		mediaURL, digest, err := uploadOne(ctx, &client, cfg, source, allowTestEndpoint)
		if err != nil {
			results = append(results, publishResult{Key: source.key, Error: err.Error()})
			continue
		}
		results = append(results, publishResult{Key: source.key, URL: mediaURL, SHA256: digest})
	}
	return publishOutput{Results: results}
}

func uploadOne(ctx context.Context, client *http.Client, cfg hostingConfig, source resolvedSource, allowTestEndpoint bool) (string, string, error) {
	if !allowTestEndpoint && !publicHTTPSURL(cfg.Endpoint) {
		return "", "", errors.New("媒体托管端点无效")
	}
	sum := sha256.Sum256(source.data)
	digest := hex.EncodeToString(sum[:])

	// The multipart body streams the file bytes between a prefix holding the
	// part header and the closing boundary, so the file is not copied.
	var head bytes.Buffer
	form := multipart.NewWriter(&head)
	header := textproto.MIMEHeader{}
	header.Set("Content-Disposition", fmt.Sprintf(`form-data; name="%s"; filename="%s"`,
		quoteEscaper.Replace(cfg.FileField), quoteEscaper.Replace(source.fileName)))
	header.Set("Content-Type", source.mime)
	if _, err := form.CreatePart(header); err != nil {
		return "", "", errors.New("媒体文件格式无效")
	}
	prefix := bytes.Clone(head.Bytes())
	head.Reset()
	form.Close()
	body := io.MultiReader(bytes.NewReader(prefix), bytes.NewReader(source.data), bytes.NewReader(head.Bytes()))

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, cfg.Endpoint, body)
	if err != nil {
		return "", "", errors.New("媒体托管请求失败")
	}
	req.ContentLength = int64(len(prefix) + len(source.data) + head.Len())
	req.Header.Set("Content-Type", form.FormDataContentType())
	if cfg.Token != "" {
		value := cfg.Token
		if cfg.AuthMode == authBearer {
			value = "Bearer " + cfg.Token
		}
		if !validHeaderValue(value) {
			return "", "", errors.New("媒体托管凭据无效")
		}
		req.Header.Set("Authorization", value)
	}
	resp, err := platform.Do(ctx, client, req, platform.Timeouts{Total: hostingTimeout})
	if err != nil {
		return "", "", errors.New("媒体托管请求失败")
	}
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		resp.Body.Close()
		return "", "", errors.New("媒体托管服务返回失败状态")
	}
	raw, err := platform.ReadAllLimited(resp, maxHostingResponseBytes, "媒体托管响应", nil)
	if err != nil {
		var tooLarge *platform.TooLargeError
		if errors.As(err, &tooLarge) {
			return "", "", errors.New("媒体托管响应过大")
		}
		return "", "", errors.New("读取媒体托管响应失败")
	}
	var value any
	if json.Unmarshal(raw, &value) != nil {
		return "", "", errors.New("媒体托管响应格式无效")
	}
	for _, segment := range strings.Split(cfg.URLField, ".") {
		object, ok := value.(map[string]any)
		if !ok {
			value = nil
			break
		}
		value = object[segment]
	}
	mediaURL, ok := value.(string)
	if !ok {
		return "", "", errors.New("媒体托管响应未包含媒体地址")
	}
	if !publicHTTPSURL(mediaURL) {
		return "", "", errors.New("媒体托管返回了无效地址")
	}
	return mediaURL, digest, nil
}

var quoteEscaper = strings.NewReplacer(`\`, `\\`, `"`, `\"`)

// validHeaderValue accepts visible ASCII and tabs, like the HTTP library the
// previous backend used.
func validHeaderValue(v string) bool {
	for i := 0; i < len(v); i++ {
		if c := v[i]; c != '\t' && (c < 0x20 || c > 0x7E) {
			return false
		}
	}
	return true
}

// publicHTTPSURL reports whether raw is an https URL without credentials whose
// host is a public name or address: no localhost, no .local/.internal/.lan
// names, no single-label hosts, no IPv6 literals and no private, loopback,
// link-local, CGNAT, multicast or broadcast IPv4 addresses (numeric hosts are
// parsed with the URL standard's IPv4 rules, so "0x7f.1" is 127.0.0.1).
func publicHTTPSURL(raw string) bool {
	u, err := url.Parse(strings.TrimSpace(raw))
	if err != nil || u.Scheme != "https" || u.User != nil {
		return false
	}
	host := strings.ToLower(u.Hostname())
	if host == "" || strings.Contains(host, ":") {
		return false
	}
	if host == "localhost" || strings.HasSuffix(host, ".localhost") || strings.HasSuffix(host, ".local") ||
		strings.HasSuffix(host, ".internal") || strings.HasSuffix(host, ".lan") {
		return false
	}
	ip, numeric, ok := parseIPv4Host(host)
	if numeric {
		return ok && publicIPv4(ip)
	}
	return strings.Contains(host, ".")
}

func publicIPv4(ip net.IP) bool {
	ip = ip.To4()
	return ip != nil && !(ip.IsPrivate() || ip.IsLoopback() || ip.IsLinkLocalUnicast() || ip.IsUnspecified() ||
		ip.IsMulticast() || ip.Equal(net.IPv4bcast) || (ip[0] == 100 && ip[1] >= 64 && ip[1] <= 127))
}

// parseIPv4Host applies the WHATWG URL host rules: a host whose last label is
// a number is an IPv4 address (decimal, octal or hex parts, 1 to 4 of them),
// and fails to parse if it is not a valid one.
func parseIPv4Host(host string) (ip net.IP, numeric, ok bool) {
	parts := strings.Split(host, ".")
	if len(parts) > 1 && parts[len(parts)-1] == "" {
		parts = parts[:len(parts)-1]
	}
	last := parts[len(parts)-1]
	if last == "" {
		return nil, false, false
	}
	if _, isNumber := ipv4Part(last); !isNumber && !allDigits(last) {
		return nil, false, false
	}
	if len(parts) > 4 {
		return nil, true, false
	}
	values := make([]uint64, len(parts))
	for i, part := range parts {
		value, valid := ipv4Part(part)
		if !valid {
			return nil, true, false
		}
		values[i] = value
	}
	var addr uint64
	for i, value := range values[:len(values)-1] {
		if value > 255 {
			return nil, true, false
		}
		addr |= value << (8 * (3 - i))
	}
	lastValue := values[len(values)-1]
	if lastValue >= 1<<(8*(5-len(values))) {
		return nil, true, false
	}
	addr |= lastValue
	return net.IPv4(byte(addr>>24), byte(addr>>16), byte(addr>>8), byte(addr)), true, true
}

// ipv4Part parses one dotted part: 0x-prefixed hex, 0-prefixed octal or
// decimal.
func ipv4Part(part string) (uint64, bool) {
	if part == "" {
		return 0, false
	}
	base := 10
	switch {
	case len(part) >= 2 && (part[:2] == "0x" || part[:2] == "0X"):
		part, base = part[2:], 16
		if part == "" {
			return 0, true
		}
	case len(part) >= 2 && part[0] == '0':
		part, base = part[1:], 8
	}
	value, err := strconv.ParseUint(part, base, 64)
	if err != nil || value > 1<<32 {
		return 0, false
	}
	return value, true
}
