package media

import (
	"bytes"
	"encoding/json"
	"image"
	"image/color"
	"image/gif"
	"image/jpeg"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	"golang.org/x/image/bmp"
)

func TestOriginalStemStripsVariantSuffixes(t *testing.T) {
	for stem, want := range map[string]string{
		"abc":                         "abc",
		"abc-lossless-20260901010101": "abc",
		"hero-q80-20260901010101":     "hero",
		"hero-q60-20260901010101":     "hero",
		"hero-png-20260901010101":     "hero",
		"hero-q80-20260925123456789":  "hero",
		"my-hero-webp-20260925123456": "my-hero",
		"hero-q80-2026092512345":      "hero-q80-2026092512345",  // 13 digits
		"hero-q90-20260901010101":     "hero-q90-20260901010101", // unknown grade
		"q80-20260901010101":          "q80-20260901010101",      // needs three parts
		"hero-q80-2026090101010a":     "hero-q80-2026090101010a",
	} {
		if got := originalStem(stem); got != want {
			t.Errorf("originalStem(%q) = %q, want %q", stem, got, want)
		}
	}
}

func TestListVariantsFindsTheOriginalAndItsVariantsNewestFirst(t *testing.T) {
	dir := t.TempDir()
	base := time.Date(2026, 9, 1, 1, 1, 1, 0, time.UTC)
	files := []struct {
		name  string
		age   time.Duration
		level string
	}{
		{"hero.png", 5 * time.Hour, "original"},
		{"hero-q80-20260901010101.jpg", 4 * time.Hour, "q80"},
		{"hero-lossless-20260901010101123.png", 1 * time.Hour, "lossless"},
		{"hero-webp-20260901010101123.webp", 2 * time.Hour, "webp"},
		{"hero-gif-20260901010101123.gif", 3 * time.Hour, "gif"},
		{"other.png", 0, ""},
		{"hero-q80-123.jpg", 0, ""},
		{"heroic.png", 0, ""},
	}
	for _, f := range files {
		path := writeFile(t, filepath.Join(dir, f.name), []byte("x"))
		mtime := base.Add(-f.age)
		os.Chtimes(path, mtime, mtime)
	}
	os.Mkdir(filepath.Join(dir, "hero-q60-20260901010101.png"), 0o755) // directories are skipped

	variants := listVariants(dir, "hero")
	var got []string
	for _, v := range variants {
		got = append(got, v.Level+":"+v.FileName)
	}
	want := []string{
		"lossless:hero-lossless-20260901010101123.png", "webp:hero-webp-20260901010101123.webp",
		"gif:hero-gif-20260901010101123.gif", "q80:hero-q80-20260901010101.jpg", "original:hero.png",
	}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("variants = %v\nwant %v", got, want)
	}
	if v := variants[0]; v.Bytes != 1 || v.KB != 0 || v.ModifiedAt != base.Add(-time.Hour).UnixMilli() || v.Path != filepath.Join(dir, v.FileName) {
		t.Fatalf("variant info = %+v", v)
	}
	if empty := listVariants(filepath.Join(dir, "missing"), "hero"); empty == nil || len(empty) != 0 {
		t.Fatalf("missing dir must give an empty list, got %#v", empty)
	}
}

func encodeAs(t *testing.T, img image.Image, format string) []byte {
	t.Helper()
	var buf bytes.Buffer
	var err error
	switch format {
	case "png":
		return pngBytes(t, img)
	case "jpg":
		err = jpeg.Encode(&buf, img, &jpeg.Options{Quality: 90})
	case "gif":
		err = gif.Encode(&buf, img, nil)
	case "bmp":
		err = bmp.Encode(&buf, img)
	case "webp":
		data, err := encodeWebPLossless(img)
		if err != nil {
			t.Fatal(err)
		}
		return data
	}
	if err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func TestCompressWritesVariantsNextToTheOriginal(t *testing.T) {
	env := newTestEnv(t)
	img := testImage(40, 30)
	wantExt := map[string]map[string]string{
		"png":  {"lossless": "png", "q80": "jpg", "q60": "jpg"},
		"jpg":  {"lossless": "jpg", "q80": "jpg", "q60": "jpg"},
		"webp": {"lossless": "webp", "q80": "jpg", "q60": "jpg"},
		"gif":  {"lossless": "png", "q80": "jpg", "q60": "jpg"},
		"bmp":  {"lossless": "png", "q80": "jpg", "q60": "jpg"},
	}
	for format, levels := range wantExt {
		original := writeFile(t, filepath.Join(env.output, "图片", "run", "hero_"+format+"."+format), encodeAs(t, img, format))
		for level, ext := range levels {
			asset, err := env.svc.compressImage(original, level)
			if err != nil {
				t.Fatalf("%s/%s: %v", format, level, err)
			}
			pattern := `^hero_` + format + `-` + level + `-\d{17}\.` + ext + `$`
			if !regexp.MustCompile(pattern).MatchString(filepath.Base(asset.Path)) || asset.Format != ext || asset.Kind != "image" {
				t.Fatalf("%s/%s: asset %+v", format, level, asset)
			}
			if filepath.Dir(asset.Path) != filepath.Dir(original) {
				t.Fatalf("%s/%s: variant written to %s", format, level, asset.Path)
			}
			data, _ := os.ReadFile(asset.Path)
			decoded, _, err := DecodeImage(data)
			if err != nil || decoded.Bounds().Dx() != 40 || decoded.Bounds().Dy() != 30 || *asset.Width != 40 || *asset.Height != 30 {
				t.Fatalf("%s/%s: decoded %v %v", format, level, decoded.Bounds(), err)
			}
			if DetectImageFormat(data) != ext {
				t.Fatalf("%s/%s: wrote %s data", format, level, DetectImageFormat(data))
			}
		}
	}
}

func TestCompressingAVariantKeepsTheOriginalStem(t *testing.T) {
	env := newTestEnv(t)
	variant := writeFile(t, filepath.Join(env.output, "hero-q80-20260901010101123.png"), pngBytes(t, testImage(4, 4)))
	asset, err := env.svc.compressImage(variant, "q60")
	if err != nil {
		t.Fatal(err)
	}
	if !regexp.MustCompile(`^hero-q60-\d{17}\.jpg$`).MatchString(filepath.Base(asset.Path)) {
		t.Fatalf("variant of a variant named %s", filepath.Base(asset.Path))
	}
}

func TestConvertRoundTripsEveryTargetFormat(t *testing.T) {
	env := newTestEnv(t)
	img := testImage(33, 17)
	original := writeFile(t, filepath.Join(env.output, "pic.png"), pngBytes(t, img))
	for _, format := range []string{"jpg", "jpeg", "png", "webp", "bmp", "gif"} {
		asset, err := env.svc.convertImage(original, format)
		if err != nil {
			t.Fatalf("%s: %v", format, err)
		}
		target := format
		if format == "jpeg" {
			target = "jpg"
		}
		if !regexp.MustCompile(`^pic-`+target+`-\d{17}\.`+target+`$`).MatchString(filepath.Base(asset.Path)) || asset.Format != target {
			t.Fatalf("%s: asset %+v", format, asset)
		}
		data, _ := os.ReadFile(asset.Path)
		decoded, _, err := DecodeImage(data)
		if err != nil || decoded.Bounds() != image.Rect(0, 0, 33, 17) || DetectImageFormat(data) != target {
			t.Fatalf("%s: decoded %v %v", format, decoded, err)
		}
		if target == "png" || target == "webp" {
			assertSamePixels(t, target, img, decoded)
		}
	}
}

func assertSamePixels(t *testing.T, label string, want *image.NRGBA, got image.Image) {
	t.Helper()
	for y := 0; y < want.Rect.Dy(); y++ {
		for x := 0; x < want.Rect.Dx(); x++ {
			w := want.NRGBAAt(x, y)
			g := color.NRGBAModel.Convert(got.At(got.Bounds().Min.X+x, got.Bounds().Min.Y+y)).(color.NRGBA)
			if w != g {
				t.Fatalf("%s: pixel (%d,%d) = %v, want %v", label, x, y, g, w)
			}
		}
	}
}

func TestImageCommandsRejectBadInput(t *testing.T) {
	env := newTestEnv(t)
	outside := writeFile(t, filepath.Join(t.TempDir(), "x.png"), pngBytes(t, testImage(2, 2)))
	inside := writeFile(t, filepath.Join(env.paths.Root, "x.png"), pngBytes(t, testImage(2, 2)))
	text := writeFile(t, filepath.Join(env.output, "x.txt"), []byte("not an image"))
	truncated := writeFile(t, filepath.Join(env.output, "t.png"), []byte("\x89PNG\r\n\x1a\n"))

	cases := []struct {
		name string
		run  func() error
		want string
	}{
		{"outside compress", func() error { _, err := env.svc.compressImage(outside, "q80"); return err }, "文件不在应用数据或输出目录中"},
		{"outside convert", func() error { _, err := env.svc.convertImage(outside, "png"); return err }, "文件不在应用数据或输出目录中"},
		{"outside inspect", func() error { _, err := env.svc.inspectImage(outside); return err }, "文件不在应用数据或输出目录中"},
		{"unknown level", func() error { _, err := env.svc.compressImage(inside, "q10"); return err }, "未知压缩等级"},
		{"unknown format", func() error { _, err := env.svc.convertImage(inside, "tiff"); return err }, "不支持的目标格式"},
		{"not an image", func() error { _, err := env.svc.compressImage(text, "q80"); return err }, "不支持的图片格式"},
		{"not an image convert", func() error { _, err := env.svc.convertImage(text, "png"); return err }, "不支持的图片格式"},
	}
	for _, tc := range cases {
		if err := tc.run(); errText(err) != tc.want {
			t.Errorf("%s: error %q, want %q", tc.name, errText(err), tc.want)
		}
	}
	if _, err := env.svc.compressImage(filepath.Join(env.output, "missing.png"), "q80"); !strings.HasPrefix(errText(err), "文件不存在或不可访问: ") {
		t.Errorf("missing file: %v", err)
	}
	if _, err := env.svc.convertImage(truncated, "png"); !strings.HasPrefix(errText(err), "识别图片格式失败") && !strings.HasPrefix(errText(err), "解码图片失败") {
		t.Errorf("truncated image: %v", err)
	}
}

func TestInspectImageReportsVariantsAndTheLatestDisplayPath(t *testing.T) {
	env := newTestEnv(t)
	original := writeFile(t, filepath.Join(env.output, "图片", "shot.PNG"), pngBytes(t, testImage(12, 8)))
	old := time.Now().Add(-time.Hour)
	os.Chtimes(original, old, old)
	variant, err := env.svc.compressImage(original, "q80")
	if err != nil {
		t.Fatal(err)
	}

	for _, path := range []string{original, variant.Path} {
		info, err := env.svc.inspectImage(path)
		if err != nil {
			t.Fatal(err)
		}
		if info.DisplayPath != variant.Path || len(info.Variants) != 2 || info.Variants[1].Level != "original" {
			t.Fatalf("inspect %s: %+v", filepath.Base(path), info)
		}
		if info.Directory != filepath.Dir(original) || info.FileName != filepath.Base(path) || info.Width == nil || *info.Width != 12 || *info.Height != 8 {
			t.Fatalf("inspect %s: %+v", filepath.Base(path), info)
		}
	}
	info, _ := env.svc.inspectImage(original)
	if *info.Format != "png" || info.KB != roundKB(info.Bytes) {
		t.Fatalf("format/kb = %v %v", *info.Format, info.KB)
	}

	// A non-image inside the roots is described without dimensions.
	text := writeFile(t, filepath.Join(env.output, "notes.txt"), []byte("hello"))
	info, err = env.svc.inspectImage(text)
	if err != nil {
		t.Fatal(err)
	}
	encoded, _ := json.Marshal(info)
	for _, want := range []string{`"width":null`, `"height":null`, `"format":null`, `"variants":[{`, `"displayPath":`} {
		if !strings.Contains(string(encoded), want) {
			t.Errorf("inspect JSON %s lacks %s", encoded, want)
		}
	}
}

func TestJPEGOutputDropsAlphaWithoutBlackening(t *testing.T) {
	img := image.NewNRGBA(image.Rect(0, 0, 8, 8))
	for i := 0; i < len(img.Pix); i += 4 {
		copy(img.Pix[i:], []byte{200, 40, 40, 0}) // fully transparent red
	}
	data, err := encodeJPEG(img, 95)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := jpeg.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	r, g, _, _ := decoded.At(4, 4).RGBA()
	if r>>8 < 180 || g>>8 > 70 {
		t.Fatalf("transparent red became %d,%d", r>>8, g>>8)
	}
}

func TestGIFConversionKeepsTransparency(t *testing.T) {
	img := image.NewNRGBA(image.Rect(0, 0, 6, 4))
	for y := range 4 {
		for x := range 6 {
			c := color.NRGBA{R: 30, G: 200, B: 90, A: 255}
			if x < 3 {
				c.A = 0
			}
			img.SetNRGBA(x, y, c)
		}
	}
	data, err := encodeGIF(img)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := gif.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	if _, _, _, a := decoded.At(1, 1).RGBA(); a != 0 {
		t.Fatalf("transparent pixel has alpha %d", a)
	}
	if r, g, _, a := decoded.At(4, 2).RGBA(); a != 0xFFFF || g>>8 < 150 || r>>8 > 90 {
		t.Fatalf("opaque pixel became %d,%d alpha %d", r>>8, g>>8, a)
	}
}
