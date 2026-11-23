// Paste into the DevTools console of a logged-in Zulip web tab (any narrow with messages,
// ideally ones with code blocks, timestamps, channel links and mentions). Read-only: it
// walks the rendered DOM and copies {message_id: textNodes text} to the clipboard.
// Then run `pbpaste > reference/live_textnodes.json` and the parity test compares it.
(() => {
  function textNodes($content) {
    const nodes = []; let offset = 0;
    const add = (node, text) => { nodes.push(text); offset += text.length; };
    const walk = (node) => {
      if (node.nodeType === Node.TEXT_NODE) { add(node, node.data); return; }
      if (node.nodeType !== Node.ELEMENT_NODE || "zo" in node.dataset) { return; }
      if (node.classList.contains("katex")) {
        add(node, node.querySelector('annotation[encoding="application/x-tex"]')?.textContent ?? node.textContent);
        return;
      }
      for (const child of node.childNodes) { walk(child); }
    };
    walk($content);
    return nodes.join("");
  }
  const out = {};
  for (const row of document.querySelectorAll(".message_row[data-message-id], .message_row[zid]")) {
    const id = row.getAttribute("data-message-id") ?? row.getAttribute("zid");
    const content = row.querySelector(".message_content");
    if (id && content) out[id] = textNodes(content);
  }
  copy(JSON.stringify(out));
  console.log(`copied textNodes for ${Object.keys(out).length} messages`);
})();
