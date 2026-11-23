"""Extract unique KaTeX formulas (TeX + MathML, with occurrence counts) from a corpus.

    python3 -I tools/extract_formulas.py reference/corpus.jsonl reference/formulas.json
"""
import html, json, re, sys

pat = re.compile(r'<span class="katex(-display)?">(?:<span class="katex">)?<span class="katex-mathml">(<math.*?</math>)</span>', re.S)
seen, out = {}, []
for line in open(sys.argv[1]):
    for m in pat.finditer(json.loads(line)["content"]):
        mathml = m.group(2)
        t = re.search(r'<annotation encoding="application/x-tex">(.*?)</annotation>', mathml, re.S)
        key = (bool(m.group(1)), html.unescape(t.group(1)) if t else "")
        if key in seen:
            seen[key]["count"] += 1
            continue
        seen[key] = d = {"display": key[0], "tex": key[1], "mathml": mathml, "count": 1}
        out.append(d)
json.dump(out, open(sys.argv[2], "w"))
print(f"{len(out)} unique, {sum(d['count'] for d in out)} total", file=sys.stderr)
