//go:build !windows && !darwin

package platform

import (
	"net/http"
	"net/url"
)

// On Linux the environment variables are the system proxy settings.
func osProxy(*http.Request) (*url.URL, error) { return nil, nil }
