package platform

import (
	"net/http"
	"net/url"
	"path"
	"strings"
	"sync"
	"time"

	"golang.org/x/sys/windows/registry"
)

// The Windows proxy settings ("Settings > Network > Proxy") live in the
// Internet Settings key. They are re-read at most every few seconds so a proxy
// switched on while the app runs is picked up.
type windowsProxy struct {
	server   string
	bypass   []string
	loadedAt time.Time
}

var (
	proxyMu    sync.Mutex
	proxyCache windowsProxy
)

func loadWindowsProxy() windowsProxy {
	proxyMu.Lock()
	defer proxyMu.Unlock()
	if !proxyCache.loadedAt.IsZero() && time.Since(proxyCache.loadedAt) < 5*time.Second {
		return proxyCache
	}
	proxyCache = windowsProxy{loadedAt: time.Now()}
	key, err := registry.OpenKey(registry.CURRENT_USER,
		`Software\Microsoft\Windows\CurrentVersion\Internet Settings`, registry.QUERY_VALUE)
	if err != nil {
		return proxyCache
	}
	defer key.Close()
	if enabled, _, err := key.GetIntegerValue("ProxyEnable"); err != nil || enabled == 0 {
		return proxyCache
	}
	server, _, _ := key.GetStringValue("ProxyServer")
	override, _, _ := key.GetStringValue("ProxyOverride")
	proxyCache.server = strings.TrimSpace(server)
	for _, item := range strings.Split(override, ";") {
		if item = strings.TrimSpace(item); item != "" {
			proxyCache.bypass = append(proxyCache.bypass, strings.ToLower(item))
		}
	}
	return proxyCache
}

func osProxy(req *http.Request) (*url.URL, error) {
	settings := loadWindowsProxy()
	if settings.server == "" || bypassed(req.URL.Hostname(), settings.bypass) {
		return nil, nil
	}
	server := pickProxy(settings.server, req.URL.Scheme)
	if server == "" {
		return nil, nil
	}
	if !strings.Contains(server, "://") {
		server = "http://" + server
	}
	return url.Parse(server)
}

// pickProxy handles both "host:port" and "http=h:p;https=h:p;socks=h:p".
func pickProxy(server, scheme string) string {
	if !strings.Contains(server, "=") {
		return server
	}
	entries := map[string]string{}
	for _, part := range strings.Split(server, ";") {
		if name, value, ok := strings.Cut(part, "="); ok {
			entries[strings.ToLower(strings.TrimSpace(name))] = strings.TrimSpace(value)
		}
	}
	if value := entries[scheme]; value != "" {
		return value
	}
	if value := entries["http"]; value != "" {
		return value
	}
	if value := entries["socks"]; value != "" {
		return "socks5://" + value
	}
	return ""
}

func bypassed(host string, rules []string) bool {
	host = strings.ToLower(host)
	for _, rule := range rules {
		if rule == "<local>" {
			if !strings.Contains(host, ".") {
				return true
			}
			continue
		}
		if matched, _ := path.Match(rule, host); matched {
			return true
		}
	}
	return false
}
