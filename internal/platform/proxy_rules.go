package platform

import (
	"net"
	"path"
	"strings"
)

// bypassed reports whether host matches one of the OS proxy exceptions:
// "<local>" (plain host names), glob patterns such as "*.local", and address
// ranges such as "169.254/16" or "10.0.0.0/8".
func bypassed(host string, rules []string) bool {
	host = strings.ToLower(host)
	for _, rule := range rules {
		if rule == "<local>" {
			if !strings.Contains(host, ".") {
				return true
			}
			continue
		}
		if prefix, ok := cidrRule(rule); ok {
			if ip := net.ParseIP(host); ip != nil && prefix.Contains(ip) {
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

// cidrRule reads "169.254/16" (macOS writes ranges with the zero octets
// left out) as well as full CIDR notation.
func cidrRule(rule string) (*net.IPNet, bool) {
	address, bits, ok := strings.Cut(rule, "/")
	if !ok {
		return nil, false
	}
	if !strings.Contains(address, ":") {
		for strings.Count(address, ".") < 3 {
			address += ".0"
		}
	}
	_, prefix, err := net.ParseCIDR(address + "/" + bits)
	return prefix, err == nil
}
