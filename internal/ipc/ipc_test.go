package ipc

import (
	"context"
	"encoding/json"
	"errors"
	"reflect"
	"strings"
	"testing"
)

type echoArgs struct {
	Name  string `json:"name"`
	Count int    `json:"count"`
}

func TestInvokeUnknownCommand(t *testing.T) {
	r := NewRouter()
	_, err := r.Invoke(context.Background(), "missing_command", nil)
	if err == nil || !strings.Contains(err.Error(), "未知命令") {
		t.Fatalf("err = %v, want 未知命令", err)
	}
}

func TestTypedDecodesArgumentObject(t *testing.T) {
	r := NewRouter()
	r.Handle("echo", Typed(func(_ context.Context, in echoArgs) (echoArgs, error) { return in, nil }))

	got, err := r.Invoke(context.Background(), "echo", json.RawMessage(`{"name":"x","count":3}`))
	if err != nil {
		t.Fatalf("Invoke: %v", err)
	}
	if want := (echoArgs{Name: "x", Count: 3}); !reflect.DeepEqual(got, want) {
		t.Fatalf("result = %#v, want %#v", got, want)
	}
}

func TestTypedAcceptsEmptyAndNullArguments(t *testing.T) {
	r := NewRouter()
	r.Handle("echo", Typed(func(_ context.Context, in echoArgs) (echoArgs, error) { return in, nil }))

	for _, args := range []json.RawMessage{nil, json.RawMessage(""), json.RawMessage("null"), json.RawMessage("  null  ")} {
		got, err := r.Invoke(context.Background(), "echo", args)
		if err != nil {
			t.Fatalf("args %q: %v", args, err)
		}
		if got != (echoArgs{}) {
			t.Fatalf("args %q: result = %#v, want zero value", args, got)
		}
	}
}

func TestTypedRejectsInvalidJSON(t *testing.T) {
	r := NewRouter()
	r.Handle("echo", Typed(func(_ context.Context, in echoArgs) (echoArgs, error) { return in, nil }))

	_, err := r.Invoke(context.Background(), "echo", json.RawMessage(`{"name":`))
	if err == nil || !strings.Contains(err.Error(), "命令参数无效") {
		t.Fatalf("err = %v, want 命令参数无效", err)
	}
}

func TestInputWrapperDecodesInputKey(t *testing.T) {
	r := NewRouter()
	r.Handle("wrapped", Typed(func(_ context.Context, in Input[echoArgs]) (string, error) {
		return in.Input.Name, nil
	}))
	got, err := r.Invoke(context.Background(), "wrapped", json.RawMessage(`{"input":{"name":"deep"}}`))
	if err != nil || got != "deep" {
		t.Fatalf("got %v, %v; want deep", got, err)
	}
}

func TestVoidReturnsNilResult(t *testing.T) {
	r := NewRouter()
	called := false
	r.Handle("void", Void(func(_ context.Context, _ struct{}) error {
		called = true
		return nil
	}))
	got, err := r.Invoke(context.Background(), "void", nil)
	if err != nil || got != nil {
		t.Fatalf("Void result = %v, %v; want nil, nil", got, err)
	}
	if !called {
		t.Fatal("handler not called")
	}
}

func TestVoidPropagatesError(t *testing.T) {
	r := NewRouter()
	boom := errors.New("disk full")
	r.Handle("void", Void(func(_ context.Context, _ struct{}) error { return boom }))
	if _, err := r.Invoke(context.Background(), "void", nil); !errors.Is(err, boom) {
		t.Fatalf("err = %v, want the handler error", err)
	}
}

func TestPanickingHandlerBecomesError(t *testing.T) {
	r := NewRouter()
	r.Handle("boom", func(context.Context, json.RawMessage) (any, error) { panic("kaboom") })
	r.Handle("ok", func(context.Context, json.RawMessage) (any, error) { return "fine", nil })

	_, err := r.Invoke(context.Background(), "boom", nil)
	if err == nil || !strings.Contains(err.Error(), "内部错误") || !strings.Contains(err.Error(), "kaboom") {
		t.Fatalf("err = %v, want an internal-error message naming the panic", err)
	}
	// The router keeps serving after a panic.
	if got, err := r.Invoke(context.Background(), "ok", nil); err != nil || got != "fine" {
		t.Fatalf("router broken after panic: %v, %v", got, err)
	}
}

func TestDuplicateHandlePanics(t *testing.T) {
	r := NewRouter()
	h := func(context.Context, json.RawMessage) (any, error) { return nil, nil }
	r.Handle("dup", h)

	defer func() {
		p := recover()
		if p == nil {
			t.Fatal("registering the same command twice must panic")
		}
	}()
	r.Handle("dup", h)
}

func TestNamesSorted(t *testing.T) {
	r := NewRouter()
	h := func(context.Context, json.RawMessage) (any, error) { return nil, nil }
	for _, name := range []string{"zeta", "alpha", "mid"} {
		r.Handle(name, h)
	}
	want := []string{"alpha", "mid", "zeta"}
	if got := r.Names(); !reflect.DeepEqual(got, want) {
		t.Fatalf("Names() = %v, want %v", got, want)
	}
}

func TestEmitterFuncAdapter(t *testing.T) {
	var gotEvent string
	var gotPayload any
	var e Emitter = EmitterFunc(func(event string, payload any) { gotEvent, gotPayload = event, payload })
	e.Emit("history://changed", 7)
	if gotEvent != "history://changed" || gotPayload != 7 {
		t.Fatalf("emit delivered %q %v", gotEvent, gotPayload)
	}
	Discard.Emit("ignored", nil) // must not panic
}
