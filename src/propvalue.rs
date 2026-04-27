//! Encoding for property value blobs.
//!
//! Each value is prefixed with `size: N\n` and followed by exactly `N`
//! bytes plus a trailing `\n` separator. Length-prefix means values may
//! contain any bytes (including newlines) without escaping.
//!
//! Reading is forgiving: blobs that don't start with `size: ` are decoded
//! using the legacy line-split format (one value per non-empty line). New
//! writes always use the size-prefix format, so legacy blobs migrate
//! automatically on the next save.

/// Serialize a list of values as length-prefixed blocks.
pub fn encode(values: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        out.extend_from_slice(format!("size: {}\n", v.len()).as_bytes());
        out.extend_from_slice(v.as_bytes());
        out.push(b'\n');
    }
    out
}

/// Parse a property value blob.
pub fn decode(bytes: &[u8]) -> Vec<String> {
    if bytes.starts_with(b"size: ") {
        return decode_size_prefixed(bytes);
    }
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn decode_size_prefixed(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = std::str::from_utf8(&rest[..nl]).unwrap_or("");
        let Some(size_str) = header.strip_prefix("size: ") else {
            break;
        };
        let Ok(size) = size_str.parse::<usize>() else {
            break;
        };
        rest = &rest[nl + 1..];
        if rest.len() < size + 1 {
            break;
        }
        let value = String::from_utf8_lossy(&rest[..size]).into_owned();
        out.push(value);
        rest = &rest[size + 1..];
    }
    out
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn round_trip_simple_values() {
        let values = vec!["high".to_string(), "alpha".to_string()];
        let bytes = encode(&values);
        assert_eq!(decode(&bytes), values);
    }

    #[test]
    fn round_trip_with_embedded_newline() {
        let values = vec!["line1\nline2\n\nline4".to_string(), "next".to_string()];
        let bytes = encode(&values);
        assert_eq!(decode(&bytes), values);
    }

    #[test]
    fn round_trip_empty_string_value() {
        let values = vec!["".to_string(), "non-empty".to_string()];
        let bytes = encode(&values);
        assert_eq!(decode(&bytes), values);
    }

    #[test]
    fn empty_list_encodes_to_empty_blob() {
        assert_eq!(encode(&[]), Vec::<u8>::new());
        assert!(decode(&[]).is_empty());
    }

    #[test]
    fn legacy_line_split_format_still_decodes() {
        let legacy = b"high\nalpha\nbeta\n";
        assert_eq!(
            decode(legacy),
            vec!["high".to_string(), "alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn legacy_format_skips_blank_lines() {
        let legacy = b"high\n\nalpha\n";
        assert_eq!(
            decode(legacy),
            vec!["high".to_string(), "alpha".to_string()]
        );
    }
}
