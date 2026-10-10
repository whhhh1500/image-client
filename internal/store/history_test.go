package store

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

func TestPersistAssetsInsertsAndUpsertsInPlace(t *testing.T) {
	db, _ := openTestDB(t)
	ctx := context.Background()
	w, h := 1024, 768
	dur := 2.5

	if err := db.PersistAssets(ctx, []AssetRef{
		{ID: "asset-1", Kind: "image", Path: "/data/assets/asset-1.png", Width: &w, Height: &h, Format: "png"},
		{ID: "asset-2", Kind: "video", Path: "/data/assets/asset-2.mp4", DurationS: &dur},
	}, AssetMeta{
		Source: "generate", Model: "gpt-image-2", ProjectID: "p1", Params: map[string]any{"size": "1024x1024"},
	}); err != nil {
		t.Fatalf("PersistAssets insert: %v", err)
	}

	rows, err := db.FrontendSelect(ctx, "SELECT id, kind, width, duration_s FROM assets ORDER BY id", nil)
	if err != nil || len(rows) != 2 {
		t.Fatalf("rows = %v, err %v; want 2 assets", rows, err)
	}
	if rows[0]["width"] != int64(1024) || rows[1]["width"] != nil || rows[1]["duration_s"] != 2.5 {
		t.Fatalf("typed columns wrong: %v", rows)
	}

	// Re-persisting the same id must update the row, not add or replace it.
	if err := db.PersistAssets(ctx, []AssetRef{
		{ID: "asset-1", Kind: "text", Path: "/data/assets/asset-1.md"},
	}, AssetMeta{Source: "edit"}); err != nil {
		t.Fatalf("PersistAssets replay: %v", err)
	}
	rows, err = db.FrontendSelect(ctx, "SELECT kind, path, width, format FROM assets WHERE id = ?", binds(`"asset-1"`))
	if err != nil || len(rows) != 1 {
		t.Fatalf("replayed asset rows = %v, err %v; want exactly one", rows, err)
	}
	if rows[0]["kind"] != "text" || rows[0]["path"] != "/data/assets/asset-1.md" {
		t.Fatalf("kind/path not updated in place: %v", rows[0])
	}
	if rows[0]["width"] != nil || rows[0]["format"] != nil {
		t.Fatalf("optional columns must be cleared by the replay: %v", rows[0])
	}
	all, err := db.FrontendSelect(ctx, "SELECT id FROM assets", nil)
	if err != nil || len(all) != 2 {
		t.Fatalf("total assets = %d, err %v; want still 2", len(all), err)
	}
}

func TestPersistAssetsMetadataJSON(t *testing.T) {
	db, _ := openTestDB(t)
	ctx := context.Background()
	if err := db.PersistAssets(ctx, []AssetRef{{ID: "asset-m", Kind: "image", Path: "/x.png"}}, AssetMeta{
		Source: "generate", Model: "gpt-image-2", ProjectID: "p1", Params: map[string]any{"size": "1024x1024"},
	}); err != nil {
		t.Fatal(err)
	}
	rows, err := db.FrontendSelect(ctx, "SELECT metadata FROM assets WHERE id = ?", binds(`"asset-m"`))
	if err != nil || len(rows) != 1 {
		t.Fatalf("metadata rows = %v, err %v", rows, err)
	}
	text, ok := rows[0]["metadata"].(string)
	if !ok {
		t.Fatalf("metadata column is %T, want JSON text", rows[0]["metadata"])
	}
	var meta map[string]any
	if err := json.Unmarshal([]byte(text), &meta); err != nil {
		t.Fatalf("metadata is not JSON: %v", err)
	}
	if meta["source"] != "generate" || meta["model"] != "gpt-image-2" || meta["projectId"] != "p1" {
		t.Fatalf("metadata = %v", meta)
	}
	params, _ := meta["params"].(map[string]any)
	if params["size"] != "1024x1024" {
		t.Fatalf("params = %v", meta["params"])
	}
}

func TestPersistAssetsNoopForEmptyList(t *testing.T) {
	db, _ := openTestDB(t)
	if err := db.PersistAssets(context.Background(), nil, AssetMeta{Source: "x"}); err != nil {
		t.Fatalf("empty persist: %v", err)
	}
}

func TestHistoryFeedRevisionsAndAcknowledgement(t *testing.T) {
	var feed HistoryFeed

	first := feed.Publish("generate", []string{"a"})
	second := feed.Publish("import", nil)
	if first.Revision != 1 || second.Revision != 2 {
		t.Fatalf("revisions = %d, %d; want 1, 2", first.Revision, second.Revision)
	}
	if len(first.AssetIDs) != 1 || first.AssetIDs[0] != "a" || first.Operation != "generate" {
		t.Fatalf("first change = %+v", first)
	}
	if second.AssetIDs == nil {
		t.Fatal("nil asset list must serialise as [] not null")
	}
	if body, _ := json.Marshal(second); string(body) == "" || !strings.Contains(string(body), `"assetIds":[]`) {
		t.Fatalf("second change JSON = %s, want assetIds []", body)
	}

	status := feed.Status()
	if status["pendingRevision"] != uint64(2) || status["emittedRevision"] != uint64(2) {
		t.Fatalf("status before ack = %v", status)
	}
	if status["lastChange"] == nil {
		t.Fatal("lastChange must be set after a publish")
	}

	feed.Acknowledge(1)
	if got := feed.Status()["pendingRevision"]; got != uint64(1) {
		t.Fatalf("pending after ack 1 = %v, want 1", got)
	}
	feed.Acknowledge(0) // older acknowledgements must not move the watermark back
	if got := feed.Status()["acknowledgedRevision"]; got != uint64(1) {
		t.Fatalf("acknowledged = %v after a lower ack, want 1", got)
	}
	feed.Acknowledge(2)
	if got := feed.Status()["pendingRevision"]; got != uint64(0) {
		t.Fatalf("pending after ack 2 = %v, want 0", got)
	}
}

func TestHistoryFeedStatusBeforeAnyChange(t *testing.T) {
	var feed HistoryFeed
	feed.MarkListenerReady()
	status := feed.Status()
	if status["mode"] != "event_driven" || status["polling"] != false {
		t.Fatalf("mode/polling = %v/%v", status["mode"], status["polling"])
	}
	if status["frontendListenerReady"] != true || status["pendingRevision"] != uint64(0) {
		t.Fatalf("status = %v", status)
	}
	if HistoryChangedEvent != "history://changed" {
		t.Fatalf("event name = %q", HistoryChangedEvent)
	}
}

func TestGeneratedProvenanceShape(t *testing.T) {
	system := "be concise"
	prov := GeneratedProvenance("draw a cat", "draw a cat, photo", &system, []string{"m1"}, []string{"p1"})
	if prov["schemaVersion"] != 1 || prov["originalInput"] != "draw a cat" || prov["generationInput"] != "draw a cat, photo" {
		t.Fatalf("provenance = %v", prov)
	}
	if prov["systemInstruction"] != &system {
		t.Fatalf("systemInstruction must be carried through")
	}
	revision, _ := prov["revision"].(map[string]any)
	if revision["type"] != "generated" {
		t.Fatalf("revision = %v", prov["revision"])
	}
	if _, ok := prov["recordedAt"].(int64); !ok {
		t.Fatalf("recordedAt = %T, want int64 milliseconds", prov["recordedAt"])
	}
}
