package comic

import (
	"bytes"
	"cmp"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"slices"
	"strconv"
	"strings"

	"zombiezen.com/go/sqlite"
)

// Lineage: every document stores a receipt of the upstream content it was
// written against (its "dependencies" column). Comparing that receipt with
// one computed from today's content tells which documents may be stale.
// Revision numbers stay the optimistic-concurrency tokens; receipts only
// carry content hashes.

// receiptEntry is one upstream input of a receipt.
type receiptEntry struct {
	Hash  *string `json:"hash"`
	Label string  `json:"label"`
}

type receipt struct {
	Version uint8                   `json:"version"`
	Values  map[string]receiptEntry `json:"values"`
}

func newReceipt() receipt { return receipt{Version: 2, Values: map[string]receiptEntry{}} }

func (e receiptEntry) equal(o receiptEntry) bool {
	return strEqual(e.Hash, o.Hash) && e.Label == o.Label
}

// parseReceipt accepts exactly what the earlier release's strict decoder
// accepted: an object with a numeric version and a values map whose entries
// carry a string label and an optional string hash.
func parseReceipt(raw string) (receipt, bool) {
	var top map[string]json.RawMessage
	if json.Unmarshal([]byte(raw), &top) != nil || top == nil {
		return receipt{}, false
	}
	var r receipt
	version, ok := top["version"]
	if !ok || isJSONNull(version) || json.Unmarshal(version, &r.Version) != nil {
		return receipt{}, false
	}
	values, ok := top["values"]
	var entries map[string]json.RawMessage
	if !ok || isJSONNull(values) || json.Unmarshal(values, &entries) != nil {
		return receipt{}, false
	}
	r.Values = make(map[string]receiptEntry, len(entries))
	for key, rawEntry := range entries {
		var fields map[string]json.RawMessage
		if isJSONNull(rawEntry) || json.Unmarshal(rawEntry, &fields) != nil {
			return receipt{}, false
		}
		var entry receiptEntry
		label, ok := fields["label"]
		if !ok || isJSONNull(label) || json.Unmarshal(label, &entry.Label) != nil {
			return receipt{}, false
		}
		if hash, ok := fields["hash"]; ok && !isJSONNull(hash) {
			var h string
			if json.Unmarshal(hash, &h) != nil {
				return receipt{}, false
			}
			entry.Hash = &h
		}
		r.Values[key] = entry
	}
	return r, true
}

func isJSONNull(raw json.RawMessage) bool { return bytes.Equal(bytes.TrimSpace(raw), []byte("null")) }

type bookChapter struct {
	id       string
	sequence int64
	number   int64
	title    *string
	sourceID *string
	content  string
}

type bookDoc struct {
	doc            Document
	chapter        string // "" for work-level settings
	receipt        string
	stateText      string
	stateHash      string
	supplement     string
	supplementHash string
}

type historic struct {
	revision       int64
	markdown       string
	receipt        string
	time           int64
	stateHash      string
	supplementHash string
}

// book is one consistent snapshot of a whole novel work's chapters and comic
// documents, with stale state resolved.
type book struct {
	scope    Scope
	chapters []bookChapter
	docs     []bookDoc
	history  map[string][]historic // only loaded for legacy receipts
	sources  map[string]string     // only loaded for legacy receipts
}

// characterState is the part of a page prompt that later pages inherit.
func characterState(md string) string {
	parts := make([]string, 0, 3)
	for _, name := range []string{"人物锚点", "人物锚点补充", "连续性要求"} {
		parts = append(parts, fmt.Sprintf("### %s\n%s", name, sectionOrEmpty(md, name)))
	}
	return strings.Join(parts, "\n\n")
}

func loadBook(conn *sqlite.Conn, s Scope) (*book, error) {
	if _, _, err := chapterSource(conn, s); err != nil {
		return nil, err
	}
	b := &book{scope: s}
	err := querySQL(conn, `SELECT ch.id,ch.sequence_no,ch.chapter_no,ch.title,ch.current_revision_id,COALESCE(r.content,'') FROM novel_chapters ch LEFT JOIN novel_chapter_revisions r ON r.id=ch.current_revision_id AND r.novel_chapter_id=ch.id WHERE ch.novel_work_id=? ORDER BY ch.sequence_no,ch.id`,
		func(stmt *sqlite.Stmt) {
			b.chapters = append(b.chapters, bookChapter{
				id: stmt.ColumnText(0), sequence: stmt.ColumnInt64(1), number: stmt.ColumnInt64(2),
				title: columnOptText(stmt, 3), sourceID: columnOptText(stmt, 4), content: stmt.ColumnText(5),
			})
		}, s.NovelWorkID)
	if err != nil {
		return nil, err
	}
	err = querySQL(conn, `SELECT id,chapter_id,kind,page_no,markdown,revision,dependencies,updated_at,optimization_instruction FROM comic_md_documents WHERE novel_work_id=? AND project_id=?`,
		func(stmt *sqlite.Stmt) {
			markdown, kind := stmt.ColumnText(4), stmt.ColumnText(2)
			var pageNo *int64
			if n := stmt.ColumnInt64(3); n != 0 {
				pageNo = &n
			}
			state := characterState(markdown)
			supplement := sectionOrEmpty(markdown, "人物锚点补充")
			b.docs = append(b.docs, bookDoc{
				chapter: stmt.ColumnText(1), receipt: stmt.ColumnText(6),
				stateText: state, stateHash: contentHash(state),
				supplement: supplement, supplementHash: contentHash(supplement),
				doc: Document{
					ID: stmt.ColumnText(0), Kind: kind, PageNo: pageNo, Markdown: markdown,
					OptimizationInstruction: stmt.ColumnText(8), Revision: stmt.ColumnInt64(5),
					ContentHash: contentHash(markdown), StaleReasons: []string{},
					Issues: validate(kind, pageNo, markdown), UpdatedAt: stmt.ColumnInt64(7),
				},
			})
		}, s.NovelWorkID, s.ProjectID)
	if err != nil {
		return nil, err
	}
	slices.SortStableFunc(b.docs, func(x, y bookDoc) int {
		return cmp.Or(cmp.Compare(b.docOrder(x), b.docOrder(y)),
			cmp.Compare(stageRank(x.doc.Kind), stageRank(y.doc.Kind)),
			optCompare(x.doc.PageNo, y.doc.PageNo))
	})

	legacy := slices.ContainsFunc(b.docs, func(d bookDoc) bool { _, ok := parseReceipt(d.receipt); return !ok })
	if legacy {
		if err := b.loadHistory(conn); err != nil {
			return nil, err
		}
	}

	for i := range b.docs {
		r := b.docs[i] // decided against the state before this document changes
		d := r.doc
		outOfPlan := false
		if d.Kind == kindPagePrompt {
			if plan, ok := b.pagePlan(r.chapter); ok && !slices.Contains(plan, orZero(d.PageNo)) {
				outOfPlan = true
			}
		}
		current := b.receipt(r.chapter, d.Kind, d.PageNo)
		reasons := []string{}
		add := func(reason string) {
			if !slices.Contains(reasons, reason) {
				reasons = append(reasons, reason)
			}
		}
		if old, ok := b.decode(r); ok {
			keys := append(sortedKeys(old.Values), sortedKeys(current.Values)...)
			for _, key := range keys {
				o, inOld := old.Values[key]
				c, inCurrent := current.Values[key]
				if inOld == inCurrent && (!inOld || o.equal(c)) {
					continue
				}
				label := "关联资料"
				if inCurrent {
					label = c.Label
				} else if inOld {
					label = o.Label
				}
				add(label + "已变化，可能受影响，请同步或核对")
			}
		} else {
			add("历史关联资料不可核验，请同步或核对")
		}
		for _, up := range b.upstream(r.chapter, d.Kind, d.PageNo) {
			if up.doc.Stale {
				add(documentLabel(up.doc.Kind, up.doc.PageNo) + "尚未更新，可能受影响，请先同步或核对上游")
			}
		}
		b.docs[i].doc.Stale = len(reasons) > 0
		b.docs[i].doc.StaleReasons = reasons
		b.docs[i].doc.OutOfPlan = outOfPlan
	}
	return b, nil
}

func sortedKeys(m map[string]receiptEntry) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	slices.Sort(keys)
	return keys
}

// docOrder sorts settings first, then by chapter sequence; documents of an
// unknown chapter go last.
func (b *book) docOrder(d bookDoc) int64 {
	if d.doc.Kind == kindSettings {
		return math.MinInt64
	}
	for _, ch := range b.chapters {
		if ch.id == d.chapter {
			return ch.sequence
		}
	}
	return math.MaxInt64
}

func (b *book) loadHistory(conn *sqlite.Conn) error {
	b.history = map[string][]historic{}
	err := querySQL(conn, `SELECT r.document_id,r.revision,r.markdown,r.dependencies,r.created_at FROM comic_md_revisions r JOIN comic_md_documents d ON d.id=r.document_id WHERE d.novel_work_id=? AND d.project_id=? ORDER BY r.revision`,
		func(stmt *sqlite.Stmt) {
			markdown := stmt.ColumnText(2)
			id := stmt.ColumnText(0)
			b.history[id] = append(b.history[id], historic{
				revision: stmt.ColumnInt64(1), markdown: markdown, receipt: stmt.ColumnText(3), time: stmt.ColumnInt64(4),
				stateHash:      contentHash(characterState(markdown)),
				supplementHash: contentHash(sectionOrEmpty(markdown, "人物锚点补充")),
			})
		}, b.scope.NovelWorkID, b.scope.ProjectID)
	if err != nil {
		return err
	}
	b.sources = map[string]string{}
	return querySQL(conn, `SELECT r.id,r.content FROM novel_chapter_revisions r JOIN novel_chapters ch ON ch.id=r.novel_chapter_id WHERE ch.novel_work_id=?`,
		func(stmt *sqlite.Stmt) { b.sources[stmt.ColumnText(0)] = stmt.ColumnText(1) }, b.scope.NovelWorkID)
}

func (b *book) chapter(id string) (bookChapter, bool) {
	for _, ch := range b.chapters {
		if ch.id == id {
			return ch, true
		}
	}
	return bookChapter{}, false
}

func (b *book) sequence(chapter string) int64 {
	if ch, ok := b.chapter(chapter); ok {
		return ch.sequence
	}
	return math.MinInt64
}

func (b *book) chapterNumber(chapter string) int64 {
	if ch, ok := b.chapter(chapter); ok {
		return ch.number
	}
	return 0
}

// pagePlan is the page list of the chapter's complete storyboard, if any.
func (b *book) pagePlan(chapter string) ([]int64, bool) {
	for _, r := range b.docs {
		if r.chapter == chapter && r.doc.Kind == kindStoryboard && len(r.doc.Issues) == 0 {
			return pageNumbers(r.doc.Markdown), true
		}
	}
	return nil, false
}

// upstream lists the documents a document of kind/page in chapter depends on:
// work settings, earlier stages of the chapter, earlier chapters' scripts and
// in-plan page prompts before it (earlier chapters, or earlier pages).
func (b *book) upstream(chapter, kind string, pageNo *int64) []*bookDoc {
	if kind == kindSettings {
		return nil
	}
	var out []*bookDoc
	for i := range b.docs {
		r := &b.docs[i]
		d := r.doc
		earlierChapter := b.sequence(r.chapter) < b.sequence(chapter)
		if d.Kind == kindSettings ||
			(r.chapter == chapter && stageRank(d.Kind) < stageRank(kind)) ||
			(earlierChapter && d.Kind == kindScript) ||
			(d.Kind == kindPagePrompt && !d.OutOfPlan &&
				(earlierChapter || (kind == kindPagePrompt && r.chapter == chapter && optCompare(d.PageNo, pageNo) < 0))) {
			out = append(out, r)
		}
	}
	return out
}

func (b *book) stateLabel(r *bookDoc) string {
	return fmt.Sprintf("第%d章第%d页人物状态", b.chapterNumber(r.chapter), orZero(r.doc.PageNo))
}

// receipt describes today's upstream content of a document.
func (b *book) receipt(chapter, kind string, pageNo *int64) receipt {
	out := newReceipt()
	// Settings are work-scoped and intentionally stay valid when a chapter
	// source changes.
	if kind == kindSettings {
		return out
	}
	for _, ch := range b.chapters {
		if ch.id == chapter {
			var h *string
			if ch.sourceID != nil {
				h = ptr(contentHash(ch.content))
			}
			out.Values["source:"+ch.id] = receiptEntry{Hash: h, Label: "章节正文"}
		}
	}
	for _, k := range []string{kindSettings, kindScript, kindStoryboard} {
		if stageRank(k) >= stageRank(kind) {
			break
		}
		key := chapter
		if k == kindSettings {
			key = ""
		}
		var h *string
		for _, r := range b.docs {
			if r.chapter == key && r.doc.Kind == k {
				h = ptr(r.doc.ContentHash)
				break
			}
		}
		out.Values["doc:"+key+":"+k] = receiptEntry{Hash: h, Label: documentLabel(k, nil)}
	}
	for _, ch := range b.chapters {
		if ch.sequence >= b.sequence(chapter) {
			continue
		}
		var h *string
		for _, r := range b.docs {
			if r.chapter == ch.id && r.doc.Kind == kindScript {
				h = ptr(r.supplementHash)
				break
			}
		}
		out.Values["doc:"+ch.id+":script"] = receiptEntry{Hash: h, Label: fmt.Sprintf("第%d章剧本", ch.number)}
	}
	for _, r := range b.upstream(chapter, kind, pageNo) {
		if r.doc.Kind == kindPagePrompt {
			out.Values["state:"+r.doc.ID] = receiptEntry{Hash: ptr(r.stateHash), Label: b.stateLabel(r)}
		}
	}
	return out
}

// decode reads a stored receipt. Version-2 receipts are used as stored;
// legacy ones (a JSON array of revision references) are translated into
// content hashes from the revision history. Anything unverifiable is
// reported as false, which conservatively marks the document stale.
func (b *book) decode(r bookDoc) (receipt, bool) {
	if stored, ok := parseReceipt(r.receipt); ok {
		return stored, stored.Version == 2
	}
	old, err := b.decodeLegacy(r)
	return old, err == nil
}

var errUnverifiable = errors.New("unverifiable legacy receipt")

func (b *book) decodeLegacy(r bookDoc) (receipt, error) {
	dec := json.NewDecoder(strings.NewReader(r.receipt))
	dec.UseNumber()
	var items []any
	if dec.Decode(&items) != nil || items == nil {
		return receipt{}, errUnverifiable
	}
	if _, err := dec.Token(); err != io.EOF {
		return receipt{}, errUnverifiable // trailing content
	}
	old := newReceipt()
	if r.doc.Kind == kindSettings {
		return old, nil
	}
	current := b.receipt(r.chapter, r.doc.Kind, r.doc.PageNo)
	labelOf := func(key string) (string, error) {
		entry, ok := current.Values[key]
		if !ok {
			return "", errUnverifiable
		}
		return entry.Label, nil
	}
	for _, item := range items {
		a, ok := item.([]any)
		if !ok || len(a) == 0 {
			return receipt{}, errUnverifiable
		}
		key, ok := a[0].(string)
		if !ok {
			return receipt{}, errUnverifiable
		}
		switch key {
		case "source":
			if len(a) < 2 {
				return receipt{}, errUnverifiable
			}
			var h *string
			if a[1] != nil {
				id, ok := a[1].(string)
				if !ok {
					return receipt{}, errUnverifiable
				}
				content, ok := b.sources[id]
				if !ok {
					return receipt{}, errUnverifiable
				}
				h = ptr(contentHash(content))
			}
			k := "source:" + r.chapter
			label, err := labelOf(k)
			if err != nil {
				return receipt{}, err
			}
			old.Values[k] = receiptEntry{Hash: h, Label: label}
		case kindSettings, kindScript, kindStoryboard:
			if len(a) < 2 {
				return receipt{}, errUnverifiable
			}
			var h *string
			if a[1] != nil {
				ref, ok := a[1].([]any)
				if !ok || len(ref) < 2 {
					return receipt{}, errUnverifiable
				}
				id, ok1 := ref[0].(string)
				rev, ok2 := jsonInt(ref[1])
				if !ok1 || !ok2 {
					return receipt{}, errUnverifiable
				}
				hashed, ok := b.historicalHash(id, rev)
				if !ok {
					return receipt{}, errUnverifiable
				}
				h = &hashed
			}
			ch := r.chapter
			if key == kindSettings {
				ch = ""
			}
			k := "doc:" + ch + ":" + key
			label, err := labelOf(k)
			if err != nil {
				return receipt{}, err
			}
			old.Values[k] = receiptEntry{Hash: h, Label: label}
		default: // [chapterId, sourceRevision, scriptDocumentId|null, scriptRevision]
			if len(a) < 3 {
				return receipt{}, errUnverifiable
			}
			var h *string
			if a[2] != nil {
				if len(a) < 4 {
					return receipt{}, errUnverifiable
				}
				rev, ok := jsonInt(a[3])
				if !ok {
					return receipt{}, errUnverifiable
				}
				id, ok := a[2].(string)
				if !ok {
					return receipt{}, errUnverifiable
				}
				found := false
				for _, hist := range b.history[id] {
					if hist.revision == rev {
						h, found = ptr(hist.supplementHash), true
						break
					}
				}
				if !found {
					return receipt{}, errUnverifiable
				}
			}
			k := "doc:" + key + ":script"
			label, err := labelOf(k)
			if err != nil {
				return receipt{}, err
			}
			old.Values[k] = receiptEntry{Hash: h, Label: label}
		}
	}
	// Legacy receipts had no page-state entries. Recover them as of the saved
	// content's original timestamp, never from today's content (which would
	// erase changes).
	if err := b.recoverLegacyStates(r, &old); err != nil {
		return receipt{}, err
	}
	return old, nil
}

func (b *book) recoverLegacyStates(r bookDoc, old *receipt) error {
	own, ok := b.history[r.doc.ID]
	if !ok {
		return errUnverifiable
	}
	i := slices.IndexFunc(own, func(h historic) bool { return h.markdown == r.doc.Markdown && h.receipt == r.receipt })
	if i < 0 {
		return errUnverifiable
	}
	asOf := own[i].time
	for ui := range b.docs {
		up := &b.docs[ui]
		if up.doc.Kind != kindPagePrompt {
			continue
		}
		before := b.sequence(up.chapter) < b.sequence(r.chapter) ||
			(r.doc.Kind == kindPagePrompt && up.chapter == r.chapter && optCompare(up.doc.PageNo, r.doc.PageNo) < 0)
		if !before {
			continue
		}
		hist, ok := b.history[up.doc.ID]
		if !ok {
			return errUnverifiable
		}
		h, found := latestAsOf(hist, asOf)
		if !found {
			continue
		}
		if slices.ContainsFunc(hist, func(o historic) bool { return o.time == h.time && o.stateHash != h.stateHash }) {
			return errUnverifiable
		}
		if bi := slices.IndexFunc(b.docs, func(d bookDoc) bool { return d.chapter == up.chapter && d.doc.Kind == kindStoryboard }); bi >= 0 {
			boardHist, ok := b.history[b.docs[bi].doc.ID]
			if !ok {
				return errUnverifiable
			}
			if hb, found := latestAsOf(boardHist, asOf); found {
				if slices.ContainsFunc(boardHist, func(o historic) bool { return o.time == hb.time && o.markdown != hb.markdown }) {
					return errUnverifiable
				}
				if len(validate(kindStoryboard, nil, hb.markdown)) == 0 && !slices.ContainsFunc(pageNumbers(hb.markdown), func(p int64) bool {
					return up.doc.PageNo != nil && *up.doc.PageNo == p
				}) {
					continue // the page was not in that storyboard's plan then
				}
			}
		}
		old.Values["state:"+up.doc.ID] = receiptEntry{Hash: ptr(h.stateHash), Label: b.stateLabel(up)}
	}
	return nil
}

// latestAsOf is the newest revision recorded at or before t.
func latestAsOf(hist []historic, t int64) (historic, bool) {
	for i := len(hist) - 1; i >= 0; i-- {
		if hist[i].time <= t {
			return hist[i], true
		}
	}
	return historic{}, false
}

func jsonInt(v any) (int64, bool) {
	n, ok := v.(json.Number)
	if !ok {
		return 0, false
	}
	i, err := strconv.ParseInt(n.String(), 10, 64)
	return i, err == nil
}

func (b *book) historicalHash(id string, revision int64) (string, bool) {
	for _, h := range b.history[id] {
		if h.revision == revision {
			return contentHash(h.markdown), true
		}
	}
	return "", false
}

// documents lists work settings plus the scope chapter's documents.
func (b *book) documents() []Document { return b.documentsFor(b.scope.ChapterID) }

// documentsFor lists work settings plus one chapter's documents, so callers
// walking several chapters of one work load the book once.
func (b *book) documentsFor(chapter string) []Document {
	out := []Document{}
	for _, r := range b.docs {
		if r.chapter == chapter || r.doc.Kind == kindSettings {
			out = append(out, r.doc)
		}
	}
	return out
}

// dependencies is the receipt to store with a document saved now.
func (b *book) dependencies(kind string, pageNo *int64) string {
	return mustJSON(b.receipt(b.scope.ChapterID, kind, pageNo))
}

// guard fingerprints everything a chapter task relies on: the chapter's
// source, the chapter's own document revisions and the inherited state of
// earlier chapters. Metadata-only saves upstream or in later chapters do not
// change it.
func (b *book) guard() string {
	var current any
	if ch, ok := b.chapter(b.scope.ChapterID); ok {
		current = []any{ch.id, ch.sequence, nullable(ch.sourceID)}
	}
	local := [][]any{}
	for _, r := range b.docs {
		if r.chapter == b.scope.ChapterID {
			local = append(local, []any{r.doc.ID, r.doc.Revision})
		}
	}
	upstream := [][]any{}
	for _, r := range b.upstream(b.scope.ChapterID, kindPagePrompt, ptr(int64(math.MaxInt64))) {
		if r.chapter == b.scope.ChapterID {
			continue
		}
		fingerprint := r.doc.ContentHash
		switch r.doc.Kind {
		case kindPagePrompt:
			fingerprint = r.stateHash
		case kindScript:
			fingerprint = r.supplementHash
		}
		upstream = append(upstream, []any{r.doc.ID, fingerprint, r.doc.Stale, r.doc.OutOfPlan, r.doc.Issues})
	}
	return contentHash(mustJSON([]any{current, local, upstream}))
}

// context is the latest upstream material for regenerating a document. It
// refuses while any upstream document is stale or incomplete.
func (b *book) context(kind string, pageNo *int64) (string, error) {
	content := ""
	if ch, ok := b.chapter(b.scope.ChapterID); ok {
		content = ch.content
	}
	var out strings.Builder
	out.WriteString("# 最新关联资料\n## 当前章节正文\n" + content)
	for _, r := range b.upstream(b.scope.ChapterID, kind, pageNo) {
		d := r.doc
		if d.Stale || len(d.Issues) > 0 {
			return "", fmt.Errorf("%s尚未更新或缺少必要节点，请先更新上游资料；你的当前文档仍可手工核对保存", documentLabel(d.Kind, d.PageNo))
		}
		var title, md string
		switch {
		case d.Kind == kindPagePrompt:
			title = fmt.Sprintf("前页人物状态（第%d章第%d页）", b.chapterNumber(r.chapter), orZero(d.PageNo))
			md = r.stateText
		case r.chapter != b.scope.ChapterID && d.Kind == kindScript:
			title, md = "前章人物状态补充", r.supplement
		default:
			md = d.Markdown
			switch d.Kind {
			case kindSettings:
				title = "最新作品设定"
			case kindScript:
				title = "最新本章剧本"
			default:
				title = "最新分页分镜"
			}
		}
		out.WriteString("\n\n## " + title + "\n" + md)
	}
	return out.String(), nil
}

// SyncTarget is a stale document the sync plan would update.
type SyncTarget struct {
	DocumentID string   `json:"documentId"`
	Revision   int64    `json:"revision"`
	Kind       string   `json:"kind"`
	PageNo     *int64   `json:"pageNo"`
	Reasons    []string `json:"reasons"`
}

// SyncPlan is what a chapter sync would update, fingerprinted so the user
// confirms exactly the plan they saw.
type SyncPlan struct {
	Fingerprint     string       `json:"fingerprint"`
	Targets         []SyncTarget `json:"targets"`
	MissingPageNos  []int64      `json:"missingPageNos"`
	ObsoletePageNos []int64      `json:"obsoletePageNos"`
	BlockedReason   *string      `json:"blockedReason"`
}

// AffectedChapter is another chapter with documents that may need a sync.
type AffectedChapter struct {
	ChapterID     string  `json:"chapterId"`
	ChapterNo     int64   `json:"chapterNo"`
	Title         *string `json:"title"`
	DocumentCount int     `json:"documentCount"`
	Reason        string  `json:"reason"`
}

func (b *book) plan() SyncPlan {
	docs := b.documents()
	targets := []SyncTarget{}
	for _, d := range docs {
		if d.Stale && !d.OutOfPlan {
			targets = append(targets, SyncTarget{DocumentID: d.ID, Revision: d.Revision, Kind: d.Kind, PageNo: d.PageNo, Reasons: d.StaleReasons})
		}
	}
	plan, _ := b.pagePlan(b.scope.ChapterID)
	missing := []int64{}
	for _, p := range plan {
		if !slices.ContainsFunc(docs, func(d Document) bool { return d.Kind == kindPagePrompt && d.PageNo != nil && *d.PageNo == p }) {
			missing = append(missing, p)
		}
	}
	obsolete := []int64{}
	for _, d := range docs {
		if d.OutOfPlan && d.PageNo != nil {
			obsolete = append(obsolete, *d.PageNo)
		}
	}
	var blocked *string
	for _, t := range targets {
		for _, up := range b.upstream(b.scope.ChapterID, t.Kind, t.PageNo) {
			if up.doc.Stale && up.chapter != b.scope.ChapterID && up.doc.Kind != kindSettings {
				blocked = ptr("前章关联资料尚未更新，请先到前章同步或核对，再更新本章")
			} else if len(up.doc.Issues) > 0 && !slices.ContainsFunc(targets, func(t SyncTarget) bool { return t.DocumentID == up.doc.ID }) {
				blocked = ptr(fmt.Sprintf("请先补全%s的必要节点", documentLabel(up.doc.Kind, up.doc.PageNo)))
			}
		}
	}
	return SyncPlan{
		Fingerprint:     contentHash(mustJSON([]any{b.guard(), targets, missing, obsolete})),
		Targets:         targets,
		MissingPageNos:  missing,
		ObsoletePageNos: obsolete,
		BlockedReason:   blocked,
	}
}

func (b *book) affected() []AffectedChapter {
	out := []AffectedChapter{}
	for _, ch := range b.chapters {
		if ch.id == b.scope.ChapterID {
			continue
		}
		count := 0
		for _, r := range b.docs {
			if r.chapter == ch.id && r.doc.Stale && !r.doc.OutOfPlan {
				count++
			}
		}
		if count > 0 {
			out = append(out, AffectedChapter{
				ChapterID: ch.id, ChapterNo: ch.number, Title: ch.title, DocumentCount: count,
				Reason: "关联资料或前页人物状态变化，后续内容可能受影响，请同步或核对",
			})
		}
	}
	return out
}
