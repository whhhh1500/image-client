package gen

import (
	"bytes"
	"image"
	"image/color"
	"image/jpeg"
	"image/png"
	"math/rand/v2"
	"testing"
)

// noisyPNG is hard to compress, like a detailed generated image.
func noisyPNG(t *testing.T, w, h int, alpha uint8) []byte {
	t.Helper()
	img := image.NewNRGBA(image.Rect(0, 0, w, h))
	r := rand.New(rand.NewPCG(1, 2))
	for i := 0; i < len(img.Pix); i += 4 {
		img.Pix[i], img.Pix[i+1], img.Pix[i+2], img.Pix[i+3] = uint8(r.IntN(256)), uint8(r.IntN(256)), uint8(r.IntN(256)), alpha
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func TestCompactForUploadShrinksLargeImages(t *testing.T) {
	original := noisyPNG(t, 1254, 1254, 255)
	if len(original) <= uploadCompactOver {
		t.Fatalf("fixture too small: %d bytes", len(original))
	}
	data, mime := compactForUpload(original, "image/png")
	if mime != "image/jpeg" || len(data) >= len(original) {
		t.Fatalf("mime %s, %d → %d bytes", mime, len(original), len(data))
	}
	cfg, err := jpeg.DecodeConfig(bytes.NewReader(data))
	if err != nil || cfg.Width != 1254 || cfg.Height != 1254 {
		t.Fatalf("decoded %v %dx%d", err, cfg.Width, cfg.Height)
	}
}

func TestCompactForUploadCapsTheLongestEdge(t *testing.T) {
	data, mime := compactForUpload(noisyPNG(t, 3000, 1500, 255), "image/png")
	cfg, err := jpeg.DecodeConfig(bytes.NewReader(data))
	if mime != "image/jpeg" || err != nil || cfg.Width != 2048 || cfg.Height != 1024 {
		t.Fatalf("mime %s, err %v, %dx%d", mime, err, cfg.Width, cfg.Height)
	}
}

func TestCompactForUploadKeepsSmallImages(t *testing.T) {
	img := image.NewRGBA(image.Rect(0, 0, 64, 64))
	var buf bytes.Buffer
	png.Encode(&buf, img)
	data, mime := compactForUpload(buf.Bytes(), "image/png")
	if mime != "image/png" || !bytes.Equal(data, buf.Bytes()) {
		t.Fatalf("small image changed: %s, %d bytes", mime, len(data))
	}
}

func TestCompactForUploadFlattensTransparencyOntoWhite(t *testing.T) {
	data, mime := compactForUpload(noisyPNG(t, 1200, 1200, 0), "image/png")
	if mime != "image/jpeg" {
		t.Fatalf("mime %s", mime)
	}
	img, err := jpeg.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	r, g, b, _ := img.At(600, 600).RGBA()
	white := color.White
	wr, wg, wb, _ := white.RGBA()
	near := func(x, y uint32) bool { return x+0x0800 >= y }
	if !near(r, wr) || !near(g, wg) || !near(b, wb) {
		t.Fatalf("transparent pixel became %d,%d,%d, want near white", r>>8, g>>8, b>>8)
	}
}
