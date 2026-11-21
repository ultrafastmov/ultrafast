//! What the server keeps in its storage besides renders: uploads (assets),
//! each a small JSON record beside its data.
//!
//!   assets/ID.json, assets/ID/FILENAME   an upload and its bytes
//!   renders/ID/NAME.mp4                  a stored render (+ .manifest.json)

use crate::api::ApiError;
use crate::store::{Storage, Store, Stores};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An upload, as stored (the public form drops `owner`, `object`,
/// `generation`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssetRecord {
    /// `asset_` + the random part.
    pub id: String,
    pub uri: String,
    pub bytes: u64,
    pub sha256: String,
    pub filename: String,
    pub content_type: String,
    pub created_at: String,
    /// The caller that uploaded it.
    pub owner: String,
    /// Its bytes' object name in the storage, and that object's version.
    pub object: String,
    pub generation: String,
}

impl AssetRecord {
    pub fn public(&self) -> Value {
        serde_json::json!({
            "id": self.id, "uri": self.uri, "bytes": self.bytes, "sha256": self.sha256,
            "filename": self.filename, "content_type": self.content_type, "created_at": self.created_at,
        })
    }
}

pub fn asset_record_name(storage: &Storage, raw_id: &str) -> String {
    storage.name(&format!("assets/{raw_id}.json"))
}

/// An asset by the random part of its id.
pub fn load_asset(stores: &Stores, storage: &Storage, raw_id: &str) -> Result<Option<AssetRecord>, ApiError> {
    if !crate::refs::valid_asset_id(raw_id) {
        return Ok(None);
    }
    let store = stores.main(storage);
    let bytes = crate::store::get_bytes(store, &storage.bucket, &asset_record_name(storage, raw_id), 1 << 20)
        .map_err(|e| store_down(store, &e))?;
    match bytes {
        None => Ok(None),
        Some(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| ApiError::new(500, "render_failed", format!("asset record {raw_id}: {e}"))),
    }
}

/// A store that could not be reached.
pub fn store_down(store: &dyn Store, e: &anyhow::Error) -> ApiError {
    ApiError::new(503, "unavailable", format!("storage ({}): {e:#}", store.scheme()))
}

/// A file name safe to keep as the last segment of an object name.
pub fn clean_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut out: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    out = out.trim_start_matches('.').to_string();
    if out.len() > 120 {
        let ext = crate::refs::extension_of(&out);
        out = format!("{}{ext}", &out[..120 - ext.len().min(120)]);
    }
    if out.is_empty() { "upload".into() } else { out }
}

/// A content type from a file name, for uploads that do not say.
pub fn content_type_for(filename: &str) -> &'static str {
    match crate::refs::extension_of(filename).as_str() {
        ".mp4" | ".m4v" => "video/mp4",
        ".mov" => "video/quicktime",
        ".webm" => "video/webm",
        ".mkv" => "video/x-matroska",
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".webp" => "image/webp",
        ".gif" => "image/gif",
        ".svg" => "image/svg+xml",
        ".mp3" => "audio/mpeg",
        ".wav" => "audio/wav",
        ".m4a" => "audio/mp4",
        ".aac" => "audio/aac",
        ".ogg" | ".opus" => "audio/ogg",
        ".flac" => "audio/flac",
        ".ttf" => "font/ttf",
        ".otf" => "font/otf",
        ".woff" => "font/woff",
        ".woff2" => "font/woff2",
        ".json" => "application/json",
        ".zip" => "application/zip",
        ".srt" => "application/x-subrip",
        ".vtt" => "text/vtt",
        ".cube" => "text/plain",
        ".glb" => "model/gltf-binary",
        ".gltf" => "model/gltf+json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames() {
        assert_eq!(clean_filename("../../etc/passwd"), "passwd");
        assert_eq!(clean_filename("C:\\clips\\My Interview.mp4"), "My_Interview.mp4");
        assert_eq!(clean_filename(".hidden"), "hidden");
        assert_eq!(clean_filename(""), "upload");
        assert_eq!(content_type_for("a.MP4"), "video/mp4");
    }
}
