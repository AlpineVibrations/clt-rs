//! Portable, task-local launch settings embedded in task prose.
use std::ops::Range;

pub(crate) const LEVELS: [&str; 7] = ["", "low", "medium", "high", "xhigh", "max", "ultra"];

/// Recognize complete whitespace-delimited directives anywhere in the task.
/// Unknown values and quoted examples remain prose. The last directive wins.
fn directives(content: &str) -> Vec<(Range<usize>, Option<&'static str>)> {
    let mut result = Vec::new();
    let mut offset = 0;
    let mut tokens = content.split_whitespace();
    let mut in_comment = false;
    while let Some(token) = tokens.next() {
        let start = offset + content[offset..].find(token).unwrap();
        offset = start + token.len();
        if token.starts_with("<!--") {
            in_comment = true;
        }
        if in_comment {
            if token.ends_with("-->") {
                in_comment = false;
            }
            continue;
        }
        if token != "/think" {
            continue;
        }
        let Some(value) = tokens.clone().next() else {
            continue;
        };
        let level = if value.eq_ignore_ascii_case("default") {
            None
        } else if let Some(level) = LEVELS[1..]
            .iter()
            .find(|level| value.eq_ignore_ascii_case(level))
        {
            Some(*level)
        } else {
            continue;
        };
        tokens.next();
        offset += content[offset..].find(value).unwrap() + value.len();
        result.push((start..offset, level));
    }
    result
}

pub(crate) fn read(content: &str) -> Option<&'static str> {
    directives(content).last().and_then(|(_, level)| *level)
}

pub(crate) fn strip(content: &str) -> String {
    let mut result = content.to_string();
    for (span, _) in directives(content).into_iter().rev() {
        // Remove adjacent horizontal whitespace without joining paragraphs.
        let end = span.end
            + result[span.end..]
                .len()
                .saturating_sub(result[span.end..].trim_start_matches([' ', '\t']).len());
        let start = if end == span.end {
            result[..span.start].trim_end_matches([' ', '\t']).len()
        } else {
            span.start
        };
        result.replace_range(start..end, "");
    }
    result.trim().to_string()
}

/// The editor supplies prose without hidden ownership/coordination markers.
pub(crate) fn cycle(content: &str) -> (String, Option<&'static str>) {
    let current = read(content).unwrap_or("");
    let index = LEVELS
        .iter()
        .position(|level| *level == current)
        .unwrap_or(0);
    let next = LEVELS[(index + 1) % LEVELS.len()];
    let mut body = strip(content);
    if next.is_empty() {
        return (body, None);
    }
    // Keep /goal at the start and retain all detail paragraphs as written.
    let first_line_end = body.find('\n').unwrap_or(body.len());
    body.insert_str(first_line_end, &format!(" /think {next}"));
    (body, Some(next))
}

pub(crate) fn label(content: &str, text: String) -> String {
    match read(content) {
        Some(level) => format!("[think: {level}] {text}"),
        None => text,
    }
}

#[cfg(test)]
mod tests;
