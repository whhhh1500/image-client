package novel

import (
	"context"
	"errors"
	"fmt"

	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"
)

// Work is a novel. Field order and names follow the Rust struct, which the
// frontend reads as camelCase JSON.
type Work struct {
	ID                         string `json:"id"`
	ProjectID                  string `json:"projectId"`
	Title                      string `json:"title"`
	Description                string `json:"description"`
	Status                     string `json:"status"`
	PublishedCanonVersionID    string `json:"publishedCanonVersionId"`
	CurrentNovelStateVersionID string `json:"currentNovelStateVersionId"`
	CurrentAnalysisLineageID   string `json:"currentAnalysisLineageId"`
	CreatedAt                  int64  `json:"createdAt"`
	UpdatedAt                  int64  `json:"updatedAt"`
}

// Volume groups chapters.
type Volume struct {
	ID          string  `json:"id"`
	NovelWorkID string  `json:"novelWorkId"`
	VolumeNo    int64   `json:"volumeNo"`
	Title       *string `json:"title"`
	CreatedAt   int64   `json:"createdAt"`
}

// Chapter is a chapter of a work; LatestRevisionID points at its current text.
type Chapter struct {
	ID               string  `json:"id"`
	NovelWorkID      string  `json:"novelWorkId"`
	VolumeID         *string `json:"volumeId"`
	SequenceNo       int64   `json:"sequenceNo"`
	ChapterNo        int64   `json:"chapterNo"`
	Title            *string `json:"title"`
	LatestRevisionID *string `json:"latestRevisionId"`
	CreatedAt        int64   `json:"createdAt"`
	UpdatedAt        int64   `json:"updatedAt"`
}

// Lineage is an analysis line of a work.
type Lineage struct {
	ID                          string  `json:"id"`
	NovelWorkID                 string  `json:"novelWorkId"`
	CurrentContextRevisionID    *string `json:"currentContextRevisionId"`
	ContinuousThroughSequenceNo int64   `json:"continuousThroughSequenceNo"`
	OptimisticVersion           int64   `json:"optimisticVersion"`
	Status                      string  `json:"status"`
}

// Entity is a story entity. The facts and aliases are not stored yet, so they
// are always empty.
type Entity struct {
	ID          string         `json:"id"`
	NovelWorkID string         `json:"novelWorkId"`
	EntityType  string         `json:"entityType"`
	Name        string         `json:"name"`
	Status      string         `json:"status"`
	Facts       map[string]any `json:"facts"`
	Aliases     []string       `json:"aliases"`
	Provisional bool           `json:"provisional"`
}

// Snapshot is everything the workbench shows for one work.
type Snapshot struct {
	Work                               Work              `json:"work"`
	Volumes                            []Volume          `json:"volumes"`
	Chapters                           []Chapter         `json:"chapters"`
	Revisions                          []ChapterRevision `json:"revisions"`
	Entities                           []Entity          `json:"entities"`
	ContextStatus                      string            `json:"contextStatus"`
	CurrentContextRevisionID           *string           `json:"currentContextRevisionId"`
	CurrentNovelStateThroughSequenceNo int64             `json:"currentNovelStateThroughSequenceNo"`
	CanonicalVersionID                 string            `json:"canonicalVersionId"`
	Lineages                           []Lineage         `json:"lineages"`
}

// CreateWorkInput is the novel_work_create argument.
type CreateWorkInput struct {
	ProjectID      string  `json:"projectId"`
	Title          string  `json:"title"`
	Description    *string `json:"description"`
	IdempotencyKey string  `json:"idempotencyKey"`
}

// ListWorksInput is the novel_work_list argument.
type ListWorksInput struct {
	ProjectID       string `json:"projectId"`
	IncludeArchived bool   `json:"includeArchived"`
}

// GetWorkInput is the novel_work_get argument.
type GetWorkInput struct {
	ProjectID   string `json:"projectId"`
	NovelWorkID string `json:"novelWorkId"`
}

const workColumns = `id, project_id, title, description, status, published_canon_version_id,
	current_novel_state_version_id, current_analysis_lineage_id, created_at, updated_at`

func scanWork(stmt *sqlite.Stmt) Work {
	return Work{
		ID:                         stmt.ColumnText(0),
		ProjectID:                  stmt.ColumnText(1),
		Title:                      stmt.ColumnText(2),
		Description:                stmt.ColumnText(3),
		Status:                     stmt.ColumnText(4),
		PublishedCanonVersionID:    stmt.ColumnText(5),
		CurrentNovelStateVersionID: stmt.ColumnText(6),
		CurrentAnalysisLineageID:   stmt.ColumnText(7),
		CreatedAt:                  stmt.ColumnInt64(8),
		UpdatedAt:                  stmt.ColumnInt64(9),
	}
}

func (s *Service) listWorks(ctx context.Context, in ListWorksInput) ([]Work, error) {
	projectID, err := requiredText(in.ProjectID, "projectId")
	if err != nil {
		return nil, err
	}
	sql := "SELECT " + workColumns + " FROM novel_works WHERE project_id = ?"
	if !in.IncludeArchived {
		sql += " AND status = 'active'"
	}
	sql += " ORDER BY created_at, id"

	works := []Work{}
	err = s.db.With(ctx, func(conn *sqlite.Conn) error {
		return query(conn, sql, []any{projectID}, func(stmt *sqlite.Stmt) {
			works = append(works, scanWork(stmt))
		})
	})
	if err != nil {
		return nil, fmt.Errorf("查询小说列表失败：%w", err)
	}
	return works, nil
}

func (s *Service) createWork(ctx context.Context, in CreateWorkInput) (Work, error) {
	projectID, err := requiredText(in.ProjectID, "projectId")
	if err != nil {
		return Work{}, err
	}
	title, err := titleText(in.Title, "title")
	if err != nil {
		return Work{}, err
	}
	request := map[string]any{
		"projectId":      in.ProjectID,
		"title":          in.Title,
		"description":    nullable(in.Description),
		"idempotencyKey": in.IdempotencyKey,
	}
	description := ""
	if in.Description != nil {
		description = *in.Description
	}

	var work Work
	err = s.db.Tx(ctx, func(conn *sqlite.Conn) error {
		w, err := withReceipt(conn, "novel_work_create", in.IdempotencyKey, request, func() (Work, error) {
			return insertWork(conn, projectID, title, description)
		})
		work = w
		return err
	})
	return work, err
}

// insertWork creates a work with its genesis canon, state and main lineage.
func insertWork(conn *sqlite.Conn, projectID, title, description string) (Work, error) {
	ts := nowMillis()
	w := Work{
		ID:                         newID("nwork"),
		ProjectID:                  projectID,
		Title:                      title,
		Description:                description,
		Status:                     "active",
		PublishedCanonVersionID:    newID("ncanon"),
		CurrentNovelStateVersionID: newID("nstate"),
		CurrentAnalysisLineageID:   newID("nlineage"),
		CreatedAt:                  ts,
		UpdatedAt:                  ts,
	}
	// The head pointers start NULL and are set once the versions exist, because
	// the versions and the lineage reference the work.
	steps := []struct {
		sql     string
		args    []any
		failure string
	}{
		{
			`INSERT INTO novel_works (id, project_id, title, description, status, published_canon_version_id,
			 current_novel_state_version_id, current_analysis_lineage_id, created_at, updated_at)
			 VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?)`,
			[]any{w.ID, w.ProjectID, w.Title, w.Description, w.Status, ts, ts},
			"创建小说失败",
		},
		{
			`INSERT INTO novel_canon_versions (id, novel_work_id, version, parent_version_id,
			 body_json, rendered_markdown, status, created_at)
			 VALUES (?, ?, 0, NULL, '{}', '', 'published', ?)`,
			[]any{w.PublishedCanonVersionID, w.ID, ts},
			"创建 Canon C0 失败",
		},
		{
			`INSERT INTO novel_state_versions (id, novel_work_id, version, parent_version_id,
			 through_novel_chapter_revision_id, body_json, created_at)
			 VALUES (?, ?, 0, NULL, NULL, '{}', ?)`,
			[]any{w.CurrentNovelStateVersionID, w.ID, ts},
			"创建 NovelState S0 失败",
		},
		{
			`INSERT INTO novel_analysis_lineages (id, novel_work_id, name,
			 base_published_canon_version_id, base_published_novel_state_version_id,
			 current_context_revision_id, continuous_through_sequence_no, status,
			 optimistic_version, created_at, updated_at)
			 VALUES (?, ?, 'main', ?, ?, NULL, 0, 'active', 0, ?, ?)`,
			[]any{w.CurrentAnalysisLineageID, w.ID, w.PublishedCanonVersionID, w.CurrentNovelStateVersionID, ts, ts},
			"创建主工作线失败",
		},
	}
	for _, step := range steps {
		if err := sqlitex.Execute(conn, step.sql, &sqlitex.ExecOptions{Args: step.args}); err != nil {
			return Work{}, fmt.Errorf("%s：%w", step.failure, err)
		}
	}
	err := sqlitex.Execute(conn,
		`UPDATE novel_works SET published_canon_version_id = ?, current_novel_state_version_id = ?,
		 current_analysis_lineage_id = ? WHERE id = ? AND project_id = ?`,
		&sqlitex.ExecOptions{Args: []any{w.PublishedCanonVersionID, w.CurrentNovelStateVersionID, w.CurrentAnalysisLineageID, w.ID, w.ProjectID}})
	if err != nil {
		return Work{}, fmt.Errorf("设置小说初始 head 失败：%w", err)
	}
	if conn.Changes() != 1 {
		return Work{}, errors.New("创建小说时未能设置初始 head")
	}
	return w, nil
}

// getWork loads a work the project owns.
func getWork(conn *sqlite.Conn, projectID, workID string) (Work, error) {
	projectID, err := requiredText(projectID, "projectId")
	if err != nil {
		return Work{}, err
	}
	workID, err = requiredText(workID, "novelWorkId")
	if err != nil {
		return Work{}, err
	}
	var work Work
	found := false
	err = query(conn, "SELECT "+workColumns+" FROM novel_works WHERE id = ? AND project_id = ?",
		[]any{workID, projectID}, func(stmt *sqlite.Stmt) {
			found = true
			work = scanWork(stmt)
		})
	if err != nil {
		return Work{}, fmt.Errorf("读取小说失败：%w", err)
	}
	if !found {
		return Work{}, errors.New("小说不存在或不属于当前项目")
	}
	return work, nil
}

func (s *Service) getSnapshot(ctx context.Context, in GetWorkInput) (Snapshot, error) {
	var snap Snapshot
	err := s.db.With(ctx, func(conn *sqlite.Conn) (err error) {
		// One read transaction, so the parts of the snapshot agree.
		end := sqlitex.Save(conn)
		defer end(&err)
		work, err := getWork(conn, in.ProjectID, in.NovelWorkID)
		if err != nil {
			return err
		}
		snap, err = snapshotOf(conn, work)
		return err
	})
	return snap, err
}

func snapshotOf(conn *sqlite.Conn, work Work) (Snapshot, error) {
	lineages, err := listLineages(conn, work.ID)
	if err != nil {
		return Snapshot{}, err
	}
	var main *Lineage
	for i := range lineages {
		if lineages[i].ID == work.CurrentAnalysisLineageID {
			main = &lineages[i]
			break
		}
	}
	if main == nil {
		return Snapshot{}, errors.New("小说缺少主工作线")
	}
	stateThrough, err := stateThroughSequence(conn, work.CurrentNovelStateVersionID)
	if err != nil {
		return Snapshot{}, err
	}
	volumes, err := listVolumes(conn, work.ID)
	if err != nil {
		return Snapshot{}, err
	}
	chapters, err := listChapters(conn, work.ID)
	if err != nil {
		return Snapshot{}, err
	}
	revisions, err := listRevisions(conn, work.ID)
	if err != nil {
		return Snapshot{}, err
	}
	entities, err := listEntities(conn, work.ID)
	if err != nil {
		return Snapshot{}, err
	}
	contextStatus := "not_started"
	if main.CurrentContextRevisionID != nil {
		contextStatus = "ready"
	}
	return Snapshot{
		Work:                               work,
		Volumes:                            volumes,
		Chapters:                           chapters,
		Revisions:                          revisions,
		Entities:                           entities,
		ContextStatus:                      contextStatus,
		CurrentContextRevisionID:           main.CurrentContextRevisionID,
		CurrentNovelStateThroughSequenceNo: stateThrough,
		CanonicalVersionID:                 work.PublishedCanonVersionID,
		Lineages:                           lineages,
	}, nil
}

// stateThroughSequence is the sequence number the state version has been
// published through; 0 when it has not advanced past the start.
func stateThroughSequence(conn *sqlite.Conn, stateVersionID string) (int64, error) {
	var seq int64
	err := query(conn, `SELECT chapter.sequence_no FROM novel_state_versions state
		LEFT JOIN novel_chapter_revisions revision ON revision.id = state.through_novel_chapter_revision_id
		LEFT JOIN novel_chapters chapter ON chapter.id = revision.novel_chapter_id
		WHERE state.id = ?`, []any{stateVersionID}, func(stmt *sqlite.Stmt) {
		if stmt.ColumnType(0) != sqlite.TypeNull {
			seq = stmt.ColumnInt64(0)
		}
	})
	if err != nil {
		return 0, fmt.Errorf("读取小说状态推进位置失败：%w", err)
	}
	return seq, nil
}

func listVolumes(conn *sqlite.Conn, workID string) ([]Volume, error) {
	volumes := []Volume{}
	err := query(conn, `SELECT id, novel_work_id, volume_no, title, created_at FROM novel_volumes
		WHERE novel_work_id = ? ORDER BY volume_no, id`, []any{workID}, func(stmt *sqlite.Stmt) {
		volumes = append(volumes, Volume{
			ID:          stmt.ColumnText(0),
			NovelWorkID: stmt.ColumnText(1),
			VolumeNo:    stmt.ColumnInt64(2),
			Title:       optionalText(stmt, 3),
			CreatedAt:   stmt.ColumnInt64(4),
		})
	})
	if err != nil {
		return nil, fmt.Errorf("查询卷列表失败：%w", err)
	}
	return volumes, nil
}

func listChapters(conn *sqlite.Conn, workID string) ([]Chapter, error) {
	chapters := []Chapter{}
	err := query(conn, `SELECT id, novel_work_id, volume_id, sequence_no, chapter_no, title,
		current_revision_id, created_at, updated_at
		FROM novel_chapters WHERE novel_work_id = ? ORDER BY sequence_no, id`, []any{workID}, func(stmt *sqlite.Stmt) {
		chapters = append(chapters, scanChapter(stmt))
	})
	if err != nil {
		return nil, fmt.Errorf("查询章节列表失败：%w", err)
	}
	return chapters, nil
}

// scanChapter reads the columns in chapterColumns order.
func scanChapter(stmt *sqlite.Stmt) Chapter {
	return Chapter{
		ID:               stmt.ColumnText(0),
		NovelWorkID:      stmt.ColumnText(1),
		VolumeID:         optionalText(stmt, 2),
		SequenceNo:       stmt.ColumnInt64(3),
		ChapterNo:        stmt.ColumnInt64(4),
		Title:            optionalText(stmt, 5),
		LatestRevisionID: optionalText(stmt, 6),
		CreatedAt:        stmt.ColumnInt64(7),
		UpdatedAt:        stmt.ColumnInt64(8),
	}
}

func listLineages(conn *sqlite.Conn, workID string) ([]Lineage, error) {
	lineages := []Lineage{}
	err := query(conn, `SELECT id, novel_work_id, current_context_revision_id, continuous_through_sequence_no,
		optimistic_version, status FROM novel_analysis_lineages
		WHERE novel_work_id = ? ORDER BY created_at, id`, []any{workID}, func(stmt *sqlite.Stmt) {
		lineages = append(lineages, Lineage{
			ID:                          stmt.ColumnText(0),
			NovelWorkID:                 stmt.ColumnText(1),
			CurrentContextRevisionID:    optionalText(stmt, 2),
			ContinuousThroughSequenceNo: stmt.ColumnInt64(3),
			OptimisticVersion:           stmt.ColumnInt64(4),
			Status:                      stmt.ColumnText(5),
		})
	})
	if err != nil {
		return nil, fmt.Errorf("查询工作线列表失败：%w", err)
	}
	return lineages, nil
}

func listRevisions(conn *sqlite.Conn, workID string) ([]ChapterRevision, error) {
	revisions := []ChapterRevision{}
	err := query(conn, `SELECT revision.id, chapter.novel_work_id, revision.novel_chapter_id, revision.version,
		revision.content, revision.content_hash, revision.asset_id,
		revision.requested_parent_context_revision_id, revision.source_kind, revision.created_at,
		(SELECT run.status FROM source_analysis_runs run WHERE run.novel_chapter_revision_id = revision.id
		 ORDER BY run.created_at DESC, run.id DESC LIMIT 1)
		FROM novel_chapter_revisions revision
		JOIN novel_chapters chapter ON chapter.id = revision.novel_chapter_id
		WHERE chapter.novel_work_id = ? ORDER BY chapter.sequence_no, revision.version, revision.id`,
		[]any{workID}, func(stmt *sqlite.Stmt) {
			revisions = append(revisions, ChapterRevision{
				ID:                               stmt.ColumnText(0),
				NovelWorkID:                      stmt.ColumnText(1),
				ChapterID:                        stmt.ColumnText(2),
				RevisionNo:                       stmt.ColumnInt64(3),
				Content:                          stmt.ColumnText(4),
				ContentHash:                      stmt.ColumnText(5),
				AssetID:                          optionalText(stmt, 6),
				RequestedParentContextRevisionID: optionalText(stmt, 7),
				SourceKind:                       stmt.ColumnText(8),
				CreatedAt:                        stmt.ColumnInt64(9),
				AnalysisStatus:                   optionalText(stmt, 10),
			})
		})
	if err != nil {
		return nil, fmt.Errorf("查询正文版本列表失败：%w", err)
	}
	return revisions, nil
}

func listEntities(conn *sqlite.Conn, workID string) ([]Entity, error) {
	entities := []Entity{}
	err := query(conn, `SELECT id, novel_work_id, entity_kind, stable_key, lifecycle, candidate_lineage_id
		FROM novel_entities WHERE novel_work_id = ? ORDER BY stable_key, id`, []any{workID}, func(stmt *sqlite.Stmt) {
		lifecycle := stmt.ColumnText(4)
		entities = append(entities, Entity{
			ID:          stmt.ColumnText(0),
			NovelWorkID: stmt.ColumnText(1),
			EntityType:  stmt.ColumnText(2),
			Name:        stmt.ColumnText(3),
			Status:      lifecycle,
			Facts:       map[string]any{},
			Aliases:     []string{},
			Provisional: lifecycle == "candidate",
		})
	})
	if err != nil {
		return nil, fmt.Errorf("查询小说实体列表失败：%w", err)
	}
	return entities, nil
}
