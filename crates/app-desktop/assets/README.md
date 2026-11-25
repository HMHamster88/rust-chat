# Material Design Icons (subset)

`MaterialDesignIcons-subset.ttf` is a **subset** of Pictogrammers' Material Design Icons webfont
(`Templarian/MaterialDesign-Webfont`, `fonts/materialdesignicons-webfont.ttf`), cut down to the
glyphs the UI uses (4.7 KB instead of 1.3 MB). Family name: **Material Design Icons**.

License: see `MaterialDesignIcons-LICENSE.txt` (Pictogrammers Free License). Keep the notice with
any distribution of the app.

Regenerating the subset (needs `pip install fonttools`):

```bash
py -m fontTools.subset materialdesignicons-webfont.ttf \
  --unicodes=U+F0493,U+F0156,U+F0425,... --no-hinting \
  --output-file=MaterialDesignIcons-subset.ttf
```

The codepoints live in `crates/app-desktop/src/icons.rs`; keep both lists in sync when adding an
icon (`icons.rs` fails to draw, rather than to build, on a missing glyph).
