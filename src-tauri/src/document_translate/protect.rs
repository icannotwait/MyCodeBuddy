//! Fail-closed Markdown code protection for document translation.
//!
//! Fenced blocks (``` / ~~~) and single-level inline backticks are replaced
//! with nonce-scoped placeholders. Restore is type-aware: fenced `CGCODE`
//! tokens must stay in source order, while intact `CGINLINE` tokens may
//! reorder inside the same fenced-code region. Cross-region inline moves,
//! missing/duplicate/unknown tokens, and malformed current-nonce markers
//! fail closed.

use thiserror::Error;

/// Opening / closing unicode for placeholders (`U+27E6` / `U+27E7`).
const TOKEN_OPEN: char = '⟦';
const TOKEN_CLOSE: char = '⟧';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityFailureKind {
    MalformedMarker,
    UnknownToken,
    MissingToken,
    DuplicateToken,
    FencedReorder,
    InlineCrossRegion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    pub text: String,
    pub inline_reorder_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtectError {
    #[error("nonce appears in source document")]
    NonceCollision,
    #[error("placeholder integrity check failed")]
    IntegrityFailed(IntegrityFailureKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Placeholder {
    token: String,
    original: String,
}

/// Protected Markdown body plus the ordered placeholder table needed to
/// restore originals after the model returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedDocument {
    /// Source with fenced/inline code replaced by placeholders.
    pub text: String,
    /// Nonce embedded in every token for this request.
    pub nonce: String,
    placeholders: Vec<Placeholder>,
}

impl ProtectedDocument {
    /// Ordered placeholder tokens expected in model output.
    pub fn tokens(&self) -> impl Iterator<Item = &str> {
        self.placeholders.iter().map(|p| p.token.as_str())
    }
}

/// Protect Markdown using a caller-supplied nonce (tests / deterministic paths).
///
/// Returns [`ProtectError::NonceCollision`] if `nonce` already appears in
/// `source` (substring check).
pub fn protect_markdown_with_nonce(
    source: &str,
    nonce: &str,
) -> Result<ProtectedDocument, ProtectError> {
    if nonce.is_empty() || source.contains(nonce) {
        return Err(ProtectError::NonceCollision);
    }
    Ok(protect_inner(source, nonce))
}

/// Protect Markdown with a random nonce, regenerating on collision.
pub fn protect_markdown(source: &str) -> Result<ProtectedDocument, ProtectError> {
    for _ in 0..32 {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        if !source.contains(&nonce) {
            return Ok(protect_inner(source, &nonce));
        }
    }
    Err(ProtectError::NonceCollision)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaceholderKind {
    Code,
    Inline,
}

struct ExpectedEntry<'a> {
    token: &'a str,
    kind: PlaceholderKind,
    fence_region: usize,
}

/// Restore originals into `output`. Fail-closed on type-aware integrity errors.
pub fn restore_markdown(
    output: &str,
    protected: &ProtectedDocument,
) -> Result<String, ProtectError> {
    Ok(restore_markdown_detailed(output, protected)?.text)
}

pub fn restore_markdown_detailed(
    output: &str,
    protected: &ProtectedDocument,
) -> Result<RestoreOutcome, ProtectError> {
    let mut fence_region = 0usize;
    let mut expected = Vec::with_capacity(protected.placeholders.len());
    for slot in &protected.placeholders {
        let kind = if slot.token.contains("CGCODE_") {
            PlaceholderKind::Code
        } else {
            PlaceholderKind::Inline
        };
        expected.push(ExpectedEntry {
            token: slot.token.as_str(),
            kind,
            fence_region,
        });
        if kind == PlaceholderKind::Code {
            fence_region += 1;
        }
    }

    let mut lookup = std::collections::HashMap::with_capacity(expected.len());
    for (idx, entry) in expected.iter().enumerate() {
        lookup.insert(entry.token, idx);
    }
    let expected_codes: Vec<&str> = expected
        .iter()
        .filter(|e| e.kind == PlaceholderKind::Code)
        .map(|e| e.token)
        .collect();
    let expected_inlines: Vec<&str> = expected
        .iter()
        .filter(|e| e.kind == PlaceholderKind::Inline)
        .map(|e| e.token)
        .collect();

    let mut counts = vec![0u8; expected.len()];
    let mut next_code = 0usize;
    let mut current_region = 0usize;
    let mut output_inlines = Vec::with_capacity(expected_inlines.len());

    let code_prefix = format!("{TOKEN_OPEN}CGCODE_{}_", protected.nonce);
    let inline_prefix = format!("{TOKEN_OPEN}CGINLINE_{}_", protected.nonce);
    let mut rest = output;
    while let Some(pos) = find_next_token_start(rest, &code_prefix, &inline_prefix) {
        let slice = &rest[pos..];
        let (prefix, kind) = if slice.starts_with(&code_prefix) {
            (code_prefix.as_str(), PlaceholderKind::Code)
        } else {
            (inline_prefix.as_str(), PlaceholderKind::Inline)
        };
        let after_prefix = &slice[prefix.len()..];
        let digit_end = after_prefix
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digit_end == 0 || !after_prefix[digit_end..].starts_with(TOKEN_CLOSE) {
            return Err(ProtectError::IntegrityFailed(
                IntegrityFailureKind::MalformedMarker,
            ));
        }
        let token_len = prefix.len() + digit_end + TOKEN_CLOSE.len_utf8();
        let token = &slice[..token_len];
        let Some(&idx) = lookup.get(token) else {
            return Err(ProtectError::IntegrityFailed(
                IntegrityFailureKind::UnknownToken,
            ));
        };
        counts[idx] = counts[idx].saturating_add(1);
        if counts[idx] > 1 {
            return Err(ProtectError::IntegrityFailed(
                IntegrityFailureKind::DuplicateToken,
            ));
        }
        match kind {
            PlaceholderKind::Code => {
                if next_code >= expected_codes.len() || token != expected_codes[next_code] {
                    return Err(ProtectError::IntegrityFailed(
                        IntegrityFailureKind::FencedReorder,
                    ));
                }
                next_code += 1;
                current_region += 1;
            }
            PlaceholderKind::Inline => {
                if expected[idx].fence_region != current_region {
                    return Err(ProtectError::IntegrityFailed(
                        IntegrityFailureKind::InlineCrossRegion,
                    ));
                }
                output_inlines.push(token);
            }
        }
        rest = &slice[token_len..];
    }

    if counts.iter().any(|&c| c != 1) {
        return Err(ProtectError::IntegrityFailed(
            IntegrityFailureKind::MissingToken,
        ));
    }

    let inline_reorder_count = expected_inlines
        .iter()
        .zip(output_inlines.iter())
        .filter(|(expected_token, found)| expected_token != found)
        .count();

    let mut result = output.to_string();
    for slot in &protected.placeholders {
        if let Some(pos) = result.find(&slot.token) {
            result.replace_range(pos..pos + slot.token.len(), &slot.original);
        } else {
            return Err(ProtectError::IntegrityFailed(
                IntegrityFailureKind::MissingToken,
            ));
        }
    }
    Ok(RestoreOutcome {
        text: result,
        inline_reorder_count,
    })
}

fn protect_inner(source: &str, nonce: &str) -> ProtectedDocument {
    let mut placeholders = Vec::new();
    let mut code_idx = 0usize;
    let mut inline_idx = 0usize;

    let after_fenced = replace_fenced(source, nonce, &mut code_idx, &mut placeholders);
    let text = replace_inline(&after_fenced, nonce, &mut inline_idx, &mut placeholders);

    // Integrity checks compare tokens in document order of first occurrence.
    placeholders.sort_by_key(|p| text.find(&p.token).unwrap_or(usize::MAX));

    ProtectedDocument {
        text,
        nonce: nonce.to_string(),
        placeholders,
    }
}

fn code_token(nonce: &str, n: usize) -> String {
    format!("{TOKEN_OPEN}CGCODE_{nonce}_{n}{TOKEN_CLOSE}")
}

fn inline_token(nonce: &str, n: usize) -> String {
    format!("{TOKEN_OPEN}CGINLINE_{nonce}_{n}{TOKEN_CLOSE}")
}

/// Replace complete fenced blocks left-to-right.
fn replace_fenced(
    source: &str,
    nonce: &str,
    code_idx: &mut usize,
    placeholders: &mut Vec<Placeholder>,
) -> String {
    let mut out = String::with_capacity(source.len());
    let mut i = 0;

    while i < source.len() {
        if is_line_start(source, i) {
            if let Some(end) = match_fenced_block(source, i) {
                let original = &source[i..end];
                let token = code_token(nonce, *code_idx);
                *code_idx += 1;
                placeholders.push(Placeholder {
                    token: token.clone(),
                    original: original.to_string(),
                });
                out.push_str(&token);
                i = end;
                continue;
            }
        }
        // Copy next char (UTF-8 safe).
        let ch = source[i..].chars().next().expect("i < len");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_line_start(s: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    // CommonMark line endings: \n, \r\n, or bare \r.
    // When i sits on the LF of a CRLF pair, that is not a content line start.
    let b = s.as_bytes();
    match b[i - 1] {
        b'\n' => true,
        b'\r' => b.get(i) != Some(&b'\n'),
        _ => false,
    }
}

/// Length of a CommonMark line ending at `bytes[idx]`, or 0 if none.
fn line_ending_len(bytes: &[u8], idx: usize) -> usize {
    match bytes.get(idx) {
        Some(b'\n') => 1,
        Some(b'\r') if bytes.get(idx + 1) == Some(&b'\n') => 2,
        Some(b'\r') => 1,
        _ => 0,
    }
}

/// Exclusive end of the line that begins at `pos` (includes any trailing
/// line ending), or `bytes.len()` when the line runs to EOF.
fn line_end_exclusive(bytes: &[u8], pos: usize) -> usize {
    let mut k = pos;
    while k < bytes.len() {
        let le = line_ending_len(bytes, k);
        if le > 0 {
            return k + le;
        }
        k += 1;
    }
    bytes.len()
}

/// If a fenced block starts at `i`, return exclusive end index.
///
/// Matches CommonMark: optional ≤3 space indent, `` ` `` / `~` fence ≥3,
/// info string (no backticks for `` ` `` fences), then body until a matching
/// close line **or EOF** (unclosed fences protect through end of document).
/// Line endings may be `\n`, `\r\n`, or bare `\r`.
fn match_fenced_block(s: &str, i: usize) -> Option<usize> {
    let rest = &s[i..];
    let bytes = rest.as_bytes();

    // Optional up to 3 spaces of indent (CommonMark).
    let mut j = 0usize;
    while j < 3 && j < bytes.len() && bytes[j] == b' ' {
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }

    let fence_char = bytes[j];
    if fence_char != b'`' && fence_char != b'~' {
        return None;
    }

    let mut fence_len = 0usize;
    while j + fence_len < bytes.len() && bytes[j + fence_len] == fence_char {
        fence_len += 1;
    }
    if fence_len < 3 {
        return None;
    }

    // Opening info string: backtick fences cannot contain backticks in info.
    // Scan until line ending or EOF (opening fence may be the last line).
    let mut k = j + fence_len;
    while k < bytes.len() {
        let le = line_ending_len(bytes, k);
        if le > 0 {
            break;
        }
        if fence_char == b'`' && bytes[k] == b'`' {
            return None;
        }
        k += 1;
    }

    // Body starts after the opening line's ending; at EOF the body is empty
    // and the unclosed fence spans through end of document.
    let mut pos = if k < bytes.len() {
        k + line_ending_len(bytes, k)
    } else {
        bytes.len()
    };

    loop {
        if pos >= bytes.len() {
            // Unclosed fence: protect opening line through EOF.
            return Some(s.len());
        }

        // Examine the line starting at `pos`.
        let mut m = pos;
        let mut spaces = 0usize;
        while spaces < 3 && m < bytes.len() && bytes[m] == b' ' {
            m += 1;
            spaces += 1;
        }

        let mut close_len = 0usize;
        while m + close_len < bytes.len() && bytes[m + close_len] == fence_char {
            close_len += 1;
        }

        if close_len >= fence_len {
            let mut end = m + close_len;
            while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
                end += 1;
            }
            let le = line_ending_len(bytes, end);
            if end >= bytes.len() || le > 0 {
                // Include trailing line ending of the closing fence when present.
                return Some(i + if le > 0 { end + le } else { end });
            }
        }

        // Advance past this line (content + line ending), or to EOF.
        let next = line_end_exclusive(bytes, pos);
        if next <= pos {
            // Safety: always make progress.
            return Some(s.len());
        }
        if next >= bytes.len() {
            // No further lines and no closer → unclosed through EOF.
            return Some(s.len());
        }
        pos = next;
    }
}

/// Replace single-level inline `` `...` `` spans (no newlines inside).
fn replace_inline(
    source: &str,
    nonce: &str,
    inline_idx: &mut usize,
    placeholders: &mut Vec<Placeholder>,
) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;

    while i < source.len() {
        if bytes[i] == b'`' {
            // Count opening run. v1: only single-backtick spans.
            let mut open_len = 0usize;
            while i + open_len < bytes.len() && bytes[i + open_len] == b'`' {
                open_len += 1;
            }
            if open_len == 1 {
                // Find closing single backtick before any line ending / EOF.
                // Prefer the first candidate close-run of length 1 so
                // `` `a` `` style nesting is not required in v1.
                let search = &source[i + 1..];
                let mut search_off = 0usize;
                let mut matched = false;
                while let Some(rel) = search[search_off..].find('`') {
                    let close = i + 1 + search_off + rel;
                    let inner = &source[i + 1..close];
                    // Inline code cannot span lines (LF / CRLF / CR).
                    if inner.contains('\n') || inner.contains('\r') {
                        break;
                    }
                    let close_run = {
                        let mut n = 0usize;
                        while close + n < bytes.len() && bytes[close + n] == b'`' {
                            n += 1;
                        }
                        n
                    };
                    if close_run == 1 {
                        let end = close + 1;
                        let original = &source[i..end];
                        let token = inline_token(nonce, *inline_idx);
                        *inline_idx += 1;
                        placeholders.push(Placeholder {
                            token: token.clone(),
                            original: original.to_string(),
                        });
                        out.push_str(&token);
                        i = end;
                        matched = true;
                        break;
                    }
                    // Multi-backtick close is not a v1 closer; keep scanning.
                    search_off = search_off + rel + close_run.max(1);
                }
                if matched {
                    continue;
                }
            }
            // Not a v1 inline span — copy the opening run as-is.
            for _ in 0..open_len {
                out.push('`');
            }
            i += open_len;
            continue;
        }

        let ch = source[i..].chars().next().expect("i < len");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn find_next_token_start(s: &str, code_prefix: &str, inline_prefix: &str) -> Option<usize> {
    let c = s.find(code_prefix);
    let i = s.find(inline_prefix);
    match (c, i) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "n0";

    fn swap_once(haystack: &str, a: &str, b: &str) -> String {
        haystack
            .replacen(a, "@@TMP@@", 1)
            .replacen(b, a, 1)
            .replacen("@@TMP@@", b, 1)
    }

    fn token_pair(protected: &ProtectedDocument, a: &str, b: &str) {
        assert!(
            protected.text.contains(a) && protected.text.contains(b),
            "missing tokens in {}",
            protected.text
        );
    }

    #[test]
    fn round_trip_fenced_backtick_tilde_and_inline() {
        let source = "\
Intro `inline` text

```rust
fn main() {
    println!(\"hi\");
}
```

Middle `x`

~~~bash
echo hi
~~~

Outro
";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();

        assert!(
            protected.text.contains("⟦CGINLINE_n0_0⟧"),
            "inline token: {}",
            protected.text
        );
        assert!(
            protected.text.contains("⟦CGCODE_n0_0⟧"),
            "fenced ``` token: {}",
            protected.text
        );
        assert!(
            protected.text.contains("⟦CGCODE_n0_1⟧"),
            "fenced ~~~ token: {}",
            protected.text
        );
        assert!(
            protected.text.contains("⟦CGINLINE_n0_1⟧"),
            "second inline: {}",
            protected.text
        );
        assert!(
            !protected.text.contains("fn main"),
            "code body must be stripped"
        );
        assert!(
            !protected.text.contains("`inline`"),
            "inline source must be stripped"
        );

        let restored = restore_markdown(&protected.text, &protected).unwrap();
        assert_eq!(restored, source);
    }

    #[test]
    fn missing_token_fails() {
        // Fence must start at line beginning (CommonMark).
        let source = "A `one` and\n```\nblock\n```\n";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        assert!(
            protected.text.contains("⟦CGCODE_n0_0⟧"),
            "expected fenced token in {}",
            protected.text
        );
        let broken = protected.text.replacen("⟦CGCODE_n0_0⟧", "MISSING", 1);
        let err = restore_markdown(&broken, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::MissingToken)
        );
    }

    #[test]
    fn duplicate_token_in_output_fails() {
        let source = "before `only` after";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let token = "⟦CGINLINE_n0_0⟧";
        let broken = format!("{} extra {token}", protected.text);
        let err = restore_markdown(&broken, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::DuplicateToken)
        );
    }

    #[test]
    fn same_region_inline_reorder_restores() {
        let source = "A `first` B `second` C";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let t0 = "⟦CGINLINE_n0_0⟧";
        let t1 = "⟦CGINLINE_n0_1⟧";
        token_pair(&protected, t0, t1);
        let swapped = swap_once(&protected.text, t0, t1);
        let outcome = restore_markdown_detailed(&swapped, &protected).unwrap();
        assert_eq!(outcome.text, "A `second` B `first` C");
        assert_eq!(outcome.inline_reorder_count, 2);
    }

    #[test]
    fn observed_id_data_blobs_inline_swap_restores() {
        let source = "It queries `id, data` from `blobs`.";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let t0 = "⟦CGINLINE_n0_0⟧";
        let t1 = "⟦CGINLINE_n0_1⟧";
        token_pair(&protected, t0, t1);
        let output = format!("It queries-from {t1} the fields {t0}.");
        let outcome = restore_markdown_detailed(&output, &protected).unwrap();
        assert_eq!(
            outcome.text,
            "It queries-from `blobs` the fields `id, data`."
        );
        assert_eq!(outcome.inline_reorder_count, 2);
    }

    #[test]
    fn multiple_same_region_inline_reorders_restore() {
        let source = "use `a` then `b` then `c`.";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let t0 = "⟦CGINLINE_n0_0⟧";
        let t1 = "⟦CGINLINE_n0_1⟧";
        let t2 = "⟦CGINLINE_n0_2⟧";
        let output = protected
            .text
            .replacen(t0, "@@0@@", 1)
            .replacen(t1, "@@1@@", 1)
            .replacen(t2, "@@2@@", 1)
            .replacen("@@0@@", t2, 1)
            .replacen("@@1@@", t0, 1)
            .replacen("@@2@@", t1, 1);
        let outcome = restore_markdown_detailed(&output, &protected).unwrap();
        assert_eq!(outcome.text, "use `c` then `a` then `b`.");
        assert_eq!(outcome.inline_reorder_count, 3);
    }

    #[test]
    fn inline_across_fenced_block_fails() {
        let source = "before `one`\n```\nblock\n```\nafter `two`\n";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let inline0 = "⟦CGINLINE_n0_0⟧";
        let inline1 = "⟦CGINLINE_n0_1⟧";
        let code = "⟦CGCODE_n0_0⟧";
        token_pair(&protected, inline0, inline1);
        assert!(protected.text.contains(code));
        let crossed = swap_once(&protected.text, inline0, inline1);
        let err = restore_markdown(&crossed, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::InlineCrossRegion)
        );
    }

    #[test]
    fn fenced_code_reorder_fails() {
        let source = "```\none\n```\n\n```\ntwo\n```\n";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let c0 = "⟦CGCODE_n0_0⟧";
        let c1 = "⟦CGCODE_n0_1⟧";
        token_pair(&protected, c0, c1);
        let swapped = swap_once(&protected.text, c0, c1);
        let err = restore_markdown(&swapped, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::FencedReorder)
        );
    }

    #[test]
    fn missing_inline_token_fails() {
        let source = "A `one` and `two`";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let broken = protected.text.replacen("⟦CGINLINE_n0_1⟧", "GONE", 1);
        let err = restore_markdown(&broken, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::MissingToken)
        );
    }

    #[test]
    fn duplicate_fenced_token_fails() {
        let source = "```\nblock\n```\n";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let token = "⟦CGCODE_n0_0⟧";
        let broken = format!("{} extra {token}", protected.text);
        let err = restore_markdown(&broken, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::DuplicateToken)
        );
    }

    #[test]
    fn malformed_marker_fails_even_when_expected_tokens_present() {
        let source = "use `code` please";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let complete = "⟦CGINLINE_n0_0⟧";
        assert!(protected.text.contains(complete));
        let truncated = format!("{} and ⟦CGINLINE_n0_", protected.text);
        let err = restore_markdown(&truncated, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::MalformedMarker)
        );

        let no_close = format!("{} and ⟦CGINLINE_n0_0", protected.text);
        let err = restore_markdown(&no_close, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::MalformedMarker)
        );
    }

    #[test]
    fn foreign_nonce_literal_is_ignored() {
        let source = "use `code` please";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let output = format!(
            "{} and literal ⟦CGINLINE_other_0⟧ plus ⟦CGCODE_ffff_1⟧",
            protected.text
        );
        let restored = restore_markdown(&output, &protected).unwrap();
        assert!(restored.contains("`code`"));
        assert!(restored.contains("⟦CGINLINE_other_0⟧"));
        assert!(restored.contains("⟦CGCODE_ffff_1⟧"));
    }

    #[test]
    fn unchanged_mixed_document_round_trips_with_zero_inline_reorders() {
        let source = "Intro `inline`\n\n```rust\nfn main() {}\n```\n\nOutro `x`\n";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let outcome = restore_markdown_detailed(&protected.text, &protected).unwrap();
        assert_eq!(outcome.text, source);
        assert_eq!(outcome.inline_reorder_count, 0);
    }

    #[test]
    fn integrity_failed_display_is_content_free() {
        let err = ProtectError::IntegrityFailed(IntegrityFailureKind::UnknownToken);
        let rendered = err.to_string();
        assert_eq!(rendered, "placeholder integrity check failed");
        assert!(!rendered.contains("CGINLINE"));
        assert!(!rendered.contains("CGCODE"));
        assert!(!rendered.contains(NONCE));
        assert!(!rendered.contains('⟦'));
    }

    #[test]
    fn altered_token_fails() {
        let source = "use `code` please";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        let broken = protected
            .text
            .replace("⟦CGINLINE_n0_0⟧", "⟦CGINLINE_n0_99⟧");
        let err = restore_markdown(&broken, &protected).unwrap_err();
        assert_eq!(
            err,
            ProtectError::IntegrityFailed(IntegrityFailureKind::UnknownToken)
        );
    }

    #[test]
    fn collision_with_nonce_errors_or_auto_regenerates() {
        // Deterministic path: source already contains the chosen nonce.
        let source = "nonce n0 appears here and `code` too";
        let err = protect_markdown_with_nonce(source, NONCE).unwrap_err();
        assert_eq!(err, ProtectError::NonceCollision);

        // Auto path regenerates until source does not contain the nonce.
        let protected = protect_markdown(source).unwrap();
        assert!(
            !source.contains(&protected.nonce),
            "auto nonce must not appear in source"
        );
        assert!(
            protected.text.contains("CGINLINE"),
            "inline still protected: {}",
            protected.text
        );
        let restored = restore_markdown(&protected.text, &protected).unwrap();
        assert_eq!(restored, source);
    }

    #[test]
    fn fenced_block_containing_backticks_preserved() {
        let source = "\
```js
const s = `template ${x}`;
console.log('`quoted`');
```
";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        // Whole fence is one CGCODE placeholder; inner backticks must not
        // become CGINLINE tokens.
        assert!(protected.text.contains("⟦CGCODE_n0_0⟧"));
        assert!(
            !protected.text.contains("CGINLINE"),
            "inner backticks must stay inside fenced placeholder: {}",
            protected.text
        );
        assert!(!protected.text.contains("template"), "fence body stripped");
        let restored = restore_markdown(&protected.text, &protected).unwrap();
        assert_eq!(restored, source);
    }

    #[test]
    fn restore_allows_prose_rewrite_around_tokens() {
        let source = "Hello `world` end";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        // Simulate model translating prose while leaving placeholders intact.
        let output = protected
            .text
            .replace("Hello", "Hola")
            .replace("end", "fin");
        let restored = restore_markdown(&output, &protected).unwrap();
        assert_eq!(restored, "Hola `world` fin");
    }

    #[test]
    fn crlf_fenced_blocks_are_protected_and_round_trip() {
        // Windows-style \r\n fences must close and protect like LF fences.
        let source = "Intro\r\n```rust\r\nfn main() {}\r\n```\r\nOutro `x`\r\n";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        assert!(
            protected.text.contains("⟦CGCODE_n0_0⟧"),
            "CRLF fenced block must become CGCODE: {}",
            protected.text
        );
        assert!(
            protected.text.contains("⟦CGINLINE_n0_0⟧"),
            "inline after CRLF fence: {}",
            protected.text
        );
        assert!(
            !protected.text.contains("fn main"),
            "CRLF fence body must be stripped: {}",
            protected.text
        );
        let restored = restore_markdown(&protected.text, &protected).unwrap();
        assert_eq!(restored, source);
    }

    #[test]
    fn crlf_tilde_fence_and_bare_cr_line_endings() {
        let crlf = "~~~\r\necho hi\r\n~~~\r\n";
        let protected = protect_markdown_with_nonce(crlf, NONCE).unwrap();
        assert!(protected.text.contains("⟦CGCODE_n0_0⟧"));
        assert_eq!(restore_markdown(&protected.text, &protected).unwrap(), crlf);

        // Bare CR line endings (less common, still CommonMark).
        let cr = "```\rcode\r```\r";
        let protected = protect_markdown_with_nonce(cr, NONCE).unwrap();
        assert!(
            protected.text.contains("⟦CGCODE_n0_0⟧"),
            "bare CR fence: {}",
            protected.text
        );
        assert_eq!(restore_markdown(&protected.text, &protected).unwrap(), cr);
    }

    #[test]
    fn unclosed_fence_protects_through_eof() {
        let source = "Before\n```js\nconst x = 1;\n// no closing fence";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        assert!(
            protected.text.contains("⟦CGCODE_n0_0⟧"),
            "unclosed fence must be protected: {}",
            protected.text
        );
        assert!(
            !protected.text.contains("const x"),
            "body through EOF must be inside placeholder: {}",
            protected.text
        );
        assert!(
            !protected.text.contains("CGINLINE"),
            "backticks inside unclosed fence must not become inline"
        );
        // Prose before the fence remains; fence+body is one token.
        assert!(protected.text.starts_with("Before\n⟦CGCODE_n0_0⟧"));
        let restored = restore_markdown(&protected.text, &protected).unwrap();
        assert_eq!(restored, source);
    }

    #[test]
    fn unclosed_fence_crlf_protects_through_eof() {
        let source = "Lead\r\n```\r\nbody line\r\nstill open";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        assert!(
            protected.text.contains("⟦CGCODE_n0_0⟧"),
            "unclosed CRLF fence: {}",
            protected.text
        );
        assert!(!protected.text.contains("body line"));
        assert_eq!(
            restore_markdown(&protected.text, &protected).unwrap(),
            source
        );
    }

    #[test]
    fn unclosed_fence_at_eof_without_body_newline() {
        // Opening fence is the last line of the document (no body).
        let source = "text\n```";
        let protected = protect_markdown_with_nonce(source, NONCE).unwrap();
        assert!(
            protected.text.contains("⟦CGCODE_n0_0⟧"),
            "opening fence at EOF: {}",
            protected.text
        );
        assert_eq!(
            restore_markdown(&protected.text, &protected).unwrap(),
            source
        );
    }
}
