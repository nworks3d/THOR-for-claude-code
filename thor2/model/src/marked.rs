//! The shape of an `item_marked_useful` event - the other half of R3's
//! observability doctrine, extended for decay: "this item actually helped"
//! needs to be an event in the log for the same reason `ItemServed` is, so
//! "was this item EVER marked useful" is a query over the log
//! (`serve::usefulness::ever_marked_useful`), never a sidecar flag that can
//! fall out of sync with it.
//!
//! Wired by crate `serve` (`serve::mark::record_useful`). This module stays
//! the single definition of the body shape, exactly like `served` does for
//! `ItemServed`.

use serde::{Deserialize, Serialize};

/// The event kind name this body is meant for
/// (`thor_core::event_store::EventKind::ItemMarkedUseful::as_str()`).
pub const ITEM_MARKED_USEFUL_KIND: &str = "item_marked_useful";

/// The body of an `item_marked_useful` event. Same shape as `ItemServed` for
/// the same reason: the log's own `entity_id` already says WHICH item, so
/// nothing else needs to be carried. Decay only ever asks "was this item
/// EVER marked useful" (existence, not a count) - `marked_at` is kept purely
/// as an audit trail for a human reading the raw log, mirroring `ItemServed`'s
/// own `served_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemMarkedUseful {
    /// ISO-8601 timestamp of the moment this item was marked useful.
    pub marked_at: String,
    /// The place this verdict is about (`place_label`: a path, a directory, a
    /// command, a tool or a moment the item is bound to). `None` for a verdict
    /// with no place (every verdict written before this field existed): it
    /// clears the noise of every place, as it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
}

/// The event kind name a noise judgement's body is meant for
/// (`thor_core::event_store::EventKind::ItemMarkedNoise::as_str()`).
pub const ITEM_MARKED_NOISE_KIND: &str = "item_marked_noise";

/// The body of an `item_marked_noise` event: the reader saying "this did not
/// belong where it fired".
///
/// Unlike `ItemMarkedUseful`, decay asks for a COUNT of these, not their
/// existence - one stray judgement should not retire a rule, a repeated one
/// should. The log's own event count answers that, so this body still carries
/// nothing but its timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemMarkedNoise {
    /// ISO-8601 timestamp of the moment this item was called noise.
    pub marked_at: String,
    /// The place this verdict is about (`place_label`). Two of these at the
    /// SAME place silence the item at that place only; a verdict with no place
    /// (every one written before this field existed) counts for everywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
}

/// The label of one place an item is bound to, the same text at the moment a
/// verdict is given and at the moment the item is served: `<kind>:<value>`,
/// lower case, slashes forward (`path:src/lib.rs`, `command:git push`,
/// `moment:push`). One definition, so a verdict about a place and a serving at
/// that place can never disagree about its spelling.
pub fn place_label(kind: &str, value: &str) -> String {
    format!("{}:{}", kind.to_lowercase(), place_value(value))
}

/// A place's value as it is compared: slashes forward, no trailing slash, and lower
/// case on Windows only (a path is case-sensitive on Linux, so `Src/A.rs` and
/// `src/a.rs` are two places there).
pub fn place_value(value: &str) -> String {
    let value = value.trim().replace('\\', "/");
    let value = if cfg!(windows) { value.to_lowercase() } else { value };
    value.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_label_is_spelled_the_same_whatever_the_slashes_or_case() {
        assert_eq!(place_label("Path", "src\\lib.rs"), "path:src/lib.rs");
        // Lower case on Windows only: a path is case-sensitive elsewhere.
        let upper = place_label("Path", "Src/Lib.rs");
        assert_eq!(upper, if cfg!(windows) { "path:src/lib.rs" } else { "path:Src/Lib.rs" });
        assert_eq!(place_label("dir", "site/"), "dir:site");
        assert_eq!(place_label("Command", " git push "), "command:git push");
    }

    /// A verdict written before the place existed still reads, and one with no
    /// place writes exactly the bytes it always did.
    #[test]
    fn a_verdict_body_without_a_place_reads_and_writes_as_before() {
        let old = r#"{"marked_at":"2026-08-02T12:00:00Z"}"#;
        let noise: ItemMarkedNoise = serde_json::from_str(old).unwrap();
        assert_eq!(noise.place, None);
        assert_eq!(serde_json::to_string(&noise).unwrap(), old);
        let placed = ItemMarkedNoise { marked_at: "2026-08-02T12:00:00Z".into(), place: Some("path:a".into()) };
        let back: ItemMarkedNoise = serde_json::from_str(&serde_json::to_string(&placed).unwrap()).unwrap();
        assert_eq!(back, placed);
    }

    #[test]
    fn item_marked_useful_round_trips() {
        let body = ItemMarkedUseful { marked_at: "2026-08-02T12:00:00Z".to_string(), place: None };
        let json = serde_json::to_string(&body).unwrap();
        let back: ItemMarkedUseful = serde_json::from_str(&json).unwrap();
        assert_eq!(body, back);
    }
}
