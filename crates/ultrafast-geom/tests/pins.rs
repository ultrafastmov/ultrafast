//! Pinned font files answer for their family ahead of everything else, and
//! only while pinned.

use std::path::{Path, PathBuf};
use ultrafast_geom::geom::{FontPin, Fonts, measure};
use ultrafast_graph::{Node, Param};

fn text(font: &str) -> Node {
    Node {
        op: "text".into(),
        params: [
            ("text".to_string(), Param::Text("Pinned type".into())),
            ("font".to_string(), Param::Text(font.into())),
            ("size".to_string(), Param::Number(64.0)),
        ]
        .into(),
        ..Default::default()
    }
}

fn advance(fonts: &mut Fonts, font: &str) -> f64 {
    measure("t", &text(font), 0.0, fonts, Path::new(".")).unwrap().advance
}

#[test]
fn pins_answer_for_their_family_until_unpinned() {
    // No downloads in tests: an unknown family is not fetched.
    unsafe { std::env::set_var("ULTRAFAST_OFFLINE", "1") };
    let file: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/assets/fonts/Unbounded-VF.ttf");
    let mut fonts = Fonts::new();
    let by_file = advance(&mut fonts, file.to_str().unwrap());
    // A family nobody has falls back to sans-serif.
    let family = "Ultrafast Pin Test Family";
    let fallback = advance(&mut fonts, family);
    assert!((fallback - by_file).abs() > 1.0, "{fallback} vs {by_file}");
    fonts
        .pin(&[FontPin {
            family: family.into(),
            weight: 400,
            italic: false,
            path: file.clone(),
        }])
        .unwrap();
    assert!((advance(&mut fonts, family) - by_file).abs() < 1e-9);
    // A pinned name beats an installed family of the same name too.
    fonts
        .pin(&[FontPin {
            family: "Geist".into(),
            weight: 400,
            italic: false,
            path: file.clone(),
        }])
        .unwrap();
    assert!((advance(&mut fonts, "Geist") - by_file).abs() < 1e-9);
    assert!(
        (advance(&mut fonts, family) - fallback).abs() < 1e-9,
        "the earlier pin was replaced"
    );
    fonts.unpin();
    assert!((advance(&mut fonts, "Geist") - by_file).abs() > 1.0);
}
