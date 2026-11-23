// Prints overlay.js digest() test vectors as JSON: [[utf16_units, hash], ...].
function digest(text) {
  let a = 0x81_1c_9d_c5;
  let b = 0x01_00_01_93;
  for (const character of text) {
    const code = character.codePointAt(0);
    a = Math.imul(a ^ code, 0x01_00_01_93);
    b = Math.imul(b ^ code, 0x81_1c_9d_c5) + 0x9e_37;
  }
  return (a >>> 0).toString(16).padStart(8, "0") + (b >>> 0).toString(16).padStart(8, "0");
}
const cases = ["", "a", "hello world", "naïve café", "∑ x_i", "emoji 😀 pair", "\\frac{a}{b}",
  "😀".slice(0, 1), "x" + "😀".slice(1), "a😀".slice(0, 2) + "z", "\n\n", "日本語のテキスト", "a".repeat(1000)];
console.log(JSON.stringify(cases.map((s) => [Array.from({length: s.length}, (_, i) => s.charCodeAt(i)), digest(s)])));
