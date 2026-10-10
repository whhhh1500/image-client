// Package ipc is the boundary with the frontend: named commands with JSON
// arguments, and events pushed the other way.
//
// Commands keep the names and argument shapes the frontend already uses
// (snake_case names, camelCase argument keys), so the UI does not depend on
// how the backend is organised.
package ipc

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"maps"
	"slices"
	"sync"
	"time"
)

// Handler serves one command. args is the JSON object the frontend sent.
type Handler func(ctx context.Context, args json.RawMessage) (any, error)

// Router dispatches commands by name.
type Router struct {
	mu       sync.RWMutex
	handlers map[string]Handler
}

func NewRouter() *Router { return &Router{handlers: map[string]Handler{}} }

// Handle registers h; registering a name twice is a programming error.
func (r *Router) Handle(name string, h Handler) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, dup := r.handlers[name]; dup {
		panic("ipc: duplicate command " + name)
	}
	r.handlers[name] = h
}

// Names lists the registered commands, sorted.
func (r *Router) Names() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return slices.Sorted(maps.Keys(r.handlers))
}

// Invoke runs a command. Panics become errors so one bad request cannot take
// the application down.
func (r *Router) Invoke(ctx context.Context, name string, args json.RawMessage) (result any, err error) {
	r.mu.RLock()
	h, ok := r.handlers[name]
	r.mu.RUnlock()
	if !ok {
		return nil, fmt.Errorf("未知命令：%s", name)
	}
	defer func() {
		if p := recover(); p != nil {
			slog.Error("ipc.command.panic", "command", name, "panic", fmt.Sprint(p))
			err = fmt.Errorf("命令 %s 内部错误：%v", name, p)
		}
	}()
	return h(ctx, args)
}

// Typed adapts fn to a Handler, decoding the argument object into In.
// Use struct{} for commands without arguments.
func Typed[In, Out any](fn func(context.Context, In) (Out, error)) Handler {
	return func(ctx context.Context, args json.RawMessage) (any, error) {
		var in In
		if trimmed := bytes.TrimSpace(args); len(trimmed) > 0 && !bytes.Equal(trimmed, []byte("null")) {
			if err := json.Unmarshal(trimmed, &in); err != nil {
				return nil, fmt.Errorf("命令参数无效：%w", err)
			}
		}
		return fn(ctx, in)
	}
}

// Void adapts a command that returns nothing; the frontend receives null.
func Void[In any](fn func(context.Context, In) error) Handler {
	return Typed(func(ctx context.Context, in In) (any, error) { return nil, fn(ctx, in) })
}

// Input wraps commands whose single argument is named "input".
type Input[T any] struct {
	Input T `json:"input"`
}

// Emitter pushes events to the frontend.
type Emitter interface {
	Emit(event string, payload any)
}

// EmitterFunc adapts a function to Emitter.
type EmitterFunc func(event string, payload any)

func (f EmitterFunc) Emit(event string, payload any) { f(event, payload) }

// Discard drops events; for callers without a window (tests, REST-only work).
var Discard Emitter = EmitterFunc(func(string, any) {})

// Logged wraps a router call with the request timing log the frontend's
// loggedInvoke used to provide on the backend side.
func Logged(name string, started time.Time, err error) {
	if err != nil {
		slog.Debug("ipc.command.end", "command", name, "status", "error", "durationMs", time.Since(started).Milliseconds())
		return
	}
	slog.Debug("ipc.command.end", "command", name, "status", "success", "durationMs", time.Since(started).Milliseconds())
}
