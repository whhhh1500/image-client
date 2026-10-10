package platform

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"
)

// rawStep is the exact bytes one accepted connection receives. stall keeps the
// connection open after the bytes are sent, to simulate a silent link.
type rawStep struct {
	raw   string
	stall bool
}

// rawServer serves one scripted step per accepted connection, so tests can send
// truncated or stalled bodies that net/http would never produce itself.
type rawServer struct {
	URL string

	mu      sync.Mutex
	headers []http.Header
}

func newRawServer(t *testing.T, steps ...rawStep) *rawServer {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	s := &rawServer{URL: "http://" + ln.Addr().String() + "/files/image.png"}
	done := make(chan struct{})
	t.Cleanup(func() { ln.Close() })
	t.Cleanup(func() { close(done) }) // runs before ln.Close

	go func() {
		for i := 0; ; i++ {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			step := rawStep{raw: "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"}
			if i < len(steps) {
				step = steps[i]
			}
			go s.handle(conn, step, done)
		}
	}()
	return s
}

func (s *rawServer) handle(conn net.Conn, step rawStep, done chan struct{}) {
	defer conn.Close()
	req, err := http.ReadRequest(bufio.NewReader(conn))
	if err != nil {
		return
	}
	s.mu.Lock()
	s.headers = append(s.headers, req.Header.Clone())
	s.mu.Unlock()
	io.WriteString(conn, step.raw)
	if step.stall {
		select {
		case <-done:
		case <-time.After(3 * time.Second):
		}
	}
}

// requests returns the headers of every request received, in order.
func (s *rawServer) requests() []http.Header {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]http.Header(nil), s.headers...)
}

func rawOK(body []byte, etag string) rawStep {
	return rawStep{raw: fmt.Sprintf("HTTP/1.1 200 OK\r\nContent-Length: %d\r\nETag: %q\r\nConnection: close\r\n\r\n%s",
		len(body), etag, body)}
}

// rawPartialHead declares the full length but sends only sent; the caller decides
// whether the connection then closes (drop) or stalls.
func rawPartialHead(total int, etag string, sent []byte, stall bool) rawStep {
	return rawStep{raw: fmt.Sprintf("HTTP/1.1 200 OK\r\nContent-Length: %d\r\nETag: %q\r\nConnection: close\r\n\r\n%s",
		total, etag, sent), stall: stall}
}

func rawRange(start int, payload []byte, total int, etag string) rawStep {
	return rawStep{raw: fmt.Sprintf("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes %d-%d/%d\r\nContent-Length: %d\r\nETag: %q\r\nConnection: close\r\n\r\n%s",
		start, total-1, total, len(payload), etag, payload)}
}

func rawStatus(status string) rawStep {
	return rawStep{raw: "HTTP/1.1 " + status + "\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"}
}

func testDownloadPolicy() DownloadPolicy {
	return DownloadPolicy{
		Attempts:   4,
		RetryDelay: time.Millisecond,
		Timeouts:   Timeouts{Header: 2 * time.Second, Idle: 150 * time.Millisecond, Total: 5 * time.Second},
		Limit:      1 << 20,
		Label:      "图片",
	}
}

func distinctPayload(n int) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte(i*7 + 3)
	}
	return b
}

func TestDownloadRetriesServerErrorThenSucceeds(t *testing.T) {
	payload := []byte("final image bytes")
	srv := newRawServer(t, rawStatus("503 Service Unavailable"), rawOK(payload, "v1"))

	got, err := Download(context.Background(), ClientFor(srv.URL), srv.URL, testDownloadPolicy(), nil)
	if err != nil {
		t.Fatalf("Download: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("payload = %q, want %q", got, payload)
	}
	if n := len(srv.requests()); n != 2 {
		t.Fatalf("requests = %d, want 2", n)
	}
}

func TestDownloadResumesAfterDroppedConnection(t *testing.T) {
	const total = 1000
	payload := distinctPayload(total)
	half := payload[:total/2]
	srv := newRawServer(t,
		rawPartialHead(total, "v1", half, false), // connection drops after half
		rawRange(total/2, payload[total/2:], total, "v1"),
	)

	var mu sync.Mutex
	var retries []DownloadRetry
	var retryReceived []int64
	onEvent := func(ev DownloadEvent) {
		mu.Lock()
		defer mu.Unlock()
		if ev.Retry != nil {
			retries = append(retries, *ev.Retry)
			retryReceived = append(retryReceived, ev.Received)
		}
	}

	got, err := Download(context.Background(), ClientFor(srv.URL), srv.URL, testDownloadPolicy(), onEvent)
	if err != nil {
		t.Fatalf("Download: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("resumed payload differs: got %d bytes, want %d", len(got), len(payload))
	}

	reqs := srv.requests()
	if len(reqs) != 2 {
		t.Fatalf("requests = %d, want 2", len(reqs))
	}
	if r := reqs[1].Get("Range"); r != fmt.Sprintf("bytes=%d-", total/2) {
		t.Errorf("resume Range = %q, want bytes=%d-", r, total/2)
	}
	if ir := reqs[1].Get("If-Range"); ir != `"v1"` {
		t.Errorf("If-Range = %q, want \"v1\"", ir)
	}

	mu.Lock()
	defer mu.Unlock()
	if len(retries) != 1 {
		t.Fatalf("retry events = %d, want 1", len(retries))
	}
	if retries[0].Next != 2 {
		t.Errorf("retry Next = %d, want 2", retries[0].Next)
	}
	if retryReceived[0] != total/2 {
		t.Errorf("retry Received = %d, want %d", retryReceived[0], total/2)
	}
}

func TestDownloadResumesStalledTransferWithinIdleBudget(t *testing.T) {
	const total = 1000
	payload := distinctPayload(total)
	srv := newRawServer(t,
		rawPartialHead(total, "v1", payload[:total/2], true), // goes silent
		rawRange(total/2, payload[total/2:], total, "v1"),
	)

	started := time.Now()
	got, err := Download(context.Background(), ClientFor(srv.URL), srv.URL, testDownloadPolicy(), nil)
	elapsed := time.Since(started)
	if err != nil {
		t.Fatalf("Download: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("payload differs after stall recovery: %d bytes", len(got))
	}
	if elapsed > 1500*time.Millisecond {
		t.Fatalf("download took %v; the 150ms idle watchdog should have resumed it quickly", elapsed)
	}
	if reqs := srv.requests(); len(reqs) != 2 || reqs[1].Get("Range") == "" {
		t.Fatalf("second request must resume with a Range header, got %d requests", len(reqs))
	}
}

func TestDownloadRestartsWhenServerIgnoresRange(t *testing.T) {
	const total = 1000
	payload := distinctPayload(total)
	srv := newRawServer(t,
		rawPartialHead(total, "v1", payload[:total/2], false),
		rawOK(payload, "v1"), // full 200 answer to a Range request
	)

	got, err := Download(context.Background(), ClientFor(srv.URL), srv.URL, testDownloadPolicy(), nil)
	if err != nil {
		t.Fatalf("Download: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("got %d bytes, want exactly %d with no duplicated prefix", len(got), len(payload))
	}
	if reqs := srv.requests(); len(reqs) != 2 || reqs[1].Get("Range") == "" {
		t.Fatalf("the retry must be a Range request for this test to mean anything")
	}
}

func TestDownloadDoesNotRetryForbidden(t *testing.T) {
	srv := newRawServer(t, rawStatus("403 Forbidden"), rawOK([]byte("never reached"), "v1"))

	_, err := Download(context.Background(), ClientFor(srv.URL), srv.URL, testDownloadPolicy(), nil)
	if err == nil || !strings.Contains(err.Error(), "403") {
		t.Fatalf("err = %v, want an error mentioning 403", err)
	}
	if n := len(srv.requests()); n != 1 {
		t.Fatalf("requests = %d, want exactly 1 (403 is not transient)", n)
	}
}

func TestDownloadGivesUpAfterAttemptBudget(t *testing.T) {
	srv := newRawServer(t,
		rawStatus("503 Service Unavailable"),
		rawStatus("503 Service Unavailable"),
		rawStatus("503 Service Unavailable"),
		rawStatus("503 Service Unavailable"),
		rawOK([]byte("beyond the budget"), "v1"),
	)

	_, err := Download(context.Background(), ClientFor(srv.URL), srv.URL, testDownloadPolicy(), nil)
	if err == nil || !strings.Contains(err.Error(), "已尝试 4 次") {
		t.Fatalf("err = %v, want 已尝试 4 次", err)
	}
	if n := len(srv.requests()); n != 4 {
		t.Fatalf("requests = %d, want 4", n)
	}
}

func TestDownloadReportsCompletionProgress(t *testing.T) {
	const total = 500
	payload := distinctPayload(total)
	srv := newRawServer(t, rawPartialHead(total, "v1", payload[:200], false), rawRange(200, payload[200:], total, "v1"))

	var last DownloadEvent
	got, err := Download(context.Background(), ClientFor(srv.URL), srv.URL, testDownloadPolicy(), func(ev DownloadEvent) {
		if ev.Retry == nil {
			last = ev
		}
	})
	if err != nil || len(got) != total {
		t.Fatalf("Download = %d bytes, %v", len(got), err)
	}
	if last.Received != total || last.Total != total {
		t.Fatalf("last progress = received %d total %d, want %d/%d", last.Received, last.Total, total, total)
	}
}
