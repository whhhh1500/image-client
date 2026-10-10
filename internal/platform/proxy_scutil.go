package platform

import (
	"strings"
)

// scutilProxy is the macOS system proxy ("System Settings > Network > Proxies")
// as `scutil --proxy` prints it. Each address is "host:port", empty when that
// proxy is switched off.
type scutilProxy struct {
	http, https, socks string
	bypass             []string
}

// parseScutilProxy reads the dictionary `scutil --proxy` prints:
//
//	<dictionary> {
//	  ExceptionsList : <array> {
//	    0 : *.local
//	  }
//	  HTTPEnable : 1
//	  HTTPPort : 7890
//	  HTTPProxy : 127.0.0.1
//	  ...
//	}
func parseScutilProxy(out string) scutilProxy {
	values := map[string]string{}
	var settings scutilProxy
	inExceptions := false
	for _, line := range strings.Split(out, "\n") {
		key, value, ok := strings.Cut(strings.TrimSpace(line), " : ")
		switch {
		case inExceptions && strings.TrimSpace(line) == "}":
			inExceptions = false
		case !ok:
		case inExceptions:
			settings.bypass = append(settings.bypass, strings.ToLower(strings.TrimSpace(value)))
		case key == "ExceptionsList":
			inExceptions = strings.HasPrefix(value, "<array>")
		default:
			values[key] = strings.TrimSpace(value)
		}
	}
	address := func(kind string) string {
		host, port := values[kind+"Proxy"], values[kind+"Port"]
		if values[kind+"Enable"] != "1" || host == "" {
			return ""
		}
		if port == "" {
			return host
		}
		return host + ":" + port
	}
	settings.http, settings.https, settings.socks = address("HTTP"), address("HTTPS"), address("SOCKS")
	if values["ExcludeSimpleHostnames"] == "1" {
		settings.bypass = append(settings.bypass, "<local>")
	}
	return settings
}

// forScheme is the proxy URL for a request scheme: the matching HTTP(S)
// proxy, otherwise the SOCKS proxy, as macOS applications use them.
func (p scutilProxy) forScheme(scheme string) string {
	switch {
	case scheme == "https" && p.https != "":
		return "http://" + p.https
	case scheme == "http" && p.http != "":
		return "http://" + p.http
	case p.socks != "":
		return "socks5://" + p.socks
	}
	return ""
}
