//! Shared diff computation utilities for the edit tool (port of pi's `edit-diff.ts`).

use anyhow::{anyhow, Result};
use similar::{ChangeTag, DiffOp, TextDiff};

pub fn detect_line_ending(content: &str) -> &'static str {
    let lf_idx = match content.find('\n') {
        Some(i) => i,
        None => return "\n",
    };
    match content.find("\r\n") {
        Some(crlf_idx) if crlf_idx < lf_idx => "\r\n",
        _ => "\n",
    }
}

pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// Split a leading UTF-8 byte order mark from decoded text.
pub fn split_bom(content: &str) -> (&str, &str) {
    match content.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}", rest),
        None => ("", content),
    }
}

/// Normalize text for fuzzy matching:
/// - Strip trailing whitespace from each line
/// - Normalize smart quotes to ASCII equivalents
/// - Normalize Unicode dashes/hyphens to ASCII hyphen
/// - Normalize special Unicode spaces to regular space
///
/// (pi additionally applies NFKC normalization; that is not ported.)
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let trimmed = text.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n");
    trimmed
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Split content into lines, each retaining its trailing `\n` (if any).
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            lines.push(&content[start..=i]);
            start = i + 1;
        }
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

#[derive(Debug, Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
pub struct TextReplacement {
    pub match_index: usize,
    pub match_length: usize,
    pub new_text: String,
}

#[derive(Debug, Clone)]
struct MatchedEdit {
    edit_index: usize,
    replacement: TextReplacement,
}

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan { start: offset, end: offset + line.len() };
            offset = span.end;
            span
        })
        .collect()
}

fn get_replacement_line_range(lines: &[LineSpan], replacement: &TextReplacement) -> Result<(usize, usize)> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let start_line = lines
        .iter()
        .position(|line| replacement_start >= line.start && replacement_start < line.end)
        .ok_or_else(|| anyhow!("Replacement range is outside the base content."))?;

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err(anyhow!("Replacement range is outside the base content."));
    }
    Ok((start_line, end_line + 1))
}

fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        let tail = result[match_index + replacement.match_length..].to_string();
        result.truncate(match_index);
        result.push_str(&replacement.new_text);
        result.push_str(&tail);
    }
    result
}

/// Apply replacements matched against `base_content` to `original_content`
/// while preserving unchanged line blocks from the original.
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(anyhow!("Cannot preserve unchanged lines because the base content has a different line count."));
    }

    struct Group {
        start_line: usize,
        end_line: usize,
        replacements: Vec<TextReplacement>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut sorted: Vec<&TextReplacement> = replacements.iter().collect();
    sorted.sort_by_key(|r| r.match_index);
    for replacement in sorted {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, replacement)?;
        if let Some(current) = groups.last_mut() {
            if start_line < current.end_line {
                current.end_line = current.end_line.max(end_line);
                current.replacements.push(replacement.clone());
                continue;
            }
        }
        groups.push(Group { start_line, end_line, replacements: vec![replacement.clone()] });
    }

    let mut original_line_index = 0;
    let mut result = String::new();
    for group in groups {
        result.push_str(&original_lines[original_line_index..group.start_line].concat());
        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        result.push_str(&apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group.replacements,
            group_start_offset,
        ));
        original_line_index = group.end_line;
    }
    result.push_str(&original_lines[original_line_index..].concat());
    Ok(result)
}

#[derive(Debug, Clone)]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone)]
pub struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
}

#[derive(Debug, Clone, Copy)]
pub struct FuzzyMatch {
    /// Byte index where the match starts (in exact or fuzzy-normalized content).
    pub index: usize,
    pub match_length: usize,
    /// Whether fuzzy matching was used (false = exact match).
    pub used_fuzzy_match: bool,
}

/// Find `old_text` in `content`, trying an exact match first, then a fuzzy match.
/// A fuzzy match returns offsets in fuzzy-normalized space.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> Option<FuzzyMatch> {
    if let Some(index) = content.find(old_text) {
        return Some(FuzzyMatch { index, match_length: old_text.len(), used_fuzzy_match: false });
    }
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    let index = fuzzy_content.find(&fuzzy_old)?;
    Some(FuzzyMatch { index, match_length: fuzzy_old.len(), used_fuzzy_match: true })
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    if fuzzy_old.is_empty() {
        return 0;
    }
    fuzzy_content.matches(fuzzy_old.as_str()).count()
}

fn not_found_error(path: &str, edit_index: usize, total: usize) -> anyhow::Error {
    if total == 1 {
        anyhow!("Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines.")
    } else {
        anyhow!("Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines.")
    }
}

fn duplicate_error(path: &str, edit_index: usize, total: usize, occurrences: usize) -> anyhow::Error {
    if total == 1 {
        anyhow!("Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.")
    } else {
        anyhow!("Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique.")
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total: usize) -> anyhow::Error {
    if total == 1 {
        anyhow!("oldText must not be empty in {path}.")
    } else {
        anyhow!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn no_change_error(path: &str, total: usize) -> anyhow::Error {
    if total == 1 {
        anyhow!("No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.")
    } else {
        anyhow!("No changes made to {path}. The replacements produced identical content.")
    }
}

/// Apply one or more exact-text replacements to LF-normalized content.
///
/// All edits are matched against the same original content, then applied in
/// reverse offset order so offsets stay stable. If any edit needs fuzzy
/// matching, replacements are computed in fuzzy-normalized space and overlaid
/// onto the original so unchanged lines keep their original bytes.
pub fn apply_edits_to_normalized_content(normalized_content: &str, edits: &[Edit], path: &str) -> Result<AppliedEditsResult> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|e| Edit { old_text: normalize_to_lf(&e.old_text), new_text: normalize_to_lf(&e.new_text) })
        .collect();
    let total = normalized_edits.len();

    for (i, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(empty_old_text_error(path, i, total));
        }
    }

    let used_fuzzy_match = normalized_edits
        .iter()
        .any(|e| fuzzy_find_text(normalized_content, &e.old_text).is_some_and(|m| m.used_fuzzy_match));
    let replacement_base: String =
        if used_fuzzy_match { normalize_for_fuzzy_match(normalized_content) } else { normalized_content.to_string() };

    let mut matched: Vec<MatchedEdit> = Vec::with_capacity(total);
    for (i, edit) in normalized_edits.iter().enumerate() {
        let m = fuzzy_find_text(&replacement_base, &edit.old_text).ok_or_else(|| not_found_error(path, i, total))?;
        let occurrences = count_occurrences(&replacement_base, &edit.old_text);
        if occurrences > 1 {
            return Err(duplicate_error(path, i, total, occurrences));
        }
        matched.push(MatchedEdit {
            edit_index: i,
            replacement: TextReplacement { match_index: m.index, match_length: m.match_length, new_text: edit.new_text.clone() },
        });
    }

    matched.sort_by_key(|m| m.replacement.match_index);
    for pair in matched.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);
        if previous.replacement.match_index + previous.replacement.match_length > current.replacement.match_index {
            return Err(anyhow!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index,
                current.edit_index
            ));
        }
    }

    let replacements: Vec<TextReplacement> = matched.into_iter().map(|m| m.replacement).collect();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(normalized_content, &replacement_base, &replacements)?
    } else {
        apply_replacements(&replacement_base, &replacements, 0)
    };

    if normalized_content == new_content {
        return Err(no_change_error(path, total));
    }

    Ok(AppliedEditsResult { base_content: normalized_content.to_string(), new_content })
}

/// Generate a standard unified patch.
pub fn generate_unified_patch(path: &str, old_content: &str, new_content: &str, context_lines: usize) -> String {
    TextDiff::from_lines(old_content, new_content)
        .unified_diff()
        .context_radius(context_lines)
        .header(path, path)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffString {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

struct DiffPart {
    tag: ChangeTag,
    lines: Vec<String>,
}

fn strip_newline(line: &str) -> String {
    line.strip_suffix('\n').unwrap_or(line).to_string()
}

/// Group a line diff into contiguous equal / removed / added parts (the shape
/// `diff.diffLines` produces).
fn diff_parts(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let diff = TextDiff::from_lines(old_content, new_content);
    let old_slices = diff.old_slices();
    let new_slices = diff.new_slices();
    let mut parts: Vec<DiffPart> = Vec::new();
    let mut push = |tag: ChangeTag, lines: Vec<String>| {
        if lines.is_empty() {
            return;
        }
        match parts.last_mut() {
            Some(last) if last.tag == tag => last.lines.extend(lines),
            _ => parts.push(DiffPart { tag, lines }),
        }
    };
    let take = |slices: &[&str], start: usize, len: usize| -> Vec<String> {
        slices[start..start + len].iter().map(|l| strip_newline(l)).collect()
    };
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { old_index, len, .. } => push(ChangeTag::Equal, take(old_slices, old_index, len)),
            DiffOp::Delete { old_index, old_len, .. } => push(ChangeTag::Delete, take(old_slices, old_index, old_len)),
            DiffOp::Insert { new_index, new_len, .. } => push(ChangeTag::Insert, take(new_slices, new_index, new_len)),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                push(ChangeTag::Delete, take(old_slices, old_index, old_len));
                push(ChangeTag::Insert, take(new_slices, new_index, new_len));
            }
        }
    }
    parts
}

/// Generate a display-oriented diff string with line numbers and context.
/// Returns both the diff string and the first changed line number (in the new file).
pub fn generate_diff_string(old_content: &str, new_content: &str, context_lines: usize) -> DiffString {
    let parts = diff_parts(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let max_line_num = old_content.split('\n').count().max(new_content.split('\n').count());
    let width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    let fmt_num = |n: usize| format!("{n:>width$}");
    let blank = " ".repeat(width);

    for i in 0..parts.len() {
        let part = &parts[i];
        let raw = &part.lines;
        match part.tag {
            ChangeTag::Insert | ChangeTag::Delete => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }
                for line in raw {
                    if part.tag == ChangeTag::Insert {
                        output.push(format!("+{} {line}", fmt_num(new_line_num)));
                        new_line_num += 1;
                    } else {
                        output.push(format!("-{} {line}", fmt_num(old_line_num)));
                        old_line_num += 1;
                    }
                }
                last_was_change = true;
            }
            ChangeTag::Equal => {
                let next_is_change = parts.get(i + 1).is_some_and(|p| p.tag != ChangeTag::Equal);
                let has_leading = last_was_change;
                let has_trailing = next_is_change;

                // Decide which context lines to show: (leading shown, skipped, trailing shown).
                let (leading, skipped, trailing): (usize, usize, usize) = if has_leading && has_trailing {
                    if raw.len() <= context_lines * 2 {
                        (raw.len(), 0, 0)
                    } else {
                        (context_lines, raw.len() - 2 * context_lines, context_lines)
                    }
                } else if has_leading {
                    let shown = raw.len().min(context_lines);
                    (shown, raw.len() - shown, 0)
                } else if has_trailing {
                    let skipped = raw.len().saturating_sub(context_lines);
                    (0, skipped, raw.len() - skipped)
                } else {
                    (0, raw.len(), 0)
                };
                let show_marker = skipped > 0 && (has_leading || has_trailing);

                for line in &raw[..leading] {
                    output.push(format!(" {} {line}", fmt_num(old_line_num)));
                    old_line_num += 1;
                    new_line_num += 1;
                }
                if show_marker {
                    output.push(format!(" {blank} ..."));
                }
                old_line_num += skipped;
                new_line_num += skipped;
                for line in &raw[raw.len() - trailing..] {
                    output.push(format!(" {} {line}", fmt_num(old_line_num)));
                    old_line_num += 1;
                    new_line_num += 1;
                }
                last_was_change = false;
            }
        }
    }

    DiffString { diff: output.join("\n"), first_changed_line }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Edit {
        Edit { old_text: old.into(), new_text: new.into() }
    }

    #[test]
    fn line_ending_helpers() {
        assert_eq!(detect_line_ending("a\r\nb"), "\r\n");
        assert_eq!(detect_line_ending("a\nb\r\n"), "\n");
        assert_eq!(detect_line_ending("abc"), "\n");
        assert_eq!(normalize_to_lf("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(restore_line_endings("a\nb", "\r\n"), "a\r\nb");
        assert_eq!(split_bom("\u{FEFF}x"), ("\u{FEFF}", "x"));
    }

    #[test]
    fn applies_multiple_edits_against_original() {
        let content = "alpha\nbeta\ngamma\n";
        let r = apply_edits_to_normalized_content(content, &[edit("gamma", "GAMMA"), edit("alpha", "ALPHA")], "f").expect("ok");
        assert_eq!(r.new_content, "ALPHA\nbeta\nGAMMA\n");
    }

    #[test]
    fn rejects_non_unique_and_overlapping() {
        let content = "x = 1\nx = 1\n";
        let err = apply_edits_to_normalized_content(content, &[edit("x = 1", "y")], "f").expect_err("dup");
        assert_eq!(err.to_string(), "Found 2 occurrences of the text in f. The text must be unique. Please provide more context to make it unique.");

        let content = "hello world\n";
        let err = apply_edits_to_normalized_content(content, &[edit("hello wo", "a"), edit("world", "b")], "f").expect_err("overlap");
        assert_eq!(err.to_string(), "edits[0] and edits[1] overlap in f. Merge them into one edit or target disjoint regions.");

        let err = apply_edits_to_normalized_content(content, &[edit("nope", "a")], "f").expect_err("missing");
        assert!(err.to_string().starts_with("Could not find the exact text in f."));
        let err = apply_edits_to_normalized_content(content, &[edit("nope", "a"), edit("x", "y")], "f").expect_err("missing");
        assert!(err.to_string().starts_with("Could not find edits[0] in f."));
        let err = apply_edits_to_normalized_content(content, &[edit("", "a")], "f").expect_err("empty");
        assert_eq!(err.to_string(), "oldText must not be empty in f.");
        let err = apply_edits_to_normalized_content(content, &[edit("hello", "hello")], "f").expect_err("nochange");
        assert!(err.to_string().starts_with("No changes made to f."));
    }

    #[test]
    fn fuzzy_match_preserves_unchanged_lines() {
        // Trailing whitespace on line 1 and a smart quote on line 2; edit line 2 only.
        let content = "keep   \nsay \u{2018}hi\u{2019}\nend\n";
        let r = apply_edits_to_normalized_content(content, &[edit("say 'hi'", "say 'bye'")], "f").expect("ok");
        assert_eq!(r.new_content, "keep   \nsay 'bye'\nend\n");
    }

    #[test]
    fn diff_string_shows_context_and_first_changed_line() {
        let old: String = (1..=12).map(|i| format!("line{i}\n")).collect();
        let new = old.replace("line7\n", "LINE7\n");
        let d = generate_diff_string(&old, &new, 4);
        assert_eq!(d.first_changed_line, Some(7));
        let expected = "    ...\n  3 line3\n  4 line4\n  5 line5\n  6 line6\n- 7 line7\n+ 7 LINE7\n  8 line8\n  9 line9\n 10 line10\n 11 line11\n    ...";
        assert_eq!(d.diff, expected);
    }

    #[test]
    fn unified_patch_has_headers() {
        let patch = generate_unified_patch("f.txt", "a\nb\n", "a\nc\n", 4);
        assert!(patch.starts_with("--- f.txt\n+++ f.txt\n@@"), "{patch}");
        assert!(patch.contains("-b\n+c\n"));
    }
}
