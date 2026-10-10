package novel

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"
)

// ChapterRevision is one saved text of a chapter.
type ChapterRevision struct {
	ID                               string  `json:"id"`
	NovelWorkID                      string  `json:"novelWorkId"`
	ChapterID                        string  `json:"chapterId"`
	RevisionNo                       int64   `json:"revisionNo"`
	Content                          string  `json:"content"`
	ContentHash                      string  `json:"contentHash"`
	AssetID                          *string `json:"assetId"`
	RequestedParentContextRevisionID *string `json:"requestedParentContextRevisionId"`
	SourceKind                       string  `json:"sourceKind"`
	AnalysisStatus                   *string `json:"analysisStatus"`
	CreatedAt                        int64   `json:"createdAt"`
}

// ChapterRevisionInput is the novel_chapter_revision_create argument. Either
// ChapterID names an existing chapter, or ChapterNo creates one.
type ChapterRevisionInput struct {
	ProjectID               string  `json:"projectId"`
	NovelWorkID             string  `json:"novelWorkId"`
	ChapterID               *string `json:"chapterId"`
	VolumeID                *string `json:"volumeId"`
	SequenceNo              *int64  `json:"sequenceNo"`
	ChapterNo               *int64  `json:"chapterNo"`
	Title                   *string `json:"title"`
	Content                 string  `json:"content"`
	ParentContextRevisionID *string `json:"parentContextRevisionId"`
	AssetID                 *string `json:"assetId"`
	SourceKind              *string `json:"sourceKind"`
	IdempotencyKey          string  `json:"idempotencyKey"`
}

// createChapterRevision validates the request and saves it as the chapter's
// next revision. The whole call is one transaction, so validation reads and
// the write see the same state.
func (s *Service) createChapterRevision(ctx context.Context, in ChapterRevisionInput) (ChapterRevision, error) {
	var revision ChapterRevision
	err := s.db.Tx(ctx, func(conn *sqlite.Conn) error {
		work, err := getWork(conn, in.ProjectID, in.NovelWorkID)
		if err != nil {
			return err
		}
		if work.Status != "active" {
			return errArchived
		}
		content, err := normalizeContent(in.Content)
		if err != nil {
			return err
		}
		sourceKind := "paste"
		if in.SourceKind != nil {
			sourceKind = *in.SourceKind
		}
		switch sourceKind {
		case "paste", "asset", "legacy_import":
		default:
			return errors.New("sourceKind 无效")
		}
		if sourceKind == "asset" && in.AssetID == nil {
			return errors.New("asset sourceKind 必须提供 assetId")
		}
		if in.AssetID != nil {
			if err := ensureAssetInProject(conn, *in.AssetID, work.ProjectID); err != nil {
				return err
			}
		}
		if in.ParentContextRevisionID != nil {
			if err := ensureParentContext(conn, *in.ParentContextRevisionID, work.ID); err != nil {
				return err
			}
		}

		revision, err = withReceipt(conn, "novel_chapter_revision_create", in.IdempotencyKey, revisionRequest(in), func() (ChapterRevision, error) {
			return insertChapterRevision(conn, work, in, content, sourceKind)
		})
		return err
	})
	return revision, err
}

// revisionRequest is the payload hashed for idempotency. It holds the input as
// received (the raw content, before normalization), with absent fields as null.
func revisionRequest(in ChapterRevisionInput) map[string]any {
	return map[string]any{
		"projectId":               in.ProjectID,
		"novelWorkId":             in.NovelWorkID,
		"chapterId":               nullable(in.ChapterID),
		"volumeId":                nullable(in.VolumeID),
		"sequenceNo":              nullable(in.SequenceNo),
		"chapterNo":               nullable(in.ChapterNo),
		"title":                   nullable(in.Title),
		"content":                 in.Content,
		"parentContextRevisionId": nullable(in.ParentContextRevisionID),
		"assetId":                 nullable(in.AssetID),
		"sourceKind":              nullable(in.SourceKind),
		"idempotencyKey":          in.IdempotencyKey,
	}
}

func insertChapterRevision(conn *sqlite.Conn, work Work, in ChapterRevisionInput, content, sourceKind string) (ChapterRevision, error) {
	ts := nowMillis()
	chapter, err := resolveChapter(conn, work, in, ts)
	if err != nil {
		return ChapterRevision{}, err
	}

	var revisionNo int64
	revisionNo, err = scalarInt(conn,
		"SELECT COALESCE(MAX(version), 0) + 1 FROM novel_chapter_revisions WHERE novel_chapter_id = ?", chapter.ID)
	if err != nil {
		return ChapterRevision{}, fmt.Errorf("读取正文版本号失败: %w", err)
	}

	sum := sha256.Sum256([]byte(content))
	revision := ChapterRevision{
		ID:                               newID("nrevision"),
		NovelWorkID:                      work.ID,
		ChapterID:                        chapter.ID,
		RevisionNo:                       revisionNo,
		Content:                          content,
		ContentHash:                      "sha256:" + hex.EncodeToString(sum[:]),
		AssetID:                          in.AssetID,
		RequestedParentContextRevisionID: in.ParentContextRevisionID,
		SourceKind:                       sourceKind,
		AnalysisStatus:                   ptr("saved"),
		CreatedAt:                        ts,
	}
	err = sqlitex.Execute(conn, `INSERT INTO novel_chapter_revisions (id, novel_chapter_id, version, content, content_hash,
		asset_id, requested_parent_context_revision_id, source_kind, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`, &sqlitex.ExecOptions{Args: []any{
		revision.ID, revision.ChapterID, revision.RevisionNo, revision.Content, revision.ContentHash,
		nullable(revision.AssetID), nullable(revision.RequestedParentContextRevisionID), revision.SourceKind, revision.CreatedAt,
	}})
	if err != nil {
		return ChapterRevision{}, fmt.Errorf("创建正文版本失败: %w", err)
	}

	err = sqlitex.Execute(conn, `UPDATE novel_chapters SET current_revision_id = ?, title = ?, updated_at = ?
		WHERE id = ? AND novel_work_id = ?`, &sqlitex.ExecOptions{Args: []any{
		revision.ID, nullable(chapter.Title), ts, chapter.ID, work.ID,
	}})
	if err != nil {
		return ChapterRevision{}, fmt.Errorf("更新章节当前版本失败: %w", err)
	}
	if conn.Changes() != 1 {
		return ChapterRevision{}, errors.New("创建正文版本时章节归属发生变化")
	}
	return revision, nil
}

// resolveChapter returns the chapter the revision belongs to, creating it when
// the request names none. A title in the request replaces the chapter's title;
// without one the existing title is kept.
func resolveChapter(conn *sqlite.Conn, work Work, in ChapterRevisionInput, ts int64) (Chapter, error) {
	if in.ChapterID != nil {
		chapter, err := chapterInWork(conn, *in.ChapterID, work.ID)
		if err != nil {
			return Chapter{}, err
		}
		if in.Title != nil {
			title, err := titleText(*in.Title, "title")
			if err != nil {
				return Chapter{}, err
			}
			chapter.Title = &title
		}
		return chapter, nil
	}

	if in.ChapterNo == nil {
		return Chapter{}, errors.New("新章节必须提供 chapterNo")
	}
	chapterNo := *in.ChapterNo
	sequenceNo := chapterNo
	if in.SequenceNo != nil {
		sequenceNo = *in.SequenceNo
	}
	if sequenceNo <= 0 || chapterNo <= 0 {
		return Chapter{}, errors.New("sequenceNo 和 chapterNo 必须是正整数")
	}
	if in.VolumeID != nil {
		ok, err := scalarBool(conn,
			"SELECT EXISTS(SELECT 1 FROM novel_volumes WHERE id = ? AND novel_work_id = ?)", *in.VolumeID, work.ID)
		if err != nil {
			return Chapter{}, fmt.Errorf("验证卷归属失败: %w", err)
		}
		if !ok {
			return Chapter{}, errors.New("卷不存在或不属于当前小说")
		}
	}

	chapter := Chapter{
		ID:          newID("nchapter"),
		NovelWorkID: work.ID,
		VolumeID:    in.VolumeID,
		SequenceNo:  sequenceNo,
		ChapterNo:   chapterNo,
		CreatedAt:   ts,
		UpdatedAt:   ts,
	}
	if in.Title != nil {
		title, err := titleText(*in.Title, "title")
		if err != nil {
			return Chapter{}, err
		}
		chapter.Title = &title
	}
	err := sqlitex.Execute(conn, `INSERT INTO novel_chapters (id, novel_work_id, volume_id, sequence_no, chapter_no,
		title, current_revision_id, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)`, &sqlitex.ExecOptions{Args: []any{
		chapter.ID, chapter.NovelWorkID, nullable(chapter.VolumeID), chapter.SequenceNo, chapter.ChapterNo,
		nullable(chapter.Title), ts, ts,
	}})
	if err != nil {
		return Chapter{}, fmt.Errorf("创建章节失败: %w", err)
	}
	return chapter, nil
}

func chapterInWork(conn *sqlite.Conn, chapterID, workID string) (Chapter, error) {
	var chapter Chapter
	found := false
	err := query(conn, `SELECT id, novel_work_id, volume_id, sequence_no, chapter_no, title,
		current_revision_id, created_at, updated_at
		FROM novel_chapters WHERE id = ? AND novel_work_id = ?`, []any{chapterID, workID}, func(stmt *sqlite.Stmt) {
		found = true
		chapter = scanChapter(stmt)
	})
	if err != nil {
		return Chapter{}, fmt.Errorf("读取章节失败: %w", err)
	}
	if !found {
		return Chapter{}, errors.New("章节不存在或不属于当前小说")
	}
	return chapter, nil
}

// ensureAssetInProject accepts an asset only when its metadata names this
// project as its owner.
func ensureAssetInProject(conn *sqlite.Conn, assetID, projectID string) error {
	var metadata *string
	err := query(conn, "SELECT metadata FROM assets WHERE id = ?", []any{assetID}, func(stmt *sqlite.Stmt) {
		metadata = optionalText(stmt, 0)
	})
	if err != nil {
		return fmt.Errorf("读取正文资产失败: %w", err)
	}
	if metadata == nil {
		return errors.New("正文资产不存在")
	}
	var meta struct {
		ProjectID *string `json:"projectId"`
	}
	if json.Unmarshal([]byte(*metadata), &meta) != nil || meta.ProjectID == nil || *meta.ProjectID != projectID {
		return errors.New("正文资产不属于当前项目")
	}
	return nil
}

// ensureParentContext checks that a frozen parent context belongs to this work.
func ensureParentContext(conn *sqlite.Conn, contextID, workID string) error {
	ok, err := scalarBool(conn, `SELECT EXISTS(
		SELECT 1 FROM novel_chapter_context_revisions context
		JOIN novel_analysis_lineages lineage ON lineage.id = context.novel_analysis_lineage_id
		WHERE context.id = ? AND lineage.novel_work_id = ?)`, contextID, workID)
	if err != nil {
		return fmt.Errorf("验证冻结父上下文失败: %w", err)
	}
	if !ok {
		return errors.New("冻结父上下文不存在或不属于当前小说")
	}
	return nil
}

func ptr[T any](v T) *T {
	return &v
}
