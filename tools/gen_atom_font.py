"""Generate PuzilAtoms.ttf: an invisible spacer font for reserving exact inline widths.

Glyph for U+E000+k has advance 2**k font units (k = 0..15), with no outlines, so any
width of 0..65535 units is a short run of these code points (its binary expansion).
At font size S px, one unit is S / UPEM px. GPUI lays out the run normally and Puzil
paints the real inline atom (math, emoji, pill, badge) into the reserved slot.

Run: uv run --with fonttools tools/gen_atom_font.py assets/fonts/PuzilAtoms.ttf
"""
import sys
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

UPEM = 4096
BITS = 16

def empty():
    return TTGlyphPen(None).glyph()

# GPUI's macOS loader skips any font without an 'm' glyph, so carry an empty one.
# U+2060 WORD JOINER (zero width) glues pill padding to its label so they wrap together.
names = [".notdef", "m", "wj"] + [f"w{k}" for k in range(BITS)]
fb = FontBuilder(UPEM, isTTF=True)
fb.setupGlyphOrder(names)
cmap = {0xE000 + k: f"w{k}" for k in range(BITS)}
cmap[ord("m")] = "m"
cmap[0x2060] = "wj"
fb.setupCharacterMap(cmap)
fb.setupGlyf({n: empty() for n in names})
metrics = {".notdef": (0, 0), "m": (UPEM // 2, 0), "wj": (0, 0)}
metrics.update({f"w{k}": (1 << k, 0) for k in range(BITS)})  # uint16: 2**15 fits
fb.setupHorizontalMetrics(metrics)
ASC, DESC = int(UPEM * 0.8), int(UPEM * 0.2)
fb.setupHorizontalHeader(ascent=ASC, descent=-DESC)
fb.setupNameTable({"familyName": "PuzilAtoms", "styleName": "Regular", "uniqueFontIdentifier": "PuzilAtoms-Regular", "fullName": "PuzilAtoms Regular", "psName": "PuzilAtoms-Regular"})
fb.setupOS2(sTypoAscender=ASC, sTypoDescender=-DESC, sTypoLineGap=0,
            usWinAscent=ASC, usWinDescent=DESC, fsType=0)
fb.setupPost()
fb.save(sys.argv[1])
