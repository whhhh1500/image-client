//go:build !windows

package platform

import (
	"net/http"
	"net/url"
)

// Outside Windows the environment variables are the system proxy settings.
func osProxy(*http.Request) (*url.URL, error) { return nil, nil }
