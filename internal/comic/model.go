package comic

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/google/uuid"
	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"
)

// Document kinds, in pipeline order.
const (
	kindSettings   = "settings"
	kindScript     = "script"
	kindStoryboard = "storyboard"
	kindPagePrompt = "page_prompt"
)

// Scope addresses one chapter of one novel work in one project.
type Scope struct {
	ProjectID   string `json:"projectId"`
	NovelWorkID string `json:"novelWorkId"`
	ChapterID   string `json:"chapterId"`
}

// Document is the current revision of one Markdown document with its
// derived lineage state.
type Document struct {
	ID                      string   `json:"id"`
	Kind                    string   `json:"kind"`
	PageNo                  *int64   `json:"pageNo"`
	Markdown                string   `json:"markdown"`
	OptimizationInstruction string   `json:"optimizationInstruction"`
	Revision                int64    `json:"revision"`
	Stale                   bool     `json:"stale"`
	ContentHash             string   `json:"contentHash"`
	StaleReasons            []string `json:"staleReasons"`
	OutOfPlan               bool     `json:"outOfPlan"`
	Issues                  []string `json:"issues"`
	UpdatedAt               int64    `json:"updatedAt"`
}

// Job is a background generation, optimization, sync or render task.
type Job struct {
	ID             string  `json:"id"`
	Kind           string  `json:"kind"`
	Status         string  `json:"status"`
	Message        *string `json:"message"`
	OutputMarkdown *string `json:"outputMarkdown"`
	CompletedPages int64   `json:"completedPages"`
	TotalPages     int64   `json:"totalPages"`
	CreatedAt      int64   `json:"createdAt"`
}

// PageImage is one rendered comic page with the prompt inputs it was made from.
type PageImage struct {
	ID                      string  `json:"id"`
	DocumentID              string  `json:"documentId"`
	DocumentRevision        int64   `json:"documentRevision"`
	PageNo                  int64   `json:"pageNo"`
	Path                    string  `json:"path"`
	Stale                   bool    `json:"stale"`
	CreatedAt               int64   `json:"createdAt"`
	PromptInjection         string  `json:"promptInjection"`
	RerunPromptInjection    string  `json:"rerunPromptInjection"`
	VisualProfileRevision   int64   `json:"visualProfileRevision"`
	VisualReferenceSnapshot string  `json:"visualReferenceSnapshot"`
	ContentHash             string  `json:"contentHash"`
	SourcePrompt            *string `json:"sourcePrompt"`
	FileAvailable           bool    `json:"fileAvailable"`
	EffectivePrompt         *string `json:"effectivePrompt"`
}

// Workspace is everything the chapter editor shows.
type Workspace struct {
	SourceRevisionID  *string           `json:"sourceRevisionId"`
	SourceContent     string            `json:"sourceContent"`
	Documents         []Document        `json:"documents"`
	Jobs              []Job             `json:"jobs"`
	Images            []PageImage       `json:"images"`
	TextReady         bool              `json:"textReady"`
	ImageReady        bool              `json:"imageReady"`
	RenderOptions     RenderOptions     `json:"renderOptions"`
	WorkVisualProfile WorkVisualProfile `json:"workVisualProfile"`
	SyncPlan          SyncPlan          `json:"syncPlan"`
	AffectedChapters  []AffectedChapter `json:"affectedChapters"`
}

// CatalogEntry projects a canonical comic record (document or rendered page)
// into the shared asset catalog. It deliberately carries no asset id: these
// records are owned by the comic_md_* tables, not by the assets table.
type CatalogEntry struct {
	SourceURI              string  `json:"sourceUri"`
	ProjectID              string  `json:"projectId"`
	Kind                   string  `json:"kind"` // "text" | "image"
	NovelWorkID            string  `json:"novelWorkId"`
	NovelChapterID         *string `json:"novelChapterId"`
	ChapterNo              *int64  `json:"chapterNo"`
	ChapterTitle           *string `json:"chapterTitle"`
	DocumentID             *string `json:"documentId"`
	DocumentRevision       *int64  `json:"documentRevision"`
	DocumentKind           *string `json:"documentKind"`
	PageNo                 *int64  `json:"pageNo"`
	Title                  string  `json:"title"`
	Text                   *string `json:"text"`
	SourcePrompt           *string `json:"sourcePrompt"`
	EffectivePrompt        *string `json:"effectivePrompt"`
	PromptSnapshotComplete bool    `json:"promptSnapshotComplete"`
	Path                   *string `json:"path"`
	CreatedAt              int64   `json:"createdAt"`
	Stale                  bool    `json:"stale"`
}

// RenderOptions is the chapter's saved prompt injection for page rendering.
type RenderOptions struct {
	PromptInjection string `json:"promptInjection"`
	Revision        int64  `json:"revision"`
}

// WorkVisualReference is one reference image of the work-level visual profile.
type WorkVisualReference struct {
	AssetID       string  `json:"assetId"`
	Path          string  `json:"path"`
	Role          string  `json:"role"`
	Weight        float64 `json:"weight"`
	SortOrder     int64   `json:"sortOrder"`
	Note          string  `json:"note"`
	SHA256        string  `json:"sha256"`
	FileAvailable bool    `json:"fileAvailable"`
}

// WorkVisualProfile is the work-level visual constitution and reference board
// shared by every chapter of a novel work.
type WorkVisualProfile struct {
	ConstitutionMarkdown string                `json:"constitutionMarkdown"`
	Revision             int64                 `json:"revision"`
	References           []WorkVisualReference `json:"references"`
}

// Revision is one saved version of a document.
type Revision struct {
	Revision                int64  `json:"revision"`
	Markdown                string `json:"markdown"`
	OptimizationInstruction string `json:"optimizationInstruction"`
	CreatedAt               int64  `json:"createdAt"`
}

// ExportResult lists the exported Markdown files.
type ExportResult struct {
	Path  string   `json:"path"`
	Files []string `json:"files"`
}

// WorkVisualExtraction is a constitution draft extracted from the references.
type WorkVisualExtraction struct {
	ConstitutionMarkdown string   `json:"constitutionMarkdown"`
	ProfileRevision      int64    `json:"profileRevision"`
	ReferenceAssetIDs    []string `json:"referenceAssetIds"`
}

// Command inputs. Scope fields are flattened into each object.
type (
	CatalogListInput struct {
		ProjectID string `json:"projectId"`
	}
	SaveInput struct {
		Scope
		Kind                    string `json:"kind"`
		PageNo                  *int64 `json:"pageNo"`
		Markdown                string `json:"markdown"`
		OptimizationInstruction string `json:"optimizationInstruction"`
		ExpectedRevision        *int64 `json:"expectedRevision"`
		AcknowledgeUpdates      bool   `json:"acknowledgeUpdates"`
	}
	HistoryInput struct {
		Scope
		DocumentID string `json:"documentId"`
	}
	GenerateInput struct {
		Scope
		Stage                    string `json:"stage"`
		ExpectedSourceRevisionID string `json:"expectedSourceRevisionId"`
	}
	PageInput struct {
		DocumentID string `json:"documentId"`
		Revision   int64  `json:"revision"`
	}
	RenderInput struct {
		Scope
		Pages                         []PageInput `json:"pages"`
		ExpectedRenderOptionsRevision *int64      `json:"expectedRenderOptionsRevision"`
		RerunPromptInjection          string      `json:"rerunPromptInjection"`
	}
	RenderOptionsSaveInput struct {
		Scope
		PromptInjection  string `json:"promptInjection"`
		ExpectedRevision int64  `json:"expectedRevision"`
	}
	OptimizeInput struct {
		Scope
		Targets     []PageInput `json:"targets"`
		Instruction string      `json:"instruction"`
		AllPages    bool        `json:"allPages"`
	}
	SyncInput struct {
		Scope
		ExpectedPlanFingerprint string `json:"expectedPlanFingerprint"`
	}
	ExportInput struct {
		Scope
		// DocumentIDs selects documents; nil exports every in-plan document.
		DocumentIDs *[]string `json:"documentIds"`
	}
	WorkVisualSaveInput struct {
		ProjectID            string                     `json:"projectId"`
		NovelWorkID          string                     `json:"novelWorkId"`
		ConstitutionMarkdown string                     `json:"constitutionMarkdown"`
		References           []WorkVisualReferenceInput `json:"references"`
		ExpectedRevision     int64                      `json:"expectedRevision"`
	}
	WorkVisualReferenceInput struct {
		AssetID   string  `json:"assetId"`
		Role      string  `json:"role"`
		Weight    float64 `json:"weight"`
		SortOrder int64   `json:"sortOrder"`
		Note      string  `json:"note"`
	}
	WorkVisualExtractInput struct {
		ProjectID        string `json:"projectId"`
		NovelWorkID      string `json:"novelWorkId"`
		ExpectedRevision int64  `json:"expectedRevision"`
		Instruction      string `json:"instruction"`
	}
	WorkVisualImportInput struct {
		ProjectID   string   `json:"projectId"`
		NovelWorkID string   `json:"novelWorkId"`
		Paths       []string `json:"paths"`
	}
)

// Size limits, in bytes as the earlier releases measured them.
const (
	maxMarkdownBytes    = 8 * 1024 * 1024
	maxInstructionBytes = 512 * 1024
)

func nowMillis() int64 { return time.Now().UnixMilli() }
func newID() string    { return uuid.NewString() }

// contentHash is the lowercase hex SHA-256 used for content receipts.
func contentHash(text string) string {
	sum := sha256.Sum256([]byte(text))
	return hex.EncodeToString(sum[:])
}

func ptr[T any](v T) *T { return &v }

func optEqual(a, b *int64) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// optCompare orders optional numbers like Rust's Option<i64>: None first.
func optCompare(a, b *int64) int {
	switch {
	case a == nil && b == nil:
		return 0
	case a == nil:
		return -1
	case b == nil:
		return 1
	case *a < *b:
		return -1
	case *a > *b:
		return 1
	}
	return 0
}

func orZero(p *int64) int64 {
	if p == nil {
		return 0
	}
	return *p
}

func strEqual(a, b *string) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

func blank(s string) bool { return strings.TrimSpace(s) == "" }

// --- SQLite helpers ---------------------------------------------------------

// dbError is how storage failures reach the user.
func dbError(err error) error { return fmt.Errorf("漫画资料读写失败：%w", err) }

// execSQL runs a statement; NULL is passed as a nil interface.
func execSQL(conn *sqlite.Conn, query string, args ...any) error {
	if err := sqlitex.Execute(conn, query, &sqlitex.ExecOptions{Args: args}); err != nil {
		return dbError(err)
	}
	return nil
}

// querySQL calls row for each result row.
func querySQL(conn *sqlite.Conn, query string, row func(*sqlite.Stmt), args ...any) error {
	err := sqlitex.Execute(conn, query, &sqlitex.ExecOptions{
		Args:       args,
		ResultFunc: func(stmt *sqlite.Stmt) error { row(stmt); return nil },
	})
	if err != nil {
		return dbError(err)
	}
	return nil
}

// queryOne reads at most one row and reports whether there was one.
func queryOne(conn *sqlite.Conn, query string, row func(*sqlite.Stmt), args ...any) (bool, error) {
	found := false
	err := querySQL(conn, query, func(stmt *sqlite.Stmt) {
		if !found {
			found = true
			row(stmt)
		}
	}, args...)
	return found, err
}

// immediate runs fn in an IMMEDIATE transaction on a connection that is not
// already in one, rolling back when fn fails.
func immediate(conn *sqlite.Conn, fn func() error) (err error) {
	end, err := sqlitex.ImmediateTransaction(conn)
	if err != nil {
		return dbError(err)
	}
	defer end(&err)
	return fn()
}

func columnOptText(stmt *sqlite.Stmt, col int) *string {
	if stmt.ColumnType(col) == sqlite.TypeNull {
		return nil
	}
	return ptr(stmt.ColumnText(col))
}

func nullable[T any](p *T) any {
	if p == nil {
		return nil
	}
	return *p
}

// --- JSON compatible with the earlier serde_json output ----------------------

// marshalJSON encodes like serde_json for the values stored in the database:
// no HTML escaping and no trailing newline.
func marshalJSON(v any) (string, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return "", err
	}
	return strings.TrimSuffix(buf.String(), "\n"), nil
}

func mustJSON(v any) string {
	s, err := marshalJSON(v)
	if err != nil {
		panic(err) // only plain data is encoded here
	}
	return s
}

// serdeString quotes s exactly as serde_json does.
func serdeString(s string) string {
	const hexDigits = "0123456789abcdef"
	var b strings.Builder
	b.Grow(len(s) + 2)
	b.WriteByte('"')
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch c {
		case '"':
			b.WriteString(`\"`)
		case '\\':
			b.WriteString(`\\`)
		case '\b':
			b.WriteString(`\b`)
		case '\f':
			b.WriteString(`\f`)
		case '\n':
			b.WriteString(`\n`)
		case '\r':
			b.WriteString(`\r`)
		case '\t':
			b.WriteString(`\t`)
		default:
			if c < 0x20 {
				b.WriteString(`\u00`)
				b.WriteByte(hexDigits[c>>4])
				b.WriteByte(hexDigits[c&0xf])
			} else {
				b.WriteByte(c)
			}
		}
	}
	b.WriteByte('"')
	return b.String()
}

// serdeFloat formats f as serde_json does (shortest round trip, "1.0" for
// whole numbers, exponent form outside [1e-5, 1e16)).
func serdeFloat(f float64) string {
	if math.IsNaN(f) || math.IsInf(f, 0) {
		return "null"
	}
	abs := math.Abs(f)
	if abs == 0 || (abs >= 1e-5 && abs < 1e16) {
		s := strconv.FormatFloat(f, 'f', -1, 64)
		if !strings.Contains(s, ".") {
			s += ".0"
		}
		return s
	}
	s := strconv.FormatFloat(f, 'e', -1, 64)
	mantissa, exponent, _ := strings.Cut(s, "e")
	sign := ""
	if strings.HasPrefix(exponent, "-") {
		sign = "-"
	}
	exponent = strings.TrimLeft(exponent, "+-0")
	return mantissa + "e" + sign + exponent
}

// referencesSnapshot serialises the reference board byte-for-byte like the
// earlier release, because rendered pages store it and compare it verbatim
// to detect a changed board.
func referencesSnapshot(references []WorkVisualReference) string {
	var b strings.Builder
	b.WriteByte('[')
	for i, r := range references {
		if i > 0 {
			b.WriteByte(',')
		}
		b.WriteString(`{"assetId":`)
		b.WriteString(serdeString(r.AssetID))
		b.WriteString(`,"path":`)
		b.WriteString(serdeString(r.Path))
		b.WriteString(`,"role":`)
		b.WriteString(serdeString(r.Role))
		b.WriteString(`,"weight":`)
		b.WriteString(serdeFloat(r.Weight))
		b.WriteString(`,"sortOrder":`)
		b.WriteString(strconv.FormatInt(r.SortOrder, 10))
		b.WriteString(`,"note":`)
		b.WriteString(serdeString(r.Note))
		b.WriteString(`,"sha256":`)
		b.WriteString(serdeString(r.SHA256))
		b.WriteString(`,"fileAvailable":`)
		b.WriteString(strconv.FormatBool(r.FileAvailable))
		b.WriteByte('}')
	}
	b.WriteByte(']')
	return b.String()
}

// rawExec runs a statement and returns the driver error unwrapped.
func rawExec(conn *sqlite.Conn, query string, args ...any) error {
	return sqlitex.Execute(conn, query, &sqlitex.ExecOptions{Args: args})
}
