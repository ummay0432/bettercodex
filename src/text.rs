//! Focused string behavior retained from OpenAI Codex commit
//! 1669c2403f793d0230065397dfc25f52b844244e.

/// Convert a markdown-style #L.. location suffix into a terminal-friendly
/// :line[:column][-line[:column]] suffix.
pub(crate) fn normalize_markdown_hash_location_suffix(suffix: &str) -> Option<String> {
    let fragment = suffix.strip_prefix('#')?;
    let (start, end) = match fragment.split_once('-') {
        Some((start, end)) => (start, Some(end)),
        None => (fragment, None),
    };
    let (start_line, start_column) = parse_markdown_hash_location_point(start)?;
    let mut normalized = String::from(":");
    normalized.push_str(start_line);
    if let Some(column) = start_column {
        normalized.push(':');
        normalized.push_str(column);
    }
    if let Some(end) = end {
        let (end_line, end_column) = parse_markdown_hash_location_point(end)?;
        normalized.push('-');
        normalized.push_str(end_line);
        if let Some(column) = end_column {
            normalized.push(':');
            normalized.push_str(column);
        }
    }
    Some(normalized)
}

fn parse_markdown_hash_location_point(point: &str) -> Option<(&str, Option<&str>)> {
    let point = point.strip_prefix('L')?;
    match point.split_once('C') {
        Some((line, column)) => Some((line, Some(column))),
        None => Some((point, None)),
    }
}

pub(crate) fn escape_xml(value: &str) -> String {
    escape_xml_with(value, true)
}

pub(crate) fn escape_xml_text(value: &str) -> String {
    escape_xml_with(value, false)
}

fn escape_xml_with(value: &str, escape_quotes: bool) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' if escape_quotes => escaped.push_str("&quot;"),
            '\'' if escape_quotes => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

pub(crate) fn escape_cdata(value: &str) -> String {
    value.replace("]]>", "]]]]><![CDATA[>")
}

#[cfg(test)]
mod tests {
    use super::escape_cdata;
    use super::escape_xml;
    use super::escape_xml_text;
    use super::normalize_markdown_hash_location_suffix;

    #[test]
    fn converts_single_location() {
        assert_eq!(
            normalize_markdown_hash_location_suffix("#L74C3"),
            Some(":74:3".to_string())
        );
    }

    #[test]
    fn converts_ranges() {
        assert_eq!(
            normalize_markdown_hash_location_suffix("#L74C3-L76C9"),
            Some(":74:3-76:9".to_string())
        );
    }

    #[test]
    fn rejects_non_markdown_locations() {
        assert_eq!(normalize_markdown_hash_location_suffix(":74:3"), None);
        assert_eq!(normalize_markdown_hash_location_suffix("#74"), None);
    }

    #[test]
    fn escapes_xml_attributes_and_text() {
        assert_eq!(
            escape_xml("<&>\"'"),
            "&lt;&amp;&gt;&quot;&apos;"
        );
        assert_eq!(escape_xml_text("<&>\"'"), "&lt;&amp;&gt;\"'");
    }

    #[test]
    fn splits_cdata_terminators() {
        assert_eq!(escape_cdata("left]]>right"), "left]]]]><![CDATA[>right");
    }
}
