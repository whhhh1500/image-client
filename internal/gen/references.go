package gen

import (
	"bytes"
	"cmp"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"golang.org/x/image/draw"

	"imageclient/internal/config"
	"imageclient/internal/media"
)

// What a reference image may be used for.
var referenceRoles = []string{
	"character_identity", "outfit", "style", "pose", "scene", "prop", "previous_panel", "base_image", "mask",
}

type referenceLimits struct {
	count       int
	each, total int64
}

var defaultReferenceLimits = referenceLimits{count: 8, each: 20 << 20, total: 50 << 20}

// Several references are composed into one board of tiles.
const (
	tileEdge   = 512
	tileGutter = 8
)

type reference struct {
	data       []byte
	format     string
	role       string
	weight     float64
	sortOrder  int64
	inputOrder int
}

// loadReferences validates and reads the request's references. They must live
// under the assets or output directory, so a request cannot read arbitrary
// local files; this covers run_node and the REST API alike.
func (s *Service) loadReferences(req ImageRequest, cfg config.Settings, limits referenceLimits) ([]reference, error) {
	inputs := req.References
	// references[] is authoritative; the legacy referencePath only applies
	// when it is empty, so a migrating caller never uploads a file twice.
	if len(inputs) == 0 {
		if path := strings.TrimSpace(req.ReferencePath); path != "" {
			weight, order := 1.0, int64(0)
			inputs = []ImageReference{{Path: path, Role: "base_image", Weight: &weight, SortOrder: &order}}
		}
	}
	return validateReferences(inputs, []string{s.paths.Assets(), s.cfg.OutputPath(cfg)}, limits)
}

func validateReferences(inputs []ImageReference, roots []string, limits referenceLimits) ([]reference, error) {
	if len(inputs) > limits.count {
		return nil, fmt.Errorf("参考图最多支持 %d 张", limits.count)
	}
	var total int64
	seen := map[string]bool{}
	refs := make([]reference, 0, len(inputs))
	for i, input := range inputs {
		role := strings.TrimSpace(input.Role)
		if !slices.Contains(referenceRoles, role) {
			return nil, errors.New("参考图 role 无效")
		}
		weight := 1.0
		if input.Weight != nil {
			weight = *input.Weight
		}
		if math.IsNaN(weight) || math.IsInf(weight, 0) || weight <= 0 || weight > 1 {
			return nil, errors.New("参考图 weight 必须在 0 到 1 之间")
		}
		order := int64(i)
		if input.SortOrder != nil {
			order = *input.SortOrder
		}
		if order < 0 {
			return nil, errors.New("参考图 sortOrder 必须为非负整数")
		}

		path, err := allowedReferencePath(strings.TrimSpace(input.Path), roots)
		if err != nil {
			return nil, err
		}
		if seen[path] {
			return nil, errors.New("references 不允许重复引用同一文件")
		}
		seen[path] = true
		info, err := os.Stat(path)
		if err != nil {
			return nil, errors.New("参考图文件不可访问")
		}
		if !info.Mode().IsRegular() {
			return nil, errors.New("参考图必须是普通文件")
		}
		tooLarge := fmt.Errorf("单张参考图不能超过 %d MiB", limits.each>>20)
		if info.Size() > limits.each {
			return nil, tooLarge
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return nil, errors.New("读取参考图失败")
		}
		if int64(len(data)) > limits.each {
			return nil, tooLarge
		}
		if total += int64(len(data)); total > limits.total {
			return nil, fmt.Errorf("参考图总大小不能超过 %d MiB", limits.total>>20)
		}
		format := media.DetectImageFormat(data)
		if format == "" {
			return nil, errors.New("参考图格式不受支持或文件已损坏")
		}
		refs = append(refs, reference{data: data, format: format, role: role, weight: weight, sortOrder: order, inputOrder: i})
	}
	slices.SortFunc(refs, func(a, b reference) int {
		return cmp.Or(cmp.Compare(a.sortOrder, b.sortOrder), strings.Compare(a.role, b.role), cmp.Compare(a.inputOrder, b.inputOrder))
	})
	return refs, nil
}

// allowedReferencePath resolves path (symlinks included) and requires it to
// lie inside one of roots.
func allowedReferencePath(path string, roots []string) (string, error) {
	if path == "" {
		return "", fmt.Errorf("参考图文件不存在或不可访问：%v", os.ErrNotExist)
	}
	resolved, err := canonicalPath(path)
	if err != nil {
		return "", fmt.Errorf("参考图文件不存在或不可访问：%v", err)
	}
	for _, root := range roots {
		if canonicalRoot, err := canonicalPath(root); err == nil && within(canonicalRoot, resolved) {
			return resolved, nil
		}
	}
	return "", errors.New("参考图必须位于应用资产目录或输出目录")
}

func canonicalPath(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return filepath.EvalSymlinks(abs)
}

// within reports whether path is root or below it.
func within(root, path string) bool {
	rel, err := filepath.Rel(root, path)
	return err == nil && !filepath.IsAbs(rel) && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

// upload is the image part of an edits request.
type upload struct {
	data     []byte
	format   string
	fileName string
}

func (u upload) mime() string {
	if u.format == "jpg" {
		return "image/jpeg"
	}
	return "image/" + u.format
}

// referenceUpload sends a single reference as is and composes several into
// one board, since the edits endpoint takes one image.
func referenceUpload(refs []reference) (upload, error) {
	if len(refs) == 0 {
		return upload{}, errors.New("缺少参考图")
	}
	if len(refs) == 1 {
		return upload{data: refs[0].data, format: refs[0].format, fileName: "ref." + refs[0].format}, nil
	}
	board, err := referenceBoard(refs)
	if err != nil {
		return upload{}, err
	}
	return upload{data: board, format: "png", fileName: "reference-board.png"}, nil
}

// referenceBoard lays the references out in 2 (up to four) or 3 columns of
// 512 px tiles on a light grey PNG.
func referenceBoard(refs []reference) ([]byte, error) {
	columns := 2
	if len(refs) > 4 {
		columns = 3
	}
	rows := (len(refs) + columns - 1) / columns
	width := columns*tileEdge + (columns+1)*tileGutter
	height := rows*tileEdge + (rows+1)*tileGutter
	board := image.NewRGBA(image.Rect(0, 0, width, height))
	draw.Draw(board, board.Bounds(), image.NewUniform(color.RGBA{242, 242, 242, 255}), image.Point{}, draw.Src)
	for i, ref := range refs {
		src, _, err := media.DecodeImage(ref.data)
		if err != nil {
			return nil, errors.New("参考图格式不受支持或文件已损坏")
		}
		placeTile(board, src, i, columns)
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, board); err != nil {
		return nil, errors.New("合成参考图板失败")
	}
	return buf.Bytes(), nil
}

// placeTile scales src to fit a tile, keeping its aspect ratio, and centres it.
func placeTile(board *image.RGBA, src image.Image, index, columns int) {
	bounds := src.Bounds()
	w, h := fitWithin(bounds.Dx(), bounds.Dy(), tileEdge)
	column, row := index%columns, index/columns
	x := tileGutter + column*(tileEdge+tileGutter) + (tileEdge-w)/2
	y := tileGutter + row*(tileEdge+tileGutter) + (tileEdge-h)/2
	draw.CatmullRom.Scale(board, image.Rect(x, y, x+w, y+h), src, bounds, draw.Over, nil)
}

// fitWithin scales w x h to fit an edge x edge square.
func fitWithin(w, h, edge int) (int, int) {
	ratio := min(float64(edge)/float64(w), float64(edge)/float64(h))
	return max(int(math.Round(float64(w)*ratio)), 1), max(int(math.Round(float64(h)*ratio)), 1)
}

// promptWithReferenceRoles tells the model which tile of the board does what.
func promptWithReferenceRoles(prompt string, refs []reference) string {
	if len(refs) <= 1 {
		return prompt
	}
	mapping := make([]string, len(refs))
	for i, ref := range refs {
		mapping[i] = fmt.Sprintf("tile %d = %s (weight %.3f)", i+1, ref.role, ref.weight)
	}
	return prompt + "\n\nReference board responsibility map: " + strings.Join(mapping, "; ") +
		". Use each tile only for its named responsibility; preserve identity and style consistency."
}
