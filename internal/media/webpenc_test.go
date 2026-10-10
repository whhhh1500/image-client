package media

import (
	"bytes"
	"image"
	"image/color"
	"image/jpeg"
	"math/rand/v2"
	"testing"

	"golang.org/x/image/webp"
)

func TestWebPLosslessRoundTripsExactly(t *testing.T) {
	rng := rand.New(rand.NewPCG(1, 2))
	noise := func(w, h int, alpha bool) *image.NRGBA {
		img := image.NewNRGBA(image.Rect(0, 0, w, h))
		for i := range img.Pix {
			img.Pix[i] = byte(rng.UintN(256))
			if i%4 == 3 && !alpha {
				img.Pix[i] = 0xFF
			}
		}
		return img
	}
	solid := func(w, h int, c color.NRGBA) *image.NRGBA {
		img := image.NewNRGBA(image.Rect(0, 0, w, h))
		for y := range h {
			for x := range w {
				img.SetNRGBA(x, y, c)
			}
		}
		return img
	}
	twoColours := solid(70, 45, color.NRGBA{10, 20, 30, 255})
	for y := 0; y < 45; y += 2 {
		for x := range 70 {
			twoColours.SetNRGBA(x, y, color.NRGBA{250, 5, 7, 128})
		}
	}
	transparentColour := solid(9, 9, color.NRGBA{123, 45, 67, 0})
	cases := map[string]*image.NRGBA{
		"1x1":                solid(1, 1, color.NRGBA{1, 2, 3, 4}),
		"row":                testImage(50, 1),
		"column":             testImage(1, 50),
		"gradient":           testImage(97, 65),
		"noise with alpha":   noise(37, 23, true),
		"opaque noise":       noise(64, 64, false),
		"two colours":        twoColours,
		"transparent colour": transparentColour,
		"large flat":         solid(300, 200, color.NRGBA{0, 0, 0, 255}),
	}
	for name, img := range cases {
		data, err := encodeWebPLossless(img)
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if DetectImageFormat(data) != "webp" {
			t.Fatalf("%s: not detected as WebP", name)
		}
		decoded, err := webp.Decode(bytes.NewReader(data))
		if err != nil {
			t.Fatalf("%s: decode: %v", name, err)
		}
		assertSamePixels(t, name, img, decoded)
	}
}

func TestWebPLosslessAcceptsOtherImageTypes(t *testing.T) {
	var buf bytes.Buffer
	if err := jpeg.Encode(&buf, testImage(31, 19), nil); err != nil {
		t.Fatal(err)
	}
	src, err := jpeg.Decode(&buf) // *image.YCbCr
	if err != nil {
		t.Fatal(err)
	}
	data, err := encodeWebPLossless(src)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := webp.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	want := image.NewNRGBA(src.Bounds())
	for y := range 19 {
		for x := range 31 {
			want.Set(x, y, src.At(x, y))
		}
	}
	assertSamePixels(t, "ycbcr", want, decoded)

	if _, err := encodeWebPLossless(image.NewNRGBA(image.Rect(0, 0, 1<<14+1, 1))); err == nil {
		t.Fatal("an image wider than WebP allows must be rejected")
	}
}

func TestHuffmanLengthsAreLimitedAndComplete(t *testing.T) {
	// Fibonacci counts produce the deepest possible tree.
	histogram := make([]uint32, 40)
	a, b := uint32(1), uint32(1)
	for i := range histogram {
		histogram[i] = a
		a, b = b, a+b
	}
	histogram[3] = 0
	for _, limit := range []int{15, 7} {
		lengths := huffmanLengths(histogram, limit)
		kraft := 0.0
		for s, l := range lengths {
			if int(l) > limit || (l == 0) != (histogram[s] == 0) {
				t.Fatalf("limit %d: symbol %d has length %d", limit, s, l)
			}
			if l > 0 {
				kraft += 1 / float64(uint64(1)<<l)
			}
		}
		if kraft != 1 {
			t.Fatalf("limit %d: Kraft sum %v, the code must be complete", limit, kraft)
		}
	}
}
