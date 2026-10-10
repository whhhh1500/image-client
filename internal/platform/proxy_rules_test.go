package platform

import (
	"reflect"
	"testing"
)

// scutilOutput is `scutil --proxy` with ClashX's system proxy switched on.
const scutilOutput = `<dictionary> {
  ExceptionsList : <array> {
    0 : 127.0.0.1
    1 : localhost
    2 : *.local
    3 : 169.254/16
  }
  ExcludeSimpleHostnames : 1
  FTPPassive : 1
  HTTPEnable : 1
  HTTPPort : 7890
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 7890
  HTTPSProxy : 127.0.0.1
  SOCKSEnable : 1
  SOCKSPort : 7891
  SOCKSProxy : 127.0.0.1
}
`

func TestParseScutilProxyReadsEnabledProxiesAndExceptions(t *testing.T) {
	got := parseScutilProxy(scutilOutput)
	want := scutilProxy{
		http: "127.0.0.1:7890", https: "127.0.0.1:7890", socks: "127.0.0.1:7891",
		bypass: []string{"127.0.0.1", "localhost", "*.local", "169.254/16", "<local>"},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("parsed %+v\nwant   %+v", got, want)
	}
	if off := parseScutilProxy("<dictionary> {\n  HTTPEnable : 0\n  HTTPProxy : 10.0.0.1\n  HTTPPort : 80\n}\n"); off.http != "" || off.forScheme("https") != "" {
		t.Fatalf("a disabled proxy was used: %+v", off)
	}
}

func TestScutilProxyPicksTheSchemesProxyThenSOCKS(t *testing.T) {
	both := scutilProxy{http: "h:1", https: "s:2", socks: "k:3"}
	socksOnly := scutilProxy{socks: "k:3"}
	for _, c := range []struct {
		proxy  scutilProxy
		scheme string
		want   string
	}{
		{both, "https", "http://s:2"},
		{both, "http", "http://h:1"},
		{scutilProxy{http: "h:1"}, "https", ""},
		{socksOnly, "https", "socks5://k:3"},
		{scutilProxy{}, "https", ""},
	} {
		if got := c.proxy.forScheme(c.scheme); got != c.want {
			t.Errorf("%+v %s: %q, want %q", c.proxy, c.scheme, got, c.want)
		}
	}
}

func TestBypassedMatchesNamesPatternsAndRanges(t *testing.T) {
	rules := []string{"<local>", "*.local", "169.254/16", "10.0.0.0/8", "localhost"}
	for host, want := range map[string]bool{
		"intranet":        true,
		"printer.local":   true,
		"169.254.10.20":   true,
		"10.1.2.3":        true,
		"localhost":       true,
		"api.example.com": false,
		"169.255.0.1":     false,
		"zzone.cc.cd":     false,
		"PRINTER.LOCAL":   true,
	} {
		if got := bypassed(host, rules); got != want {
			t.Errorf("bypassed(%q) = %v, want %v", host, got, want)
		}
	}
}
