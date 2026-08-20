/// Simple case-insensitive subsequence matcher used for fuzzy filtering.
///
/// Returns the indices (character positions) of the matched characters in the
/// ORIGINAL `haystack` string and a score where smaller is better.
///
/// Unicode correctness: non-ASCII haystacks are lowercased as they are scanned
/// while retaining each character's original index. This ensures the returned
/// indices can be safely used with `str::chars().enumerate()` consumers for
/// highlighting, even when lowercasing expands certain characters (e.g.,
/// ß → ss, İ → i̇).
pub(crate) fn fuzzy_match(haystack: &str, needle: &str) -> Option<(Vec<usize>, i32)> {
    if needle.is_empty() {
        return Some((Vec::new(), i32::MAX));
    }

    if haystack.is_ascii() && needle.is_ascii() {
        return fuzzy_match_ascii(haystack.as_bytes(), needle.as_bytes());
    }

    if needle.is_ascii() {
        fuzzy_match_unicode(
            haystack,
            needle
                .bytes()
                .map(|byte| char::from(byte.to_ascii_lowercase())),
            needle.len(),
        )
    } else {
        // Preserve `str::to_lowercase`'s context-dependent Unicode behavior for
        // the needle; `char::to_lowercase` is intentionally not equivalent.
        let lowered_needle = needle.to_lowercase();
        let lowered_needle_len = lowered_needle.chars().count();
        fuzzy_match_unicode(haystack, lowered_needle.chars(), lowered_needle_len)
    }
}

fn fuzzy_match_ascii(haystack: &[u8], needle: &[u8]) -> Option<(Vec<usize>, i32)> {
    if needle.len() > haystack.len() {
        return None;
    }

    let first_needle = needle[0];
    let first_position = haystack
        .iter()
        .position(|byte| byte.eq_ignore_ascii_case(&first_needle))?;
    let mut indices = Vec::with_capacity(needle.len());
    indices.push(first_position);
    let mut position = first_position + 1;
    let mut last_position = first_position;
    for needle_byte in &needle[1..] {
        let relative_position = haystack[position..]
            .iter()
            .position(|byte| byte.eq_ignore_ascii_case(needle_byte))?;
        position += relative_position;
        indices.push(position);
        last_position = position;
        position += 1;
    }

    Some((
        indices,
        match_score(first_position, last_position, needle.len()),
    ))
}

fn fuzzy_match_unicode(
    haystack: &str,
    mut lowered_needle: impl Iterator<Item = char>,
    lowered_needle_len: usize,
) -> Option<(Vec<usize>, i32)> {
    let mut lowered_haystack = haystack
        .chars()
        .enumerate()
        .flat_map(|(original_index, character)| {
            character
                .to_lowercase()
                .enumerate()
                .map(move |(expansion_index, lowered_character)| {
                    (original_index, expansion_index, lowered_character)
                })
        })
        .enumerate();
    let mut next_match = |needle_character| {
        lowered_haystack.find_map(
            |(lowered_index, (original_index, expansion_index, haystack_character))| {
                (haystack_character == needle_character).then_some((
                    lowered_index,
                    original_index,
                    expansion_index,
                ))
            },
        )
    };

    let (first_match_position, first_original_index, first_expansion_index) =
        next_match(lowered_needle.next()?)?;
    let first_position = first_match_position - first_expansion_index;
    let mut indices = Vec::with_capacity(lowered_needle_len);
    indices.push(first_original_index);
    let mut last_position = first_match_position;
    for needle_character in lowered_needle {
        let (matched_position, original_index, _) = next_match(needle_character)?;
        if indices.last().copied() != Some(original_index) {
            indices.push(original_index);
        }
        last_position = matched_position;
    }

    Some((
        indices,
        match_score(first_position, last_position, lowered_needle_len),
    ))
}

fn match_score(first_position: usize, last_position: usize, needle_len: usize) -> i32 {
    let window = (last_position as i32 - first_position as i32 + 1) - needle_len as i32;
    let mut score = window.max(0);
    if first_position == 0 {
        score -= 100;
    }
    score
}

