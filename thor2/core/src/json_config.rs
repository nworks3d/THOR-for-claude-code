//! Reading and writing the JSON files of other programs (Claude Code's
//! settings and tool-server files) without choking on a UTF-8 byte order mark.
//!
//! Some editors, and Windows PowerShell 5.1's `Set-Content -Encoding UTF8`, put
//! a BOM (U+FEFF) at the start of a file. `serde_json` rejects it as "not valid
//! JSON ... line 1 column 1", so every reader strips it, and a writer that
//! rewrites such a file puts it back, so the file keeps the shape its owner
//! gave it.

use std::io;
use std::path::Path;

/// The UTF-8 byte order mark as text.
pub const BOM: char = '\u{feff}';

/// `text` without a leading byte order mark.
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix(BOM).unwrap_or(text)
}

/// Read a JSON file: the text without its BOM, and whether it had one.
pub fn read_json_text(path: &Path) -> io::Result<(String, bool)> {
    let raw = std::fs::read_to_string(path)?;
    let had_bom = raw.starts_with(BOM);
    Ok((strip_bom(&raw).to_string(), had_bom))
}

/// `read_json_text` for a caller that only wants the text.
pub fn read_json_text_only(path: &Path) -> io::Result<String> {
    read_json_text(path).map(|(text, _)| text)
}

/// `body` with a BOM in front when the file it replaces had one.
pub fn with_bom(body: String, had_bom: bool) -> String {
    if had_bom {
        format!("{BOM}{body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bom_is_stripped_and_remembered() {
        let dir = std::env::temp_dir().join(format!("thor-json-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.json");
        std::fs::write(&path, "\u{feff}{\"a\":1}").unwrap();
        let (text, bom) = read_json_text(&path).unwrap();
        assert_eq!(text, "{\"a\":1}");
        assert!(bom);
        assert_eq!(with_bom(text, bom), "\u{feff}{\"a\":1}");
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(read_json_text(&path).unwrap(), ("{}".to_string(), false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
