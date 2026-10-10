package store

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"zombiezen.com/go/sqlite"
)

// AssetRef is a produced file, as the frontend and the assets table know it.
type AssetRef struct {
	ID        string   `json:"id"`
	Kind      string   `json:"kind"` // "image" | "video" | "text"
	Path      string   `json:"path"`
	Width     *int     `json:"width,omitempty"`
	Height    *int     `json:"height,omitempty"`
	DurationS *float64 `json:"durationS,omitempty"`
	Format    string   `json:"format,omitempty"`
}

// AssetMeta is stored as the asset row's metadata JSON.
type AssetMeta struct {
	Source    string `json:"source"`
	Model     any    `json:"model"`     // string or null
	ProjectID any    `json:"projectId"` // string or null
	Params    any    `json:"params"`
}

// PersistAssets upserts assets in one transaction. A replayed write updates
// the row in place instead of replacing it, which would trip the ON DELETE
// RESTRICT references other tables hold.
func (db *DB) PersistAssets(ctx context.Context, assets []AssetRef, meta AssetMeta) error {
	if len(assets) == 0 {
		return nil
	}
	metadata, err := json.Marshal(meta)
	if err != nil {
		return fmt.Errorf("序列化资产历史失败：%w", err)
	}
	createdAt := time.Now().UnixMilli()
	err = db.Tx(ctx, func(conn *sqlite.Conn) error {
		stmt := conn.Prep(`INSERT INTO assets (id, kind, path, width, height, duration_s, format, created_at, metadata)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
			ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, path = excluded.path, width = excluded.width,
				height = excluded.height, duration_s = excluded.duration_s, format = excluded.format,
				metadata = excluded.metadata`)
		for _, a := range assets {
			stmt.BindText(1, a.ID)
			stmt.BindText(2, a.Kind)
			stmt.BindText(3, a.Path)
			bindOptionalInt(stmt, 4, a.Width)
			bindOptionalInt(stmt, 5, a.Height)
			if a.DurationS != nil {
				stmt.BindFloat(6, *a.DurationS)
			} else {
				stmt.BindNull(6)
			}
			if a.Format != "" {
				stmt.BindText(7, a.Format)
			} else {
				stmt.BindNull(7)
			}
			stmt.BindInt64(8, createdAt)
			stmt.BindText(9, string(metadata))
			if _, err := stmt.Step(); err != nil {
				return fmt.Errorf("写入资产历史失败：%w", err)
			}
			stmt.Reset()
		}
		return nil
	})
	if err != nil {
		return err
	}
	ids := make([]string, len(assets))
	for i, a := range assets {
		ids[i] = a.ID
	}
	slog.Info("history.backend_assets_persisted", "source", meta.Source, "assetCount", len(assets), "assetIds", ids, "projectId", meta.ProjectID)
	return nil
}

func bindOptionalInt(stmt *sqlite.Stmt, param int, v *int) {
	if v == nil {
		stmt.BindNull(param)
		return
	}
	stmt.BindInt64(param, int64(*v))
}

// GeneratedProvenance is the provenance record for an asset produced by a
// backend request (the REST API), matching what the frontend records.
func GeneratedProvenance(originalInput, generationInput string, systemInstruction *string, sourceMaterials, parentAssetIDs any) map[string]any {
	return map[string]any{
		"schemaVersion":     1,
		"originalInput":     originalInput,
		"generationInput":   generationInput,
		"systemInstruction": systemInstruction,
		"sourceMaterials":   sourceMaterials,
		"parentAssetIds":    parentAssetIDs,
		"revision":          map[string]any{"type": "generated"},
		"recordedAt":        time.Now().UnixMilli(),
	}
}

// HistoryChange tells the frontend that backend work added assets, so it
// reloads its library. Revisions let the backend observe the reload.
type HistoryChange struct {
	Revision  uint64   `json:"revision"`
	Operation string   `json:"operation"`
	AssetIDs  []string `json:"assetIds"`
	ChangedAt int64    `json:"changedAt"`
}

// HistoryChangedEvent is the event name the frontend listens to.
const HistoryChangedEvent = "history://changed"

// HistoryFeed numbers changes and tracks what the frontend acknowledged.
type HistoryFeed struct {
	mu            sync.Mutex
	emitted       uint64
	acknowledged  uint64
	listenerReady bool
	last          *HistoryChange
}

// Publish records a change and returns it for emission.
func (f *HistoryFeed) Publish(operation string, assetIDs []string) HistoryChange {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.emitted++
	change := HistoryChange{Revision: f.emitted, Operation: operation, AssetIDs: append([]string{}, assetIDs...), ChangedAt: time.Now().UnixMilli()}
	f.last = &change
	return change
}

// Acknowledge records the newest revision the frontend has reloaded.
func (f *HistoryFeed) Acknowledge(revision uint64) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.acknowledged = max(f.acknowledged, revision)
}

// MarkListenerReady records that the frontend subscribed to changes.
func (f *HistoryFeed) MarkListenerReady() {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.listenerReady = true
}

// Status reports the sync state (served by the REST API).
func (f *HistoryFeed) Status() map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	return map[string]any{
		"mode":                  "event_driven",
		"polling":               false,
		"frontendListenerReady": f.listenerReady,
		"emittedRevision":       f.emitted,
		"acknowledgedRevision":  f.acknowledged,
		"pendingRevision":       f.emitted - min(f.acknowledged, f.emitted),
		"lastChange":            f.last,
	}
}
