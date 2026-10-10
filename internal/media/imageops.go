package media

import (
	"bytes"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/color/palette"
	"image/draw"
	"image/gif"
	"image/jpeg"
	"image/png"
	"log/slog"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/google/uuid"
	"golang.org/x/image/bmp"

	"imageclient/internal/store"
)

// Compression and conversion write variants next to the original, named
// <original stem>-<grade>-<yyyymmddhhmmssSSS>.<ext>, so every variant of an
// image can be found from any one of them.

type imageFileInfo struct {
	Path        string             `json:"path"`
	Directory   string             `json:"directory"`
	FileName    string             `json:"fileName"`
	Bytes       int64              `json:"bytes"`
	KB          float64            `json:"kb"`
	Width       *int               `json:"width"`
	Height      *int               `json:"height"`
	Format      *string            `json:"format"`
	DisplayPath string             `json:"displayPath"`
	Variants    []imageVariantInfo `json:"variants"`
}

type imageVariantInfo struct {
	Path       string  `json:"path"`
	FileName   string  `json:"fileName"`
	Level      string  `json:"level"`
	Bytes      int64   `json:"bytes"`
	KB         float64 `json:"kb"`
	ModifiedAt int64   `json:"modifiedAt"`
}

// variantGrades are the grade segments of variant names, in the order a
// variant's level is recognised.
var variantGrades = []string{"lossless", "q80", "q60", "jpg", "png", "webp", "bmp", "gif"}

// originalStem strips a "-<grade>-<stamp>" variant suffix. Stamps have 17
// digits (with milliseconds) or 14 (older files).
func originalStem(stem string) string {
	parts := strings.Split(stem, "-")
	if len(parts) < 3 {
		return stem
	}
	last, grade := parts[len(parts)-1], parts[len(parts)-2]
	if (len(last) == 14 || len(last) == 17) && allDigits(last) && slices.Contains(variantGrades, grade) {
		return strings.Join(parts[:len(parts)-2], "-")
	}
	return stem
}

func allDigits(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] < '0' || s[i] > '9' {
			return false
		}
	}
	return true
}

func roundKB(size int64) float64 { return math.Round(float64(size)/1024*10) / 10 }

// listVariants returns the original and every variant of stem in dir, newest
// first.
func listVariants(dir, stem string) []imageVariantInfo {
	variants := []imageVariantInfo{}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return variants
	}
	for _, entry := range entries {
		name := entry.Name()
		path := filepath.Join(dir, name)
		info, err := os.Stat(path)
		if err != nil || !info.Mode().IsRegular() {
			continue
		}
		fileStem, _, _ := splitName(name)
		if originalStem(fileStem) != stem {
			continue
		}
		level := "other"
		if fileStem == stem {
			level = "original"
		} else {
			for _, grade := range variantGrades {
				if strings.Contains(fileStem, "-"+grade+"-") {
					level = grade
					break
				}
			}
		}
		variants = append(variants, imageVariantInfo{
			Path: path, FileName: name, Level: level,
			Bytes: info.Size(), KB: roundKB(info.Size()), ModifiedAt: max(info.ModTime().UnixMilli(), 0),
		})
	}
	slices.SortStableFunc(variants, func(a, b imageVariantInfo) int {
		switch {
		case a.ModifiedAt > b.ModifiedAt:
			return -1
		case a.ModifiedAt < b.ModifiedAt:
			return 1
		}
		return 0
	})
	return variants
}

func (s *Service) imagePath(path string) (string, error) {
	return s.allowedPath(path, "文件不存在或不可访问", "文件不在应用数据或输出目录中")
}

// inspectImage describes an image file and its variants. displayPath is the
// newest variant, which the library shows in place of the original.
func (s *Service) inspectImage(path string) (imageFileInfo, error) {
	canonical, err := s.imagePath(path)
	if err != nil {
		return imageFileInfo{}, err
	}
	info, err := os.Stat(canonical)
	if err != nil {
		return imageFileInfo{}, fmt.Errorf("读取文件信息失败：%w", err)
	}
	dir, name := filepath.Dir(canonical), filepath.Base(canonical)
	stem, ext, hasExt := splitName(name)
	variants := listVariants(dir, originalStem(stem))
	result := imageFileInfo{
		Path: canonical, Directory: dir, FileName: name,
		Bytes: info.Size(), KB: roundKB(info.Size()),
		DisplayPath: canonical, Variants: variants,
	}
	if len(variants) > 0 {
		result.DisplayPath = variants[0].Path
	}
	if file, err := os.Open(canonical); err == nil {
		cfg, _, err := image.DecodeConfig(file)
		file.Close()
		if err == nil {
			result.Width, result.Height = &cfg.Width, &cfg.Height
			if hasExt {
				format := asciiLower(ext)
				result.Format = &format
			}
		}
	}
	return result, nil
}

// readSourceImage loads and decodes an image for re-encoding.
func readSourceImage(path string) (image.Image, string, error) {
	info, err := os.Stat(path)
	if err != nil {
		return nil, "", fmt.Errorf("读取原图失败：%w", err)
	}
	if info.Size() > MaxMediaAssetBytes {
		return nil, "", errors.New("图片超过 512 MiB 大小限制")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, "", fmt.Errorf("读取原图失败：%w", err)
	}
	if len(data) > MaxMediaAssetBytes {
		return nil, "", errors.New("图片超过 512 MiB 大小限制")
	}
	format := DetectImageFormat(data)
	if format == "" {
		return nil, "", errors.New("不支持的图片格式")
	}
	img, _, err := DecodeImage(data)
	if err != nil {
		return nil, "", err
	}
	return img, format, nil
}

// compressImage writes a compressed variant: lossless keeps PNG/WebP (GIF and
// BMP become PNG, JPEG is re-encoded at quality 95); q80/q60 are JPEG.
func (s *Service) compressImage(path, level string) (store.AssetRef, error) {
	canonical, err := s.imagePath(path)
	if err != nil {
		return store.AssetRef{}, err
	}
	if level != "lossless" && level != "q80" && level != "q60" {
		return store.AssetRef{}, errors.New("未知压缩等级")
	}
	img, format, err := readSourceImage(canonical)
	if err != nil {
		return store.AssetRef{}, err
	}
	encoded, ext, err := encodeCompressed(img, format, level)
	if err != nil {
		return store.AssetRef{}, err
	}
	out, err := writeVariant(canonical, level, ext, encoded, "写入压缩图失败")
	if err != nil {
		return store.AssetRef{}, err
	}
	slog.Info("asset.image_compress.end", "source", canonical, "output", out, "level", level, "bytes", len(encoded))
	return imageAsset(out, img, ext), nil
}

// convertImage writes a copy of the image in another format.
func (s *Service) convertImage(path, format string) (store.AssetRef, error) {
	canonical, err := s.imagePath(path)
	if err != nil {
		return store.AssetRef{}, err
	}
	target := format
	switch format {
	case "jpg", "png", "webp", "bmp", "gif":
	case "jpeg":
		target = "jpg"
	default:
		return store.AssetRef{}, errors.New("不支持的目标格式")
	}
	img, _, err := readSourceImage(canonical)
	if err != nil {
		return store.AssetRef{}, err
	}
	encoded, err := encodeConverted(img, target)
	if err != nil {
		return store.AssetRef{}, err
	}
	out, err := writeVariant(canonical, target, target, encoded, "写入转换图失败")
	if err != nil {
		return store.AssetRef{}, err
	}
	slog.Info("asset.image_convert.end", "source", canonical, "output", out, "format", target, "bytes", len(encoded))
	return imageAsset(out, img, target), nil
}

func writeVariant(original, grade, ext string, data []byte, failure string) (string, error) {
	stem, _, _ := splitName(filepath.Base(original))
	name := fmt.Sprintf("%s-%s-%s.%s", originalStem(stem), grade, variantStamp(time.Now()), ext)
	out := filepath.Join(filepath.Dir(original), name)
	if err := os.WriteFile(out, data, 0o644); err != nil {
		return "", fmt.Errorf("%s：%w", failure, err)
	}
	return out, nil
}

func imageAsset(path string, img image.Image, format string) store.AssetRef {
	width, height := img.Bounds().Dx(), img.Bounds().Dy()
	return store.AssetRef{ID: uuid.NewString(), Kind: "image", Path: path, Width: &width, Height: &height, Format: format}
}

func encodeCompressed(img image.Image, format, level string) ([]byte, string, error) {
	switch {
	case level == "lossless" && format == "png":
		data, err := encodePNG(img)
		return wrapEncode(data, "png", err, "PNG 无损编码失败")
	case level == "lossless" && (format == "jpg" || format == "jpeg"):
		data, err := encodeJPEG(img, 95)
		return wrapEncode(data, "jpg", err, "JPEG 近无损编码失败")
	case level == "lossless" && format == "webp":
		data, err := encodeWebPLossless(img)
		return wrapEncode(data, "webp", err, "WebP 编码失败")
	case level == "lossless" && (format == "gif" || format == "bmp"):
		data, err := encodePNG(img)
		return wrapEncode(data, "png", err, "无损转 PNG 失败")
	case level == "q80" || level == "q60":
		switch format {
		case "jpg", "jpeg", "png", "webp", "gif", "bmp":
			quality := 60
			if level == "q80" {
				quality = 80
			}
			data, err := encodeJPEG(img, quality)
			return wrapEncode(data, "jpg", err, "JPEG 有损编码失败")
		}
	}
	return nil, "", fmt.Errorf("不支持压缩该格式：%s", format)
}

func wrapEncode(data []byte, ext string, err error, failure string) ([]byte, string, error) {
	if err != nil {
		return nil, "", fmt.Errorf("%s：%w", failure, err)
	}
	return data, ext, nil
}

func encodeConverted(img image.Image, format string) ([]byte, error) {
	var (
		data    []byte
		err     error
		failure string
	)
	switch format {
	case "jpg":
		failure = "JPEG 编码失败"
		data, err = encodeJPEG(img, 90)
	case "png":
		failure = "PNG 编码失败"
		data, err = encodePNG(img)
	case "webp":
		failure = "WebP 编码失败"
		data, err = encodeWebPLossless(img)
	case "bmp":
		failure = "BMP 编码失败"
		var buf bytes.Buffer
		err = bmp.Encode(&buf, img)
		data = buf.Bytes()
	case "gif":
		failure = "GIF 编码失败"
		data, err = encodeGIF(img)
	default:
		return nil, fmt.Errorf("不支持的目标格式：%s", format)
	}
	if err != nil {
		return nil, fmt.Errorf("%s：%w", failure, err)
	}
	return data, nil
}

func encodePNG(img image.Image) ([]byte, error) {
	var buf bytes.Buffer
	err := png.Encode(&buf, img)
	return buf.Bytes(), err
}

func encodeJPEG(img image.Image, quality int) ([]byte, error) {
	var buf bytes.Buffer
	err := jpeg.Encode(&buf, dropAlpha(img), &jpeg.Options{Quality: quality})
	return buf.Bytes(), err
}

// encodeGIF quantises to the Plan 9 palette with dithering. Pixels that are
// mostly transparent map to a transparent palette entry, so a transparent
// background survives the conversion.
func encodeGIF(img image.Image) ([]byte, error) {
	var buf bytes.Buffer
	if paletted, ok := img.(*image.Paletted); ok {
		err := gif.Encode(&buf, paletted, nil)
		return buf.Bytes(), err
	}
	if opaque, ok := img.(interface{ Opaque() bool }); ok && opaque.Opaque() {
		err := gif.Encode(&buf, img, nil)
		return buf.Bytes(), err
	}
	bounds := img.Bounds()
	colours := append(color.Palette{}, palette.Plan9[:255]...)
	out := image.NewPaletted(bounds, append(colours, color.Transparent))
	draw.FloydSteinberg.Draw(out, bounds, dropAlpha(img), bounds.Min)
	for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
		for x := bounds.Min.X; x < bounds.Max.X; x++ {
			if _, _, _, a := img.At(x, y).RGBA(); a < 0x8000 {
				out.SetColorIndex(x, y, 255)
			}
		}
	}
	err := gif.Encode(&buf, out, nil)
	return buf.Bytes(), err
}

// dropAlpha discards the alpha channel without blending, as JPEG output
// always did: a transparent pixel keeps its stored colour rather than
// turning black.
func dropAlpha(img image.Image) image.Image {
	if opaque, ok := img.(interface{ Opaque() bool }); ok && opaque.Opaque() {
		return img
	}
	bounds := img.Bounds()
	out := image.NewRGBA(bounds)
	switch src := img.(type) {
	case *image.NRGBA:
		for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
			row := src.Pix[src.PixOffset(bounds.Min.X, y):]
			dst := out.Pix[out.PixOffset(bounds.Min.X, y):]
			for x := 0; x < bounds.Dx()*4; x += 4 {
				dst[x], dst[x+1], dst[x+2], dst[x+3] = row[x], row[x+1], row[x+2], 0xFF
			}
		}
	case *image.NRGBA64:
		for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
			row := src.Pix[src.PixOffset(bounds.Min.X, y):]
			dst := out.Pix[out.PixOffset(bounds.Min.X, y):]
			for x := 0; x < bounds.Dx(); x++ {
				dst[x*4], dst[x*4+1], dst[x*4+2], dst[x*4+3] = row[x*8], row[x*8+2], row[x*8+4], 0xFF
			}
		}
	default:
		for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
			for x := bounds.Min.X; x < bounds.Max.X; x++ {
				c := color.NRGBAModel.Convert(img.At(x, y)).(color.NRGBA)
				out.SetRGBA(x, y, color.RGBA{R: c.R, G: c.G, B: c.B, A: 0xFF})
			}
		}
	}
	return out
}
