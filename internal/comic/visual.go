package comic

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"zombiezen.com/go/sqlite"

	"imageclient/internal/media"
	"imageclient/internal/store"
)

// The work visual profile is a work-level constitution plus up to eight
// reference images, shared by every chapter and frozen into each render.

const (
	maxReferenceBytes           = 20 * 1024 * 1024
	maxReferences               = 8
	maxExtractionImageBytes     = 6 * 1024 * 1024
	maxExtractionTotalBytes     = 16 * 1024 * 1024
	styleReferenceDir           = "漫画画风参考"
	renderSnapshotDir           = ".漫画画风任务快照"
	defaultStyleReferenceSource = "画风参考"
)

var referenceRoles = []string{"character_identity", "outfit", "style", "pose", "scene", "prop", "previous_panel", "base_image", "mask"}

func workScope(conn *sqlite.Conn, projectID, workID string) error {
	exists := false
	if _, err := queryOne(conn, `SELECT EXISTS(SELECT 1 FROM novel_works WHERE id=? AND project_id=? AND status='active')`,
		func(stmt *sqlite.Stmt) { exists = stmt.ColumnBool(0) }, workID, projectID); err != nil {
		return err
	}
	if !exists {
		return errors.New("未找到当前小说作品，请重新选择作品")
	}
	return nil
}

func referenceDigest(data []byte) string {
	sum := sha256.Sum256(data)
	return "sha256:" + hex.EncodeToString(sum[:])
}

type digestEntry struct {
	modTime time.Time
	size    int64
	digest  string
}

var (
	digestMu    sync.Mutex
	digestCache = map[string]digestEntry{}
)

// cachedReferenceDigest digests a reference image, reusing the result while
// size and modification time are unchanged: hashing up to 8 x 20 MiB on every
// workspace read was the largest cost of opening a chapter.
func cachedReferenceDigest(path string) (string, bool) {
	info, err := os.Stat(path)
	if err != nil || !info.Mode().IsRegular() || info.Size() > maxReferenceBytes {
		return "", false
	}
	digestMu.Lock()
	cached, ok := digestCache[path]
	digestMu.Unlock()
	if ok && cached.size == info.Size() && cached.modTime.Equal(info.ModTime()) {
		return cached.digest, true
	}
	data, err := os.ReadFile(path)
	if err != nil || len(data) > maxReferenceBytes || media.DetectImageFormat(data) == "" {
		return "", false
	}
	digest := referenceDigest(data)
	digestMu.Lock()
	digestCache[path] = digestEntry{info.ModTime(), info.Size(), digest}
	digestMu.Unlock()
	return digest, true
}

func workVisualProfile(conn *sqlite.Conn, projectID, workID string) (WorkVisualProfile, error) {
	if err := workScope(conn, projectID, workID); err != nil {
		return WorkVisualProfile{}, err
	}
	profile := WorkVisualProfile{References: []WorkVisualReference{}}
	if _, err := queryOne(conn, `SELECT constitution_markdown,revision FROM comic_md_work_visual_profiles WHERE novel_work_id=? AND project_id=?`,
		func(stmt *sqlite.Stmt) {
			profile.ConstitutionMarkdown, profile.Revision = stmt.ColumnText(0), stmt.ColumnInt64(1)
		},
		workID, projectID); err != nil {
		return WorkVisualProfile{}, err
	}
	err := querySQL(conn, `SELECT reference.asset_id,reference.role,reference.weight,reference.sort_order,reference.note,reference.sha256,asset.path FROM comic_md_work_visual_references reference JOIN assets asset ON asset.id=reference.asset_id WHERE reference.novel_work_id=? ORDER BY reference.sort_order,reference.id`,
		func(stmt *sqlite.Stmt) {
			profile.References = append(profile.References, WorkVisualReference{
				AssetID: stmt.ColumnText(0), Role: stmt.ColumnText(1), Weight: stmt.ColumnFloat(2),
				SortOrder: stmt.ColumnInt64(3), Note: stmt.ColumnText(4), SHA256: stmt.ColumnText(5), Path: stmt.ColumnText(6),
			})
		}, workID)
	if err != nil {
		return WorkVisualProfile{}, err
	}
	for i := range profile.References {
		r := &profile.References[i]
		digest, ok := cachedReferenceDigest(r.Path)
		r.FileAvailable = ok && digest == r.SHA256
	}
	return profile, nil
}

// validatedReferenceAsset checks that an asset may serve as a reference of
// the work and returns its path and digest.
func validatedReferenceAsset(conn *sqlite.Conn, projectID, workID, assetID string) (path, digest string, err error) {
	var kind string
	var metadata *string
	found, err := queryOne(conn, `SELECT kind,path,metadata FROM assets WHERE id=?`,
		func(stmt *sqlite.Stmt) {
			kind, path, metadata = stmt.ColumnText(0), stmt.ColumnText(1), columnOptText(stmt, 2)
		}, assetID)
	if err != nil {
		return "", "", err
	}
	if !found {
		return "", "", errors.New("作品视觉参考图片不存在")
	}
	if kind != "image" {
		return "", "", errors.New("作品视觉参考必须是图片资产")
	}
	var meta struct {
		ProjectID any `json:"projectId"`
		Params    struct {
			NovelWorkID     any `json:"novelWorkId"`
			ComicGeneration struct {
				NovelWorkID any `json:"novelWorkId"`
			} `json:"comicGeneration"`
		} `json:"params"`
	}
	if metadata != nil {
		_ = json.Unmarshal([]byte(*metadata), &meta)
	}
	if p, ok := meta.ProjectID.(string); !ok || p != projectID {
		return "", "", errors.New("作品视觉参考必须属于当前项目")
	}
	declared, ok := meta.Params.NovelWorkID.(string)
	if !ok {
		declared, ok = meta.Params.ComicGeneration.NovelWorkID.(string)
	}
	if ok && declared != workID {
		return "", "", errors.New("作品视觉参考已明确属于另一部小说")
	}
	// Check the declared size first: reading a multi-GB file only to reject it
	// is an avoidable out-of-memory path.
	info, err := os.Stat(path)
	if err != nil {
		return "", "", errors.New("作品视觉参考图片不存在或不可访问")
	}
	if info.Size() > maxReferenceBytes {
		return "", "", errors.New("单张作品视觉参考图不能超过 20 MiB")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return "", "", errors.New("作品视觉参考图片不存在或不可访问")
	}
	if len(data) > maxReferenceBytes {
		return "", "", errors.New("单张作品视觉参考图不能超过 20 MiB")
	}
	if media.DetectImageFormat(data) == "" {
		return "", "", errors.New("作品视觉参考图格式不受支持或文件已损坏")
	}
	return path, referenceDigest(data), nil
}

type referenceFile struct {
	assetID, role string
	weight        float64
	sortOrder     int64
	note, sha256  string
	path          string
}

// frozenVisual is the visual profile admitted with a job, with every
// reference re-verified against its saved digest.
type frozenVisual struct {
	profile        WorkVisualProfile
	referencesJSON string
	files          []referenceFile
}

func freezeVisual(conn *sqlite.Conn, projectID, workID string) (frozenVisual, error) {
	profile, err := workVisualProfile(conn, projectID, workID)
	if err != nil {
		return frozenVisual{}, err
	}
	files := make([]referenceFile, 0, len(profile.References))
	for _, r := range profile.References {
		path, digest, err := validatedReferenceAsset(conn, projectID, workID, r.AssetID)
		if err != nil {
			return frozenVisual{}, err
		}
		if digest != r.SHA256 {
			return frozenVisual{}, errors.New("作品视觉参考图内容已变化，请移除后重新添加并保存")
		}
		files = append(files, referenceFile{r.AssetID, r.Role, r.Weight, r.SortOrder, r.Note, digest, path})
	}
	return frozenVisual{profile: profile, referencesJSON: referencesSnapshot(profile.References), files: files}, nil
}

// materializeReferences copies the frozen references into a per-job folder,
// so a reference edited after admission can never reach the provider. The
// caller removes root when the job ends.
func materializeReferences(assetsDir string, v frozenVisual, jobID string) (root string, refs []ImageReference, err error) {
	root = filepath.Join(assetsDir, renderSnapshotDir, jobID)
	if err := os.MkdirAll(root, 0o755); err != nil {
		return "", nil, fmt.Errorf("创建画风参考快照失败：%v", err)
	}
	defer func() {
		if err != nil {
			os.RemoveAll(root)
		}
	}()
	refs = make([]ImageReference, 0, len(v.files))
	for i, r := range v.files {
		data, err := os.ReadFile(r.path)
		if err != nil {
			return "", nil, errors.New("读取画风参考快照失败，图片可能已被移动")
		}
		if referenceDigest(data) != r.sha256 {
			return "", nil, errors.New("画风参考图在任务提交前发生变化，本次生成未发送")
		}
		format := media.DetectImageFormat(data)
		if format == "" {
			return "", nil, errors.New("画风参考图格式不受支持或文件已损坏")
		}
		path := filepath.Join(root, fmt.Sprintf("%02d.%s", i, format))
		if err := os.WriteFile(path, data, 0o644); err != nil {
			return "", nil, fmt.Errorf("写入画风参考快照失败：%v", err)
		}
		refs = append(refs, ImageReference{Path: path, Role: r.role, Weight: r.weight, SortOrder: r.sortOrder})
	}
	return root, refs, nil
}

func saveWorkVisualProfile(conn *sqlite.Conn, in WorkVisualSaveInput) (WorkVisualProfile, error) {
	if err := workScope(conn, in.ProjectID, in.NovelWorkID); err != nil {
		return WorkVisualProfile{}, err
	}
	if len(in.ConstitutionMarkdown) > maxInstructionBytes {
		return WorkVisualProfile{}, errors.New("视觉宪法过长，请精简后保存")
	}
	if len(in.References) > maxReferences {
		return WorkVisualProfile{}, errors.New("作品级视觉参考最多支持 8 张")
	}
	var current int64
	if _, err := queryOne(conn, `SELECT revision FROM comic_md_work_visual_profiles WHERE novel_work_id=? AND project_id=?`,
		func(stmt *sqlite.Stmt) { current = stmt.ColumnInt64(0) }, in.NovelWorkID, in.ProjectID); err != nil {
		return WorkVisualProfile{}, err
	}
	if in.ExpectedRevision != current {
		return WorkVisualProfile{}, errors.New("作品视觉宪法已有新版本，请刷新后对照保存")
	}
	assetIDs, orders, digests := map[string]bool{}, map[int64]bool{}, map[string]bool{}
	referenceDigests := make([]string, 0, len(in.References))
	for _, r := range in.References {
		if blank(r.AssetID) || assetIDs[r.AssetID] {
			return WorkVisualProfile{}, errors.New("作品视觉参考不能重复引用同一图片")
		}
		assetIDs[r.AssetID] = true
		if !slices.Contains(referenceRoles, r.Role) {
			return WorkVisualProfile{}, errors.New("作品视觉参考 role 无效")
		}
		if math.IsNaN(r.Weight) || math.IsInf(r.Weight, 0) || r.Weight <= 0 || r.Weight > 1 {
			return WorkVisualProfile{}, errors.New("作品视觉参考 weight 必须在 0 到 1 之间")
		}
		if r.SortOrder < 0 || orders[r.SortOrder] {
			return WorkVisualProfile{}, errors.New("作品视觉参考排序必须是互不重复的非负整数")
		}
		orders[r.SortOrder] = true
		_, digest, err := validatedReferenceAsset(conn, in.ProjectID, in.NovelWorkID, r.AssetID)
		if err != nil {
			return WorkVisualProfile{}, err
		}
		if digests[digest] {
			return WorkVisualProfile{}, errors.New("作品视觉参考不能重复使用内容相同的图片")
		}
		digests[digest] = true
		referenceDigests = append(referenceDigests, digest)
	}
	now := nowMillis()
	if err := execSQL(conn, `INSERT INTO comic_md_work_visual_profiles(novel_work_id,project_id,constitution_markdown,revision,updated_at) VALUES(?,?,?,?,?) ON CONFLICT(novel_work_id) DO UPDATE SET constitution_markdown=excluded.constitution_markdown,revision=excluded.revision,updated_at=excluded.updated_at`,
		in.NovelWorkID, in.ProjectID, in.ConstitutionMarkdown, current+1, now); err != nil {
		return WorkVisualProfile{}, err
	}
	if err := execSQL(conn, `DELETE FROM comic_md_work_visual_references WHERE novel_work_id=?`, in.NovelWorkID); err != nil {
		return WorkVisualProfile{}, err
	}
	for i, r := range in.References {
		if err := execSQL(conn, `INSERT INTO comic_md_work_visual_references(id,novel_work_id,asset_id,role,weight,sort_order,note,sha256,created_at) VALUES(?,?,?,?,?,?,?,?,?)`,
			newID(), in.NovelWorkID, r.AssetID, r.Role, r.Weight, r.SortOrder, strings.TrimSpace(r.Note), referenceDigests[i], now); err != nil {
			return WorkVisualProfile{}, err
		}
	}
	return workVisualProfile(conn, in.ProjectID, in.NovelWorkID)
}

// within reports whether path lies inside root (or is root).
func within(root, path string) bool {
	rel, err := filepath.Rel(root, path)
	return err == nil && !filepath.IsAbs(rel) && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

func canonicalPath(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return filepath.EvalSymlinks(abs)
}

// referenceDataURL inlines a reference image for a multimodal text request.
// Only files under the assets or output directory may be sent.
func referenceDataURL(r referenceFile, allowedRoots []string, total *int) (string, error) {
	canonical, err := canonicalPath(r.path)
	if err != nil {
		return "", fmt.Errorf("作品视觉参考图不可访问：%s", r.assetID)
	}
	allowed := false
	for _, root := range allowedRoots {
		if c, err := canonicalPath(root); err == nil && within(c, canonical) {
			allowed = true
			break
		}
	}
	if !allowed {
		return "", errors.New("作品视觉参考图必须位于应用资产目录或输出目录")
	}
	info, err := os.Stat(canonical)
	if err != nil {
		return "", errors.New("读取作品视觉参考图失败")
	}
	if !info.Mode().IsRegular() {
		return "", errors.New("作品视觉参考必须是普通图片文件")
	}
	if info.Size() > maxExtractionImageBytes {
		return "", errors.New("用于视觉提取的单张参考图不能超过 6 MiB，请先压缩图片")
	}
	*total += int(info.Size())
	if *total > maxExtractionTotalBytes {
		return "", errors.New("用于视觉提取的参考图总大小不能超过 16 MiB，请减少或压缩图片")
	}
	data, err := os.ReadFile(canonical)
	if err != nil {
		return "", errors.New("读取作品视觉参考图失败")
	}
	mime := map[string]string{"jpg": "image/jpeg", "png": "image/png", "webp": "image/webp", "gif": "image/gif", "bmp": "image/bmp"}[media.DetectImageFormat(data)]
	if mime == "" {
		return "", errors.New("作品视觉参考图格式不受支持或文件已损坏")
	}
	return "data:" + mime + ";base64," + base64.StdEncoding.EncodeToString(data), nil
}

func extractionMessages(v frozenVisual, allowedRoots []string, instruction string) ([]ChatMessage, error) {
	if len(v.files) == 0 {
		return nil, errors.New("请先至少保存一张作品级视觉参考图，再提取视觉宪法")
	}
	parts := []ContentPart{{Type: "text", Text: extractionText(v.files, v.profile.ConstitutionMarkdown, instruction)}}
	total := 0
	for _, r := range v.files {
		url, err := referenceDataURL(r, allowedRoots, &total)
		if err != nil {
			return nil, err
		}
		parts = append(parts, ContentPart{Type: "image_url", ImageURL: &ImageURL{URL: url}})
	}
	return []ChatMessage{{Role: "system", Content: extractSystem}, {Role: "user", Content: parts}}, nil
}

// extractConstitution drafts a constitution from the saved references. The
// draft is returned, not saved; it is discarded if the profile changed while
// the model was working.
func (s *Service) extractConstitution(ctx context.Context, in WorkVisualExtractInput) (WorkVisualExtraction, error) {
	if len(in.Instruction) > maxInstructionBytes {
		return WorkVisualExtraction{}, errors.New("视觉宪法提取要求过长，请精简后重试")
	}
	cfg := s.config.Get()
	if !cfg.LLMReady() || s.text == nil {
		return WorkVisualExtraction{}, errors.New("请先在设置中配置支持图片输入的文本模型服务")
	}
	var visual frozenVisual
	err := s.db.With(ctx, func(conn *sqlite.Conn) error {
		v, err := freezeVisual(conn, in.ProjectID, in.NovelWorkID)
		if err != nil {
			return err
		}
		if v.profile.Revision != in.ExpectedRevision {
			return errors.New("作品视觉宪法已有新版本，请刷新后再提取")
		}
		visual = v
		return nil
	})
	if err != nil {
		return WorkVisualExtraction{}, err
	}
	messages, err := extractionMessages(visual, []string{s.paths.Assets(), s.config.OutputPath(cfg)}, in.Instruction)
	if err != nil {
		return WorkVisualExtraction{}, err
	}
	completion, err := s.text.Complete(ctx, TextRequest{
		Endpoint: completionEndpoint(cfg.LLMAPIURL), APIKey: cfg.LLMAPIKey, Model: cfg.LLMModel,
		Operation: "comic_markdown.work_visual_extract", Messages: messages,
	})
	if err != nil {
		return WorkVisualExtraction{}, err
	}
	if err := rejectComplianceBlock(completion.Content); err != nil {
		return WorkVisualExtraction{}, err
	}
	if !completion.Completed {
		return WorkVisualExtraction{}, errors.New("视觉宪法提取文本未正常结束，草稿未保存，请重试")
	}
	constitution := strings.TrimSpace(completion.Content)
	if constitution == "" {
		return WorkVisualExtraction{}, errors.New("视觉宪法提取未返回内容")
	}
	var current int64
	err = s.db.With(ctx, func(conn *sqlite.Conn) error {
		p, err := workVisualProfile(conn, in.ProjectID, in.NovelWorkID)
		current = p.Revision
		return err
	})
	if err != nil {
		return WorkVisualExtraction{}, err
	}
	if current != visual.profile.Revision {
		return WorkVisualExtraction{}, errors.New("视觉宪法提取期间参考图或已有宪法发生变化，本次草稿已过期，请重新提取")
	}
	ids := make([]string, len(visual.files))
	for i, f := range visual.files {
		ids[i] = f.assetID
	}
	return WorkVisualExtraction{ConstitutionMarkdown: constitution, ProfileRevision: visual.profile.Revision, ReferenceAssetIDs: ids}, nil
}

// importReferences copies 1–8 local images into assets/漫画画风参考 and
// records them as uploads of the work. Files are removed again if recording
// fails.
func (s *Service) importReferences(ctx context.Context, in WorkVisualImportInput) ([]store.AssetRef, error) {
	if len(in.Paths) == 0 || len(in.Paths) > maxReferences {
		return nil, errors.New("一次请选择 1 到 8 张画风参考图")
	}
	type source struct {
		label  string
		data   []byte
		format string
	}
	var sources []source
	seen := map[string]bool{}
	for _, raw := range in.Paths {
		canonical, err := canonicalPath(raw)
		if err != nil {
			return nil, fmt.Errorf("读取画风参考图失败：%v", err)
		}
		if seen[canonical] {
			continue
		}
		seen[canonical] = true
		info, err := os.Stat(canonical)
		if err != nil {
			return nil, fmt.Errorf("读取画风参考图失败：%v", err)
		}
		if info.Size() > maxReferenceBytes {
			return nil, errors.New("单张画风参考图不能超过 20 MiB")
		}
		data, err := os.ReadFile(canonical)
		if err != nil {
			return nil, fmt.Errorf("读取画风参考图失败：%v", err)
		}
		if len(data) > maxReferenceBytes {
			return nil, errors.New("单张画风参考图不能超过 20 MiB")
		}
		format := media.DetectImageFormat(data)
		if format == "" {
			return nil, errors.New("画风参考图格式不受支持或文件已损坏")
		}
		label := filepath.Base(canonical)
		if label == "" || label == "." || label == string(filepath.Separator) {
			label = defaultStyleReferenceSource
		}
		sources = append(sources, source{label, data, format})
	}
	if len(sources) == 0 {
		return nil, errors.New("所选画风参考图均为重复文件")
	}
	if err := s.db.With(ctx, func(conn *sqlite.Conn) error { return workScope(conn, in.ProjectID, in.NovelWorkID) }); err != nil {
		return nil, err
	}
	imported := make([]store.AssetRef, 0, len(sources))
	removeAll := func() {
		for _, a := range imported {
			os.Remove(a.Path)
		}
	}
	for _, src := range sources {
		asset, err := media.SaveBytes(filepath.Join(s.paths.Assets(), styleReferenceDir), "image", src.data, src.format)
		if err != nil {
			removeAll()
			return nil, err
		}
		imported = append(imported, asset)
	}
	err := s.db.With(ctx, func(conn *sqlite.Conn) error {
		if err := workScope(conn, in.ProjectID, in.NovelWorkID); err != nil {
			return err
		}
		return immediate(conn, func() error {
			for i, asset := range imported {
				metadata := mustJSON(map[string]any{
					"source":    sources[i].label,
					"projectId": in.ProjectID,
					"params": map[string]any{
						"novelWorkId":         in.NovelWorkID,
						"comicStyleReference": true,
						"catalog": map[string]any{
							"version":  1,
							"category": "upload",
							"origin":   "local_upload",
							"group":    map[string]any{"type": "comic_work", "id": in.NovelWorkID},
						},
					},
				})
				if err := execSQL(conn, `INSERT INTO assets(id,kind,path,width,height,duration_s,format,created_at,metadata) VALUES(?,?,?,?,?,?,?,?,?)`,
					asset.ID, asset.Kind, asset.Path, nullable(asset.Width), nullable(asset.Height), nullable(asset.DurationS), asset.Format, nowMillis(), metadata); err != nil {
					return err
				}
			}
			return nil
		})
	})
	if err != nil {
		removeAll()
		return nil, err
	}
	return imported, nil
}
