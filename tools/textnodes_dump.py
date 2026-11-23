"""Dump Overlay's textNodes() text for every message in a corpus, as parsed by real Chrome.

Each message's server HTML is assigned with innerHTML (as Zulip web does), then walked with
the exact textNodes() from overlay.js. Prints JSON {message_id: text} to stdout.

    python3 -I tools/textnodes_dump.py reference/corpus.jsonl > reference/chrome_textnodes.json
"""
import base64, json, os, subprocess, sys, tempfile

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
corpus = [json.loads(l) for l in open(sys.argv[1])]
payload = json.dumps([[m["id"], m["content"]] for m in corpus]).replace("</", "<\\/")

# textNodes() copied verbatim from overlay.js (the `node` field dropped).
page = """<!doctype html><meta charset=utf-8><body><script>
const corpus = %s;
function textNodes($content) {
  const nodes = []; let offset = 0;
  const add = (node, text, atomic) => { nodes.push({start: offset, length: text.length, text, atomic}); offset += text.length; };
  const walk = (node) => {
    if (node.nodeType === Node.TEXT_NODE) { add(node, node.data, false); return; }
    if (node.nodeType !== Node.ELEMENT_NODE || "zo" in node.dataset) { return; }
    if (node.classList.contains("katex")) {
      const tex = node.querySelector('annotation[encoding="application/x-tex"]')?.textContent ?? node.textContent;
      add(node, tex, true); return;
    }
    for (const child of node.childNodes) { walk(child); }
  };
  walk($content);
  return {nodes, text: nodes.map(({text}) => text).join("")};
}
const out = {};
for (const [id, html] of corpus) {
  const div = document.createElement("div");
  div.className = "message_content rendered_markdown";
  div.innerHTML = html;
  out[id] = textNodes(div).text;
}
const bytes = new TextEncoder().encode(JSON.stringify(out));
let bin = ""; for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
document.body.textContent = "BEGIN" + btoa(bin) + "END";
</script>""" % payload

with tempfile.TemporaryDirectory() as tmp:
    path = os.path.join(tmp, "dump.html")
    open(path, "w").write(page)
    dom = subprocess.run(
        [CHROME, "--headless=new", f"--user-data-dir={tmp}/profile", "--disable-gpu",
         "--virtual-time-budget=60000", "--dump-dom", f"file://{path}"],
        capture_output=True, text=True, check=True).stdout
b64 = dom[dom.index("BEGIN") + 5: dom.index("END")]
sys.stdout.write(base64.b64decode(b64).decode())
