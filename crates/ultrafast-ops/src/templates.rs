//! The starter documents `ultrafast new` writes (and the MCP `ultrafast_new`
//! tool): each is a finished-looking film to edit. The files live in
//! `examples/templates/`; `templates/` in this crate links to them so the
//! published crate carries them too.

/// Template names, in the order `ultrafast new` lists them.
pub const NAMES: &[&str] = &["vertical", "title", "square", "promo", "explainer", "product"];

/// The document JSON of template `name`, or `None` for an unknown name.
pub fn get(name: &str) -> Option<&'static str> {
    Some(match name {
        "vertical" => include_str!("../templates/vertical.json"),
        "title" => include_str!("../templates/title.json"),
        "square" => include_str!("../templates/square.json"),
        "promo" => include_str!("../templates/promo.json"),
        "explainer" => include_str!("../templates/explainer.json"),
        "product" => include_str!("../templates/product.json"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_name_has_a_document() {
        for n in super::NAMES {
            let doc = super::get(n).unwrap();
            assert!(serde_json::from_str::<serde_json::Value>(doc).is_ok(), "{n}");
        }
        assert!(super::get("nope").is_none());
    }
}
