package platform

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestIsLoopbackURL(t *testing.T) {
	cases := map[string]bool{
		"http://localhost:8123/v1/images": true,
		"http://LOCALHOST/x":              true,
		"http://127.0.0.1:8080/x":         true,
		"http://[::1]:8080/x":             true,
		"https://api.example.com/v1":      false,
		"https://93.184.216.34/v1":        false,
		"http://::":                       false,
		"garbage":                         false,
		"":                                false,
	}
	for in, want := range cases {
		if got := IsLoopbackURL(in); got != want {
			t.Errorf("IsLoopbackURL(%q) = %v, want %v", in, got, want)
		}
	}
}

func TestDoHeaderTimeout(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		select {
		case <-time.After(200 * time.Millisecond):
		case <-r.Context().Done():
		}
		w.WriteHeader(http.StatusOK)
	}))
	t.Cleanup(srv.Close)

	req, _ := http.NewRequest(http.MethodGet, srv.URL, nil)
	_, err := Do(context.Background(), srv.Client(), req, Timeouts{Header: 50 * time.Millisecond})
	var timeout *TimeoutError
	if !errors.As(err, &timeout) {
		t.Fatalf("err = %v (%T), want *TimeoutError", err, err)
	}
	if !strings.Contains(timeout.Error(), "等待响应") {
		t.Fatalf("message %q does not mention 等待响应", timeout.Error())
	}
}

func TestDoIdleTimeoutOnStalledBody(t *testing.T) {
	release := make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Length", "100")
		io.WriteString(w, "partial")
		w.(http.Flusher).Flush()
		select {
		case <-release:
		case <-r.Context().Done():
		}
	}))
	t.Cleanup(srv.Close)
	t.Cleanup(func() { close(release) }) // runs before srv.Close

	req, _ := http.NewRequest(http.MethodGet, srv.URL, nil)
	resp, err := Do(context.Background(), srv.Client(), req, Timeouts{Idle: 50 * time.Millisecond})
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()

	data, err := io.ReadAll(resp.Body)
	var timeout *TimeoutError
	if !errors.As(err, &timeout) {
		t.Fatalf("read err = %v (%T), want *TimeoutError", err, err)
	}
	if !strings.Contains(timeout.Error(), "没有收到新数据") {
		t.Fatalf("message %q does not mention 没有收到新数据", timeout.Error())
	}
	if string(data) != "partial" {
		t.Fatalf("bytes before the stall = %q, want partial", data)
	}
}

func TestDoIdleTimeoutAllowsSlowButSteadyBody(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		flusher := w.(http.Flusher)
		for i := 0; i < 15; i++ { // 15 chunks, one every 20ms: 300ms in total
			io.WriteString(w, "chunk")
			flusher.Flush()
			time.Sleep(20 * time.Millisecond)
		}
	}))
	t.Cleanup(srv.Close)

	req, _ := http.NewRequest(http.MethodGet, srv.URL, nil)
	resp, err := Do(context.Background(), srv.Client(), req, Timeouts{Idle: 100 * time.Millisecond})
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("steady body must not time out: %v", err)
	}
	if got := string(data); got != strings.Repeat("chunk", 15) {
		t.Fatalf("body length = %d, want 75", len(got))
	}
}

func TestDoTotalTimeoutCapsWholeRequest(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		flusher := w.(http.Flusher)
		for i := 0; i < 50; i++ {
			if _, err := io.WriteString(w, "x"); err != nil {
				return
			}
			flusher.Flush()
			time.Sleep(20 * time.Millisecond)
		}
	}))
	t.Cleanup(srv.Close)

	req, _ := http.NewRequest(http.MethodGet, srv.URL, nil)
	resp, err := Do(context.Background(), srv.Client(), req, Timeouts{Total: 80 * time.Millisecond})
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	_, err = io.ReadAll(resp.Body)
	var timeout *TimeoutError
	if !errors.As(err, &timeout) || !strings.Contains(timeout.Error(), "总时限") {
		t.Fatalf("read err = %v, want a total-time-limit *TimeoutError", err)
	}
}

func TestTimeoutMessageShowsSubSecondLimit(t *testing.T) {
	// Expected: a sub-second limit is shown with its real value, never as zero.
	if got := seconds(150 * time.Millisecond); got == "0" {
		t.Fatalf("seconds(150ms) = %q; the user-facing message would read 超过 0 秒", got)
	}
}

func TestReadAllLimited(t *testing.T) {
	t.Run("declared length over limit", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Length", "100")
			w.Write([]byte(strings.Repeat("a", 100)))
		}))
		t.Cleanup(srv.Close)
		resp, err := srv.Client().Get(srv.URL)
		if err != nil {
			t.Fatal(err)
		}
		_, err = ReadAllLimited(resp, 10, "图片", nil)
		var tooLarge *TooLargeError
		if !errors.As(err, &tooLarge) || tooLarge.Label != "图片" {
			t.Fatalf("err = %v, want *TooLargeError labelled 图片", err)
		}
	})

	t.Run("chunked body grows past limit", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			flusher := w.(http.Flusher)
			for i := 0; i < 3; i++ {
				w.Write([]byte(strings.Repeat("b", 10)))
				flusher.Flush()
			}
		}))
		t.Cleanup(srv.Close)
		resp, err := srv.Client().Get(srv.URL)
		if err != nil {
			t.Fatal(err)
		}
		_, err = ReadAllLimited(resp, 25, "视频", nil)
		var tooLarge *TooLargeError
		if !errors.As(err, &tooLarge) {
			t.Fatalf("err = %v, want *TooLargeError", err)
		}
	})

	t.Run("within limit reports progress", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Write([]byte("hello world"))
		}))
		t.Cleanup(srv.Close)
		resp, err := srv.Client().Get(srv.URL)
		if err != nil {
			t.Fatal(err)
		}
		var last int64
		data, err := ReadAllLimited(resp, 1024, "图片", func(n int64) { last = n })
		if err != nil || string(data) != "hello world" {
			t.Fatalf("data = %q, err = %v", data, err)
		}
		if last != int64(len("hello world")) {
			t.Fatalf("final progress = %d, want 11", last)
		}
	})
}
