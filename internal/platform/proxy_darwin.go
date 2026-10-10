package platform

import (
	"net/http"
	"net/url"
	"os/exec"
	"sync"
	"time"
)

// An app started from Finder or the Dock does not inherit a shell's proxy
// variables, so the proxy set in System Settings (where tools such as Clash
// switch on their "system proxy") is read from scutil. It is re-read at most
// every few seconds so a proxy switched on while the app runs is picked up.
var (
	proxyMu       sync.Mutex
	proxyCache    scutilProxy
	proxyLoadedAt time.Time
)

func loadDarwinProxy() scutilProxy {
	proxyMu.Lock()
	defer proxyMu.Unlock()
	if !proxyLoadedAt.IsZero() && time.Since(proxyLoadedAt) < 5*time.Second {
		return proxyCache
	}
	proxyLoadedAt = time.Now()
	out, err := exec.Command("scutil", "--proxy").Output()
	if err != nil {
		proxyCache = scutilProxy{}
		return proxyCache
	}
	proxyCache = parseScutilProxy(string(out))
	return proxyCache
}

func osProxy(req *http.Request) (*url.URL, error) {
	settings := loadDarwinProxy()
	if bypassed(req.URL.Hostname(), settings.bypass) {
		return nil, nil
	}
	server := settings.forScheme(req.URL.Scheme)
	if server == "" {
		return nil, nil
	}
	return url.Parse(server)
}
