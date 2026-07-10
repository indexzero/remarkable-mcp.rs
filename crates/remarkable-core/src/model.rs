//! The reMarkable document model.
//!
//! reMarkable's cloud sync API exposes a **flat** list of items, each carrying a
//! `parent` UUID (empty for root, the literal `"trash"` for deleted). The hierarchy
//! is reconstructed client-side — see [`crate::library::Library`].
//!
//! Field names and the `DocumentType` / `CollectionType` discriminants are taken
//! from the metadata JSON observed identically across all three reference servers
//! (lanej `blobMetadata`, SamMorrowDrums and wavyrai `Document`).

use serde::{Deserialize, Serialize};

/// Whether an item is a document (notebook / PDF / ePub) or a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemType {
    /// A notebook, PDF, or ePub — `"DocumentType"` in the cloud metadata.
    Document,
    /// A folder — `"CollectionType"` in the cloud metadata.
    Folder,
}

impl ItemType {
    /// Parse the reMarkable `type` discriminant string.
    pub fn from_cloud(s: &str) -> Self {
        match s {
            "CollectionType" => ItemType::Folder,
            _ => ItemType::Document,
        }
    }

    /// The emoji used when rendering this item in an ASCII tree (lanej convention).
    pub fn icon(self) -> &'static str {
        match self {
            ItemType::Folder => "📁",
            ItemType::Document => "📄",
        }
    }
}

/// Raw metadata as stored in a document's `.metadata` blob on the reMarkable cloud.
///
/// Only the fields the metadata layer needs are modeled; unknown fields are ignored.
/// The misspelling `visibleName` is reMarkable's, not ours.
#[derive(Debug, Clone, Deserialize)]
pub struct RawMetadata {
    #[serde(rename = "visibleName", default)]
    pub visible_name: String,
    #[serde(rename = "type", default)]
    pub type_: String,
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub pinned: bool,
    /// Milliseconds-since-epoch as a string, when present.
    #[serde(rename = "lastModified", default)]
    pub last_modified: Option<String>,
}

/// A resolved document or folder in the user's reMarkable library.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    /// The item's UUID.
    pub id: String,
    /// User-visible name.
    pub name: String,
    /// Document or folder.
    pub kind: ItemType,
    /// Parent folder UUID, or `None` for items at the library root.
    pub parent: Option<String>,
    /// Monotonic version counter (used as the base for metadata updates).
    pub version: i64,
    /// Whether the item is bookmarked / pinned.
    pub pinned: bool,
    /// Last-modified time, parsed from the cloud's millisecond timestamp when valid.
    pub last_modified: Option<chrono::DateTime<chrono::Utc>>,
}

impl Item {
    /// `true` if this item is a folder.
    pub fn is_folder(&self) -> bool {
        self.kind == ItemType::Folder
    }

    /// Build an [`Item`] from raw cloud metadata and the item's UUID.
    ///
    /// Returns `None` for items parented to `"trash"` so callers can filter trashed
    /// items in one place (matching lanej / wavyrai behavior).
    pub fn from_metadata(id: String, meta: RawMetadata) -> Option<Self> {
        if meta.parent == "trash" {
            return None;
        }
        let parent = if meta.parent.is_empty() {
            None
        } else {
            Some(meta.parent)
        };
        let last_modified = meta
            .last_modified
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis);
        Some(Item {
            id,
            name: meta.visible_name,
            kind: ItemType::from_cloud(&meta.type_),
            parent,
            version: meta.version,
            pinned: meta.pinned,
            last_modified,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> RawMetadata {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn parses_a_document_at_root() {
        let item = Item::from_metadata(
            "id-1".into(),
            raw(r#"{"visibleName":"Notes","type":"DocumentType","parent":"","version":3}"#),
        )
        .unwrap();
        assert_eq!(item.name, "Notes");
        assert_eq!(item.kind, ItemType::Document);
        assert_eq!(item.parent, None);
        assert_eq!(item.version, 3);
        assert!(!item.is_folder());
    }

    #[test]
    fn parses_a_folder_with_parent() {
        let item = Item::from_metadata(
            "id-2".into(),
            raw(r#"{"visibleName":"Work","type":"CollectionType","parent":"id-1"}"#),
        )
        .unwrap();
        assert!(item.is_folder());
        assert_eq!(item.parent.as_deref(), Some("id-1"));
    }

    #[test]
    fn trashed_items_are_filtered() {
        let item = Item::from_metadata(
            "id-3".into(),
            raw(r#"{"visibleName":"Gone","type":"DocumentType","parent":"trash"}"#),
        );
        assert!(item.is_none());
    }

    #[test]
    fn parses_millisecond_timestamp() {
        let item = Item::from_metadata(
            "id-4".into(),
            raw(r#"{"visibleName":"T","type":"DocumentType","parent":"","lastModified":"1700000000000"}"#),
        )
        .unwrap();
        assert!(item.last_modified.is_some());
    }

    #[test]
    fn tolerates_unknown_and_missing_fields() {
        // Extra fields ignored; missing fields default.
        let item = Item::from_metadata(
            "id-5".into(),
            raw(r#"{"visibleName":"X","type":"DocumentType","parent":"","futureField":42}"#),
        )
        .unwrap();
        assert_eq!(item.name, "X");
        assert!(!item.pinned);
    }
}
