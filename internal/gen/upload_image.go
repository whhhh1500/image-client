package gen

import (
	"bytes"
	"image"
	"image/color"
	"image/jpeg"
	"log/slog"

	"golang.org/x/image/draw"

	"imageclient/internal/media"
)

// Local reference images travel inside the video request as data URLs. A
// full-size generated PNG (about 2.5 MB at 1254×1254) pushed task creation past
// its time limit on a slow uplink, so large images are re-encoded as JPEG and
// capped in size before upload; providers scale references down anyway.
const (
	uploadCompactOver = 512 << 10 // smaller images are sent unchanged
	uploadMaxEdge     = 2048
	uploadJPEGQuality = 90
)

// compactForUpload returns a smaller JPEG rendition of data when that helps,
// otherwise data unchanged. Transparency is flattened onto white, as a video
// first frame has no alpha channel.
func compactForUpload(data []byte, mime string) ([]byte, string) {
	if len(data) <= uploadCompactOver {
		return data, mime
	}
	src, _, err := media.DecodeImage(data)
	if err != nil {
		return data, mime
	}
	bounds := src.Bounds()
	w, h := bounds.Dx(), bounds.Dy()
	if longest := max(w, h); longest > uploadMaxEdge {
		w, h = max(1, w*uploadMaxEdge/longest), max(1, h*uploadMaxEdge/longest)
	}
	canvas := image.NewRGBA(image.Rect(0, 0, w, h))
	draw.Draw(canvas, canvas.Bounds(), image.NewUniform(color.White), image.Point{}, draw.Src)
	draw.CatmullRom.Scale(canvas, canvas.Bounds(), src, bounds, draw.Over, nil)

	var out bytes.Buffer
	if err := jpeg.Encode(&out, canvas, &jpeg.Options{Quality: uploadJPEGQuality}); err != nil || out.Len() >= len(data) {
		return data, mime
	}
	slog.Info("video.local_image_compacted", "fromBytes", len(data), "toBytes", out.Len(),
		"fromSize", []int{bounds.Dx(), bounds.Dy()}, "toSize", []int{w, h})
	return out.Bytes(), "image/jpeg"
}
