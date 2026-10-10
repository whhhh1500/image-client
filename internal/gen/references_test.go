package gen

import (
	"bytes"
	"image/color"
	"image/png"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"imageclient/internal/config"
)

func referenceRolesOf(refs []reference) []string {
	roles := make([]string, len(refs))
	for i, r := range refs {
		roles[i] = r.role
	}
	return roles
}

func TestRejectsReferencePathsOutsideAllowedRoots(t *testing.T) {
	root := t.TempDir()
	inside := filepath.Join(root, "ref.png")
	os.WriteFile(inside, []byte("png"), 0o644)
	outside := filepath.Join(t.TempDir(), "ref.png")
	os.WriteFile(outside, []byte("png"), 0o644)
	if _, err := allowedReferencePath(inside, []string{root}); err != nil {
		t.Fatal(err)
	}
	if _, err := allowedReferencePath(outside, []string{root}); err == nil || err.Error() != "参考图必须位于应用资产目录或输出目录" {
		t.Fatalf("err = %v", err)
	}
	if _, err := allowedReferencePath(filepath.Join(root, "missing.png"), []string{root}); err == nil || !strings.HasPrefix(err.Error(), "参考图文件不存在或不可访问：") {
		t.Fatalf("err = %v", err)
	}
	if _, err := allowedReferencePath("", []string{root}); err == nil || !strings.HasPrefix(err.Error(), "参考图文件不存在或不可访问：") {
		t.Fatalf("err = %v", err)
	}
	// A sibling directory sharing the root's name prefix is outside.
	sibling := root + "-other"
	os.MkdirAll(sibling, 0o755)
	defer os.RemoveAll(sibling)
	os.WriteFile(filepath.Join(sibling, "x.png"), []byte("png"), 0o644)
	if _, err := allowedReferencePath(filepath.Join(sibling, "x.png"), []string{root}); err == nil {
		t.Fatal("a name-prefix sibling must not count as inside")
	}
}

func TestReferencesSortStablyBySortOrderThenRole(t *testing.T) {
	env := newTestService(t, nil)
	first, second, third := filepath.Join(env.output, "first.png"), filepath.Join(env.output, "second.png"), filepath.Join(env.output, "third.png")
	writePNG(t, first, color.RGBA{255, 0, 0, 255})
	writePNG(t, second, color.RGBA{0, 255, 0, 255})
	writePNG(t, third, color.RGBA{0, 0, 255, 255})
	w6, w10, w8, two, one := 0.6, 1.0, 0.8, int64(2), int64(1)
	req := ImageRequest{References: []ImageReference{
		{Path: first, Role: "style", Weight: &w6, SortOrder: &two},
		{Path: second, Role: "scene", Weight: &w10, SortOrder: &one},
		{Path: third, Role: "character_identity", Weight: &w8, SortOrder: &two},
	}}
	refs, err := env.s.loadReferences(req, env.s.cfg.Get(), defaultReferenceLimits)
	if err != nil {
		t.Fatal(err)
	}
	if got := referenceRolesOf(refs); !reflect.DeepEqual(got, []string{"scene", "character_identity", "style"}) {
		t.Fatalf("order = %v", got)
	}
}

func TestReferencesRejectInvalidRoleDuplicatePathAndLimits(t *testing.T) {
	env := newTestService(t, nil)
	cfg := env.s.cfg.Get()
	image := filepath.Join(env.output, "ref.png")
	writePNG(t, image, color.RGBA{1, 2, 3, 255})
	load := func(refs ...ImageReference) error {
		_, err := env.s.loadReferences(ImageRequest{References: refs}, cfg, defaultReferenceLimits)
		return err
	}
	zero, tooHeavy, negative := 0.0, 1.5, int64(-1)
	for want, refs := range map[string][]ImageReference{
		"参考图 role 无效":              {{Path: image, Role: "anything"}},
		"references 不允许重复引用同一文件":   {{Path: image, Role: "style"}, {Path: image, Role: "scene"}},
		"参考图 weight 必须在 0 到 1 之间":  {{Path: image, Role: "style", Weight: &zero}},
		"参考图 weight 必须在 0 到 1 之间 ": {{Path: image, Role: "style", Weight: &tooHeavy}},
		"参考图 sortOrder 必须为非负整数":    {{Path: image, Role: "style", SortOrder: &negative}},
		"参考图格式不受支持或文件已损坏":          {{Path: writeFile(t, env.output, "notes.png", "plain text"), Role: "style"}},
		"参考图必须是普通文件":               {{Path: env.output, Role: "style"}},
	} {
		if err := load(refs...); err == nil || err.Error() != strings.TrimSpace(want) {
			t.Errorf("%s: err = %v", want, err)
		}
	}
	two := []ImageReference{{Path: image, Role: "style"}, {Path: filepath.Join(env.output, "other.png"), Role: "scene"}}
	roots := []string{env.output}
	if _, err := validateReferences(two, roots, referenceLimits{count: 1, each: 1024, total: 1024}); err == nil || err.Error() != "参考图最多支持 1 张" {
		t.Fatalf("err = %v", err)
	}
	one := two[:1]
	if _, err := validateReferences(one, roots, referenceLimits{count: 1, each: 1 << 20, total: 1}); err == nil || err.Error() != "参考图总大小不能超过 0 MiB" {
		t.Fatalf("err = %v", err)
	}
	if _, err := validateReferences(one, roots, referenceLimits{count: 1, each: 1, total: 1024}); err == nil || err.Error() != "单张参考图不能超过 0 MiB" {
		t.Fatalf("err = %v", err)
	}
	if _, err := validateReferences(one, roots, defaultReferenceLimits); err != nil {
		t.Fatal(err)
	}
}

func writeFile(t *testing.T, dir, name, content string) string {
	t.Helper()
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestLegacyReferencePathBecomesASingleBaseImageReference(t *testing.T) {
	env := newTestService(t, nil)
	image := filepath.Join(env.output, "legacy.png")
	writePNG(t, image, color.RGBA{1, 2, 3, 255})
	refs, err := env.s.loadReferences(ImageRequest{ReferencePath: " " + image + " "}, env.s.cfg.Get(), defaultReferenceLimits)
	if err != nil {
		t.Fatal(err)
	}
	if len(refs) != 1 || refs[0].role != "base_image" || refs[0].sortOrder != 0 || refs[0].weight != 1 {
		t.Fatalf("refs = %+v", refs)
	}
	// references[] wins over the legacy field.
	other := filepath.Join(env.output, "other.png")
	writePNG(t, other, color.RGBA{4, 5, 6, 255})
	refs, err = env.s.loadReferences(ImageRequest{ReferencePath: image, References: []ImageReference{{Path: other, Role: "style"}}},
		env.s.cfg.Get(), defaultReferenceLimits)
	if err != nil || len(refs) != 1 || refs[0].role != "style" {
		t.Fatalf("refs = %+v, err = %v", refs, err)
	}
}

func TestReferencesMayLiveInTheAssetsDirectory(t *testing.T) {
	env := newTestService(t, func(s *config.Settings) { s.OutputDir = "" })
	image := filepath.Join(env.s.paths.Assets(), "a.png")
	writePNG(t, image, color.RGBA{1, 2, 3, 255})
	if _, err := env.s.loadReferences(ImageRequest{ReferencePath: image}, env.s.cfg.Get(), defaultReferenceLimits); err != nil {
		t.Fatal(err)
	}
}

func TestSeveralReferencesAreComposedIntoOneBoard(t *testing.T) {
	env := newTestService(t, nil)
	first, second := filepath.Join(env.output, "first.png"), filepath.Join(env.output, "second.png")
	writePNG(t, first, color.RGBA{255, 0, 0, 255})
	writePNG(t, second, color.RGBA{0, 255, 0, 255})
	one, zero := int64(1), int64(0)
	refs, err := env.s.loadReferences(ImageRequest{References: []ImageReference{
		{Path: first, Role: "style", SortOrder: &one},
		{Path: second, Role: "scene", SortOrder: &zero},
	}}, env.s.cfg.Get(), defaultReferenceLimits)
	if err != nil {
		t.Fatal(err)
	}
	upload, err := referenceUpload(refs)
	if err != nil {
		t.Fatal(err)
	}
	if upload.fileName != "reference-board.png" || upload.mime() != "image/png" {
		t.Fatalf("upload = %s %s", upload.fileName, upload.mime())
	}
	board, err := png.Decode(bytes.NewReader(upload.data))
	if err != nil {
		t.Fatal(err)
	}
	// Two columns, one row of 512 px tiles with 8 px gutters.
	if b := board.Bounds(); b.Dx() != 2*512+3*8 || b.Dy() != 512+2*8 {
		t.Fatalf("board = %v", b)
	}
	// Tile 1 (scene, green) is centred in the first cell; the gutter stays grey.
	if r, g, b, _ := board.At(8+256, 8+256).RGBA(); r>>8 != 0 || g>>8 != 255 || b>>8 != 0 {
		t.Fatalf("tile 1 centre = %v %v %v", r>>8, g>>8, b>>8)
	}
	if r, _, _, _ := board.At(2, 2).RGBA(); r>>8 != 242 {
		t.Fatalf("background = %v", r>>8)
	}
	prompt := promptWithReferenceRoles("draw a scene", refs)
	want := "draw a scene\n\nReference board responsibility map: tile 1 = scene (weight 1.000); tile 2 = style (weight 1.000). " +
		"Use each tile only for its named responsibility; preserve identity and style consistency."
	if prompt != want {
		t.Fatalf("prompt = %q", prompt)
	}
	if strings.Contains(prompt, env.root) {
		t.Fatal("paths must not leak into the prompt")
	}
	if promptWithReferenceRoles("p", refs[:1]) != "p" {
		t.Fatal("a single reference keeps the prompt")
	}
	single, _ := referenceUpload(refs[:1])
	if single.fileName != "ref.png" {
		t.Fatalf("single = %s", single.fileName)
	}
	if columns, _ := fitWithin(1024, 512, 512); columns != 512 {
		t.Fatal(columns)
	}
	if w, h := fitWithin(24, 16, 512); w != 512 || h != 341 {
		t.Fatal(w, h)
	}
}

func TestBoardsOfFiveOrMoreUseThreeColumns(t *testing.T) {
	data := pngBytes(t, 10, 10, color.RGBA{9, 9, 9, 255})
	refs := make([]reference, 5)
	for i := range refs {
		refs[i] = reference{data: data, format: "png", role: "style", weight: 1}
	}
	board, err := referenceBoard(refs)
	if err != nil {
		t.Fatal(err)
	}
	img, _ := png.Decode(bytes.NewReader(board))
	if b := img.Bounds(); b.Dx() != 3*512+4*8 || b.Dy() != 2*512+3*8 {
		t.Fatalf("board = %v", b)
	}
}
