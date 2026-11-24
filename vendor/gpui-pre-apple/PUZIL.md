# gpui-pre-apple 0.3.12, patched for Puzil

GPUI's macOS renderer from crates.io (Apache-2.0, see `LICENSE-APACHE`), with three changes. The first two together hold two window-sized drawables instead of four: about 34 MB less memory at a
1280×860 window on a 2× display (`tools/bench/run.py`: 167 → 133 MB at idle).

1. **`src/display_link.rs`: frames are paced by `CVDisplayLink`** (GPUI's path for macOS 13 and
   older) instead of `CAMetalDisplayLink`. The Metal display link keeps four drawables in
   flight regardless of the layer's `maximumDrawableCount` or its own `preferredFrameLatency`.
   `PUZIL_METAL_DISPLAY_LINK=1` switches it back on, for comparison. Its advantage is pacing
   on variable-refresh (ProMotion) displays.
2. **`src/metal_renderer.rs`: `maximumDrawableCount` is 2, not 3.** Puzil's frames take a few
   milliseconds against a 16.7 ms budget, so a third buffer only costs memory.

3. **`src/text_system.rs`: the installed font families are listed on a background thread** from
   the moment the text system is created, and the first `all_font_names` call takes that list.
   gpui-component's theme asks for it during `init`, on the main thread, where it cost ~20 ms
   of launch. Later calls list the fonts again, as before.

Applied with `[patch.crates-io]` in the workspace `Cargo.toml`. To update GPUI, copy the new
release over this folder and reapply the changes (search for `Puzil:`).
