//! SVG text is set from the faces the text op uses. Its own test binary:
//! it turns downloads off for the process.

/// Text is set from the faces the text op uses: a generic family maps to an
/// installed face and a bundled family is found by name. A family neither
/// installed nor on Google Fonts falls back with a warning, not silently.
#[test]
fn svg_text_finds_fonts_like_the_text_op() {
    // No downloads in tests.
    unsafe { std::env::set_var("ULTRAFAST_OFFLINE", "1") };
    let text = |family: &str| {
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><text x="10" y="60" font-family="{family}" font-size="40">Hello</text></svg>"##
        )
    };
    for family in ["sans-serif", "Geist", "NoSuchFontXyz"] {
        let imp = ultrafast_render::svg::import(text(family).as_bytes(), None, None).unwrap();
        let paths = imp.component.nodes.values().filter(|n| n.op == "path").count();
        assert!(paths > 0, "{family}: no outlines");
        let warned = imp
            .warnings
            .iter()
            .any(|w| w.contains("isn't on this machine or Google Fonts"));
        assert_eq!(warned, family == "NoSuchFontXyz", "{family}: {:?}", imp.warnings);
    }
}
