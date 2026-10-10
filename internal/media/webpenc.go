package media

import (
	"encoding/binary"
	"errors"
	"image"
	"image/draw"
	"slices"
)

// encodeWebPLossless writes img as a lossless WebP (VP8L) file, the only kind
// of WebP the application produces. The encoder keeps to the simple end of
// the format: the subtract-green transform, a predictor transform with one
// mode per 32×32 tile, and a single set of prefix codes over literal pixels
// (no LZ77 or colour cache). Pixel values, including the colour of fully
// transparent pixels, are preserved exactly.
func encodeWebPLossless(img image.Image) ([]byte, error) {
	width, height := img.Bounds().Dx(), img.Bounds().Dy()
	if width < 1 || height < 1 || width > 1<<14 || height > 1<<14 {
		return nil, errors.New("图片尺寸超出 WebP 限制")
	}
	pix := nrgbaPixels(img)
	hasAlpha := false
	for p := 3; p < len(pix); p += 4 {
		if pix[p] != 0xFF {
			hasAlpha = true
			break
		}
	}

	w := &bitWriter{buf: make([]byte, 0, len(pix)/2)}
	w.write(0x2F, 8) // VP8L signature
	w.write(uint32(width-1), 14)
	w.write(uint32(height-1), 14)
	w.write(boolBit(hasAlpha), 1)
	w.write(0, 3) // version

	// Transforms, inverted by the decoder in reverse order.
	subtractGreen(pix)
	w.write(1, 1)
	w.write(webpSubtractGreen, 2)

	modes := choosePredictors(pix, width, height)
	w.write(1, 1)
	w.write(webpPredictor, 2)
	w.write(webpTileBits-2, 3)
	modePix := make([]byte, 4*len(modes))
	for i, mode := range modes {
		modePix[4*i+1], modePix[4*i+3] = mode, 0xFF
	}
	writeEntropyImage(w, modePix, false)
	applyPredictors(pix, width, height, modes)
	w.write(0, 1) // no more transforms

	writeEntropyImage(w, pix, true)
	data := w.bytes()

	pad := len(data) & 1
	out := make([]byte, 0, 20+len(data)+pad)
	out = append(out, "RIFF"...)
	out = binary.LittleEndian.AppendUint32(out, uint32(4+8+len(data)+pad))
	out = append(out, "WEBPVP8L"...)
	out = binary.LittleEndian.AppendUint32(out, uint32(len(data)))
	out = append(out, data...)
	if pad == 1 {
		out = append(out, 0)
	}
	return out, nil
}

const (
	webpPredictor     = 0
	webpSubtractGreen = 2
	webpTileBits      = 5
)

func boolBit(b bool) uint32 {
	if b {
		return 1
	}
	return 0
}

// nrgbaPixels copies img into tightly packed, non-premultiplied RGBA bytes.
func nrgbaPixels(img image.Image) []byte {
	bounds := img.Bounds()
	if src, ok := img.(*image.NRGBA); ok {
		pix := make([]byte, 0, 4*bounds.Dx()*bounds.Dy())
		for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
			start := src.PixOffset(bounds.Min.X, y)
			pix = append(pix, src.Pix[start:start+4*bounds.Dx()]...)
		}
		return pix
	}
	rect := image.Rect(0, 0, bounds.Dx(), bounds.Dy())
	if opaque, ok := img.(interface{ Opaque() bool }); ok && opaque.Opaque() {
		// Without transparency premultiplied and straight alpha coincide, and
		// drawing into RGBA has fast paths for JPEG's YCbCr.
		dst := image.NewRGBA(rect)
		draw.Draw(dst, rect, img, bounds.Min, draw.Src)
		return dst.Pix
	}
	dst := image.NewNRGBA(rect)
	draw.Draw(dst, rect, img, bounds.Min, draw.Src)
	return dst.Pix
}

func subtractGreen(pix []byte) {
	for p := 0; p < len(pix); p += 4 {
		pix[p] -= pix[p+1]
		pix[p+2] -= pix[p+1]
	}
}

// predictorModes are the candidates tried per tile, best guess first:
// Select, L, T, ClampAddSubtractFull, Average2(L, T),
// ClampAddSubtractHalf(Average2(L, T), TL).
var predictorModes = []uint8{11, 1, 2, 12, 7, 13}

func tiles(size int) int { return (size + 1<<webpTileBits - 1) >> webpTileBits }

// predict returns the prediction for the pixel at offset p, whose left, top
// and top-left neighbours exist.
func predict(pix []byte, p, stride int, mode uint8) [4]byte {
	left, top, topLeft := pix[p-4:p], pix[p-stride:p-stride+4], pix[p-stride-4:p-stride]
	var out [4]byte
	switch mode {
	case 1:
		copy(out[:], left)
	case 2:
		copy(out[:], top)
	case 7:
		for c := range out {
			out[c] = avg2(left[c], top[c])
		}
	case 11:
		var distLeft, distTop int
		for c := range out {
			distLeft += absInt(int(topLeft[c]) - int(top[c]))
			distTop += absInt(int(topLeft[c]) - int(left[c]))
		}
		if distLeft < distTop {
			copy(out[:], left)
		} else {
			copy(out[:], top)
		}
	case 12:
		for c := range out {
			out[c] = clamp255(int(left[c]) + int(top[c]) - int(topLeft[c]))
		}
	case 13:
		for c := range out {
			a := int(avg2(left[c], top[c]))
			out[c] = clamp255(a + (a-int(topLeft[c]))/2)
		}
	}
	return out
}

func avg2(a, b byte) byte { return byte((int(a) + int(b)) / 2) }

func clamp255(v int) byte { return byte(min(max(v, 0), 255)) }

func absInt(v int) int {
	if v < 0 {
		return -v
	}
	return v
}

// choosePredictors picks, per tile, the mode with the smallest residuals.
func choosePredictors(pix []byte, width, height int) []uint8 {
	tilesX, tilesY := tiles(width), tiles(height)
	modes := make([]uint8, tilesX*tilesY)
	stride := 4 * width
	for ty := range tilesY {
		for tx := range tilesX {
			best, bestCost := predictorModes[0], -1
			for _, mode := range predictorModes {
				cost := 0
				for y := max(ty<<webpTileBits, 1); y < min((ty+1)<<webpTileBits, height); y++ {
					for x := max(tx<<webpTileBits, 1); x < min((tx+1)<<webpTileBits, width); x++ {
						p := y*stride + 4*x
						pred := predict(pix, p, stride, mode)
						for c := range pred {
							d := int(pix[p+c] - pred[c])
							cost += min(d, 256-d)
						}
					}
				}
				if bestCost < 0 || cost < bestCost {
					best, bestCost = mode, cost
				}
			}
			modes[ty*tilesX+tx] = best
		}
	}
	return modes
}

// applyPredictors replaces every pixel by its residual. Walking backwards
// keeps the neighbours each prediction reads unmodified.
func applyPredictors(pix []byte, width, height int, modes []uint8) {
	stride, tilesX := 4*width, tiles(width)
	for y := height - 1; y >= 0; y-- {
		for x := width - 1; x >= 0; x-- {
			p := y*stride + 4*x
			var pred [4]byte
			switch {
			case x == 0 && y == 0:
				pred = [4]byte{0, 0, 0, 0xFF} // opaque black
			case y == 0:
				copy(pred[:], pix[p-4:p])
			case x == 0:
				copy(pred[:], pix[p-stride:p-stride+4])
			default:
				pred = predict(pix, p, stride, modes[(y>>webpTileBits)*tilesX+x>>webpTileBits])
			}
			for c := range pred {
				pix[p+c] -= pred[c]
			}
		}
	}
}

// writeEntropyImage writes pixels as literals under one set of prefix codes.
// Only the main image may carry meta prefix codes; we never use them.
func writeEntropyImage(w *bitWriter, pix []byte, mainImage bool) {
	w.write(0, 1) // no colour cache
	if mainImage {
		w.write(0, 1) // no meta prefix codes
	}
	var (
		green               = make([]uint32, 256+24)
		red, blue, alpha    = make([]uint32, 256), make([]uint32, 256), make([]uint32, 256)
		distance            = make([]uint32, 40)
		histograms          = [][]uint32{green, red, blue, alpha, distance}
		channelOfHistograms = [4]int{1, 0, 2, 3} // green, red, blue, alpha
	)
	for p := 0; p < len(pix); p += 4 {
		green[pix[p+1]]++
		red[pix[p]]++
		blue[pix[p+2]]++
		alpha[pix[p+3]]++
	}
	codes := make([]prefixCode, len(histograms))
	for i, histogram := range histograms {
		codes[i] = newPrefixCode(histogram)
		codes[i].writeHeader(w)
	}
	for p := 0; p < len(pix); p += 4 {
		for i, channel := range channelOfHistograms {
			symbol := pix[p+channel]
			w.write(uint32(codes[i].codes[symbol]), uint(codes[i].depths[symbol]))
		}
	}
}

// prefixCode is a canonical prefix (Huffman) code. A code with at most one
// used symbol is sent as a "simple" code and costs no bits per symbol.
type prefixCode struct {
	simple  bool
	symbol  int     // the symbol of a simple code
	lengths []uint8 // declared code lengths of a normal code
	depths  []uint8 // bits written per symbol
	codes   []uint16
}

func newPrefixCode(histogram []uint32) prefixCode {
	used := 0
	symbol := 0
	for s, count := range histogram {
		if count > 0 {
			used++
			symbol = s
		}
	}
	if used <= 1 {
		return prefixCode{simple: true, symbol: symbol,
			depths: make([]uint8, len(histogram)), codes: make([]uint16, len(histogram))}
	}
	lengths := huffmanLengths(histogram, 15)
	return prefixCode{lengths: lengths, depths: lengths, codes: canonicalCodes(lengths)}
}

// codeLengthOrder is the order in which code-length code lengths are sent.
var codeLengthOrder = [19]int{17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15}

type codeLengthToken struct{ symbol, extraBits, extra uint8 }

func (c prefixCode) writeHeader(w *bitWriter) {
	if c.simple {
		w.write(1, 1) // simple code
		w.write(0, 1) // one symbol
		if c.symbol < 2 {
			w.write(0, 1)
			w.write(uint32(c.symbol), 1)
		} else {
			w.write(1, 1)
			w.write(uint32(c.symbol), 8)
		}
		return
	}
	w.write(0, 1) // normal code

	// Code lengths are themselves prefix coded; runs of zeros use symbols
	// 17 (3-10 zeros) and 18 (11-138 zeros).
	var tokens []codeLengthToken
	for i := 0; i < len(c.lengths); {
		length, run := c.lengths[i], 1
		for i+run < len(c.lengths) && c.lengths[i+run] == length {
			run++
		}
		i += run
		if length == 0 {
			for run >= 11 {
				n := min(run, 138)
				tokens = append(tokens, codeLengthToken{18, 7, uint8(n - 11)})
				run -= n
			}
			if run >= 3 {
				tokens = append(tokens, codeLengthToken{17, 3, uint8(run - 3)})
				run = 0
			}
		}
		for ; run > 0; run-- {
			tokens = append(tokens, codeLengthToken{symbol: length})
		}
	}
	histogram := make([]uint32, 19)
	for _, t := range tokens {
		histogram[t.symbol]++
	}
	lengths := huffmanLengths(histogram, 7)
	codes := canonicalCodes(lengths)
	depths := lengths
	if used := len(histogram) - countZero(histogram); used == 1 {
		depths = make([]uint8, len(lengths)) // a lone symbol is read with no bits
	}
	count := 4
	for i, symbol := range codeLengthOrder {
		if lengths[symbol] != 0 {
			count = max(count, i+1)
		}
	}
	w.write(uint32(count-4), 4)
	for _, symbol := range codeLengthOrder[:count] {
		w.write(uint32(lengths[symbol]), 3)
	}
	w.write(0, 1) // lengths follow for the whole alphabet
	for _, t := range tokens {
		w.write(uint32(codes[t.symbol]), uint(depths[t.symbol]))
		w.write(uint32(t.extra), uint(t.extraBits))
	}
}

func countZero(values []uint32) int {
	n := 0
	for _, v := range values {
		if v == 0 {
			n++
		}
	}
	return n
}

// huffmanLengths returns optimal code lengths for histogram no longer than
// maxLen. When the tree is too deep the counts are flattened and the tree
// rebuilt, which converges because equal counts give a balanced tree.
func huffmanLengths(histogram []uint32, maxLen int) []uint8 {
	type node struct {
		weight      uint64
		left, right int
	}
	var symbols []int
	for s, count := range histogram {
		if count > 0 {
			symbols = append(symbols, s)
		}
	}
	lengths := make([]uint8, len(histogram))
	switch len(symbols) {
	case 0:
		return lengths
	case 1:
		lengths[symbols[0]] = 1
		return lengths
	}
	for shift := uint(0); ; shift++ {
		n := len(symbols)
		nodes := make([]node, n, 2*n-1)
		for i, s := range symbols {
			nodes[i] = node{weight: max(uint64(histogram[s])>>shift, 1), left: -1, right: s}
		}
		slices.SortStableFunc(nodes, func(a, b node) int {
			switch {
			case a.weight < b.weight:
				return -1
			case a.weight > b.weight:
				return 1
			}
			return 0
		})
		// Two-queue construction: merged nodes are created in non-decreasing
		// weight order, so the smallest is always at the front of a queue.
		leaf, merged := 0, n
		take := func() int {
			if leaf < n && (merged >= len(nodes) || nodes[leaf].weight <= nodes[merged].weight) {
				leaf++
				return leaf - 1
			}
			merged++
			return merged - 1
		}
		for range n - 1 {
			a, b := take(), take()
			nodes = append(nodes, node{weight: nodes[a].weight + nodes[b].weight, left: a, right: b})
		}
		depth := make([]int, len(nodes))
		deepest := 0
		for i := len(nodes) - 1; i >= n; i-- {
			for _, child := range []int{nodes[i].left, nodes[i].right} {
				depth[child] = depth[i] + 1
				deepest = max(deepest, depth[child])
			}
		}
		if deepest > maxLen {
			continue
		}
		for i := range n {
			lengths[nodes[i].right] = uint8(depth[i])
		}
		return lengths
	}
}

// canonicalCodes assigns canonical codes, bit-reversed because the stream is
// written least significant bit first while codes are read most significant
// bit first.
func canonicalCodes(lengths []uint8) []uint16 {
	var count, next [16]int
	for _, l := range lengths {
		if l > 0 {
			count[l]++
		}
	}
	code := 0
	for l := 1; l < 16; l++ {
		code = (code + count[l-1]) << 1
		next[l] = code
	}
	codes := make([]uint16, len(lengths))
	for s, l := range lengths {
		if l == 0 {
			continue
		}
		c := next[l]
		next[l]++
		reversed := 0
		for range l {
			reversed = reversed<<1 | c&1
			c >>= 1
		}
		codes[s] = uint16(reversed)
	}
	return codes
}

// bitWriter packs values least significant bit first.
type bitWriter struct {
	buf   []byte
	acc   uint64
	nbits uint
}

func (w *bitWriter) write(value uint32, n uint) {
	w.acc |= uint64(value) << w.nbits
	w.nbits += n
	for w.nbits >= 8 {
		w.buf = append(w.buf, byte(w.acc))
		w.acc >>= 8
		w.nbits -= 8
	}
}

func (w *bitWriter) bytes() []byte {
	if w.nbits > 0 {
		w.buf = append(w.buf, byte(w.acc))
		w.acc, w.nbits = 0, 0
	}
	return w.buf
}
