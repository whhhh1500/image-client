package platform

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"time"
)

var (
	clientsOnce sync.Once
	proxied     *http.Client
	direct      *http.Client
)

func initClients() {
	newClient := func(proxy func(*http.Request) (*url.URL, error)) *http.Client {
		transport := http.DefaultTransport.(*http.Transport).Clone()
		transport.Proxy = proxy
		transport.ResponseHeaderTimeout = 0 // phases are bounded by Do's watchdog
		return &http.Client{Transport: transport}
	}
	proxied = newClient(systemProxy)
	direct = newClient(nil)
}

// ClientFor returns the shared client matching url's proxy policy: loopback
// endpoints (local model servers) never go through a proxy.
func ClientFor(rawURL string) *http.Client {
	clientsOnce.Do(initClients)
	if IsLoopbackURL(rawURL) {
		return direct
	}
	return proxied
}

// SharedClient is the client for arbitrary public URLs.
func SharedClient() *http.Client {
	clientsOnce.Do(initClients)
	return proxied
}

// IsLoopbackURL reports whether rawURL targets localhost or a loopback IP.
func IsLoopbackURL(rawURL string) bool {
	u, err := url.Parse(rawURL)
	if err != nil {
		return false
	}
	host := u.Hostname()
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

// systemProxy honours HTTP(S)_PROXY first, then the OS proxy settings.
func systemProxy(req *http.Request) (*url.URL, error) {
	if proxy, err := http.ProxyFromEnvironment(req); proxy != nil || err != nil {
		return proxy, err
	}
	return osProxy(req)
}

// Timeouts bound the phases of one request. Generation endpoints can think for
// minutes before answering, and a slow link must not cut off a body that is
// still arriving, so the wait for headers and the silence between body chunks
// are limited separately; Total is the absolute ceiling.
type Timeouts struct {
	Header time.Duration
	Idle   time.Duration
	Total  time.Duration
}

// TimeoutError says which phase overran; its message is user-facing.
type TimeoutError struct{ msg string }

func (e *TimeoutError) Error() string { return e.msg }

// seconds renders a limit for messages; sub-second limits keep one decimal.
func seconds(d time.Duration) string {
	if d < time.Second {
		return strconv.FormatFloat(d.Seconds(), 'f', 1, 64)
	}
	return strconv.Itoa(int(d.Round(time.Second) / time.Second))
}

// Do sends req under t. The response body must be closed; reads fail with a
// *TimeoutError after t.Idle without data.
func Do(ctx context.Context, client *http.Client, req *http.Request, t Timeouts) (*http.Response, error) {
	var cancelTotal context.CancelFunc = func() {}
	if t.Total > 0 {
		ctx, cancelTotal = context.WithTimeoutCause(ctx, t.Total,
			&TimeoutError{fmt.Sprintf("超过 %s 秒总时限", seconds(t.Total))})
	}
	ctx, cancel := context.WithCancelCause(ctx)
	stop := func() { cancel(nil); cancelTotal() }

	if t.Header > 0 {
		watchdog := time.AfterFunc(t.Header, func() {
			cancel(&TimeoutError{fmt.Sprintf("等待响应超过 %s 秒", seconds(t.Header))})
		})
		defer watchdog.Stop()
	}
	resp, err := client.Do(req.WithContext(ctx))
	if err != nil {
		cause := context.Cause(ctx)
		stop()
		var timeout *TimeoutError
		if errors.As(cause, &timeout) {
			return nil, timeout
		}
		return nil, err
	}
	resp.Body = &watchedBody{body: resp.Body, ctx: ctx, cancel: cancel, stop: stop, idle: t.Idle}
	return resp, nil
}

// watchedBody re-arms the idle watchdog around every read.
type watchedBody struct {
	body   io.ReadCloser
	ctx    context.Context
	cancel context.CancelCauseFunc
	stop   func()
	idle   time.Duration
	timer  *time.Timer
}

func (b *watchedBody) Read(p []byte) (int, error) {
	if b.idle > 0 {
		if b.timer == nil {
			b.timer = time.AfterFunc(b.idle, func() {
				b.cancel(&TimeoutError{fmt.Sprintf("超过 %s 秒没有收到新数据", seconds(b.idle))})
			})
		} else {
			b.timer.Reset(b.idle)
		}
	}
	n, err := b.body.Read(p)
	if b.timer != nil {
		b.timer.Stop()
	}
	if err != nil && err != io.EOF {
		var timeout *TimeoutError
		if cause := context.Cause(b.ctx); errors.As(cause, &timeout) {
			return n, timeout
		}
	}
	return n, err
}

func (b *watchedBody) Close() error {
	if b.timer != nil {
		b.timer.Stop()
	}
	err := b.body.Close()
	b.stop()
	return err
}

// TooLargeError means a body exceeded its size limit; retrying cannot help.
type TooLargeError struct{ Label string }

func (e *TooLargeError) Error() string { return e.Label + "超过大小限制" }

// ReadLimited appends r to buf until EOF, failing as soon as buf would exceed
// limit. Bytes read before a transfer error stay in buf, so a caller can
// resume with a Range request. onProgress (optional) sees the running length.
func ReadLimited(r io.Reader, buf []byte, limit int64, label string, onProgress func(int64)) ([]byte, error) {
	chunk := make([]byte, 32*1024)
	for {
		n, err := r.Read(chunk)
		if n > 0 {
			if int64(len(buf)+n) > limit {
				return buf, &TooLargeError{Label: label}
			}
			buf = append(buf, chunk[:n]...)
			if onProgress != nil {
				onProgress(int64(len(buf)))
			}
		}
		if err == io.EOF {
			return buf, nil
		}
		if err != nil {
			return buf, fmt.Errorf("读取%s失败: %w", label, err)
		}
	}
}

// ReadAllLimited reads a whole response body under limit.
func ReadAllLimited(resp *http.Response, limit int64, label string, onProgress func(int64)) ([]byte, error) {
	defer resp.Body.Close()
	if resp.ContentLength > limit {
		return nil, &TooLargeError{Label: label}
	}
	initial := 0
	if resp.ContentLength > 0 {
		initial = int(resp.ContentLength)
	}
	return ReadLimited(resp.Body, make([]byte, 0, initial), limit, label, onProgress)
}
