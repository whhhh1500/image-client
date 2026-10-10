package platform

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strconv"
	"strings"
	"time"
)

// DownloadPolicy governs Download. A GET is idempotent, so unlike a paid
// generation request it is retried, resuming from the bytes already received.
type DownloadPolicy struct {
	Attempts   int
	Timeouts   Timeouts
	RetryDelay time.Duration // doubles after every failed attempt
	Limit      int64
	Label      string
	Header     http.Header // sent with every attempt, e.g. Authorization
}

// DownloadEvent reports progress. Retry is set when an attempt failed and the
// next one starts after Delay.
type DownloadEvent struct {
	Received int64
	Total    int64 // 0 when unknown
	Attempt  int
	Retry    *DownloadRetry
}

type DownloadRetry struct {
	Err   error
	Delay time.Duration
	Next  int
}

type fatalError struct{ err error }

func (e fatalError) Error() string { return e.err.Error() }
func (e fatalError) Unwrap() error { return e.err }

// Download fetches url. Transient failures (network errors, stalls, 408, 429,
// 5xx) are retried with exponential backoff and a Range request for the rest;
// expired or forbidden URLs and oversized bodies fail at once.
func Download(ctx context.Context, client *http.Client, url string, p DownloadPolicy, onEvent func(DownloadEvent)) ([]byte, error) {
	if onEvent == nil {
		onEvent = func(DownloadEvent) {}
	}
	started := time.Now()
	var (
		body      []byte
		validator string
		resumed   bool
		lastErr   error
	)
	for attempt := 1; attempt <= p.Attempts; attempt++ {
		resumed = resumed || len(body) > 0
		err := downloadAttempt(ctx, client, url, p, attempt, &body, &validator, onEvent)
		if err == nil {
			slog.Info("download.complete", "durationMs", time.Since(started).Milliseconds(), "bytes", len(body),
				"attempts", attempt, "resumed", resumed, "source", SafeURL(url))
			return body, nil
		}
		var fatal fatalError
		if errors.As(err, &fatal) || ctx.Err() != nil {
			return nil, fmt.Errorf("下载%s失败：%w", p.Label, err)
		}
		slog.Warn("download.retry", "attempt", attempt, "receivedBytes", len(body), "error", ErrorText(err), "source", SafeURL(url))
		lastErr = err
		if attempt < p.Attempts {
			delay := p.RetryDelay << (attempt - 1)
			onEvent(DownloadEvent{Received: int64(len(body)), Attempt: attempt,
				Retry: &DownloadRetry{Err: err, Delay: delay, Next: attempt + 1}})
			select {
			case <-time.After(delay):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
	}
	return nil, fmt.Errorf("下载%s失败（已尝试 %d 次）：%w", p.Label, p.Attempts, lastErr)
}

func downloadAttempt(ctx context.Context, client *http.Client, url string, p DownloadPolicy, attempt int,
	body *[]byte, validator *string, onEvent func(DownloadEvent)) error {
	req, err := http.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return fatalError{err}
	}
	if p.Header != nil {
		req.Header = p.Header.Clone()
	}
	resuming := len(*body) > 0
	if resuming {
		req.Header.Set("Range", fmt.Sprintf("bytes=%d-", len(*body)))
		// Without a validator the bytes are still resumed: result URLs name
		// immutable files, and a server that ignores Range answers 200 anyway.
		if *validator != "" {
			req.Header.Set("If-Range", *validator)
		}
	}
	resp, err := Do(ctx, client, req, p.Timeouts)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	switch status := resp.StatusCode; {
	case status == http.StatusPartialContent && resuming:
		// "bytes START-END/TOTAL" must continue exactly where we stopped.
		contentRange := resp.Header.Get("Content-Range")
		if start, ok := rangeStart(contentRange); !ok || start != int64(len(*body)) {
			*body, *validator = (*body)[:0], ""
			return fmt.Errorf("续传范围不匹配：%s", contentRange)
		}
	case status >= 200 && status < 300:
		// A full body: the first attempt, or a server that ignored Range.
		*body = (*body)[:0]
		*validator = ""
		if etag := resp.Header.Get("ETag"); etag != "" && !strings.HasPrefix(etag, "W/") {
			*validator = etag // If-Range accepts strong validators only
		} else if modified := resp.Header.Get("Last-Modified"); modified != "" {
			*validator = modified
		}
	case status == http.StatusRequestedRangeNotSatisfiable && resuming:
		*body, *validator = (*body)[:0], ""
		return fmt.Errorf("%s", resp.Status)
	case status == http.StatusRequestTimeout || status == http.StatusTooManyRequests || status >= 500:
		return fmt.Errorf("%s", resp.Status)
	default:
		return fatalError{fmt.Errorf("%s", resp.Status)}
	}

	var total int64
	if resp.ContentLength >= 0 {
		total = int64(len(*body)) + resp.ContentLength // a 206 carries only the rest
		if total > p.Limit {
			return fatalError{&TooLargeError{Label: p.Label}}
		}
	}
	onEvent(DownloadEvent{Received: int64(len(*body)), Total: total, Attempt: attempt})
	*body, err = ReadLimited(resp.Body, *body, p.Limit, p.Label, func(received int64) {
		onEvent(DownloadEvent{Received: received, Total: total, Attempt: attempt})
	})
	var tooLarge *TooLargeError
	if errors.As(err, &tooLarge) {
		return fatalError{err}
	}
	return err
}

func rangeStart(contentRange string) (int64, bool) {
	spec, ok := strings.CutPrefix(contentRange, "bytes ")
	if !ok {
		return 0, false
	}
	start, _, ok := strings.Cut(spec, "-")
	if !ok {
		return 0, false
	}
	value, err := strconv.ParseInt(strings.TrimSpace(start), 10, 64)
	return value, err == nil
}
