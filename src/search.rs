//! Finding names across a whole scan.

use rayon::prelude::*;

use crate::tree::Tree;

/// What a search found.
pub struct SearchResult {
    pub query: String,
    /// Matches, biggest first, at most [`MAX_LISTED`].
    pub listed: Vec<u32>,
    /// Every match, listed or not.
    pub total: u64,
    /// The bytes those matches hold, a folder counted once however much of it matched.
    pub bytes: u64,
    /// One bit per node: set for a match, for everything inside a matched
    /// folder and for every folder above a match. The map dims the rest.
    lit: Vec<u64>,
}

/// The most matches the results list carries.
pub const MAX_LISTED: usize = 2000;

#[inline]
fn get(bits: &[u64], node: u32) -> bool {
    bits[node as usize / 64] >> (node % 64) & 1 == 1
}

#[inline]
fn set(bits: &mut [u64], node: u32) {
    bits[node as usize / 64] |= 1 << (node % 64);
}

impl SearchResult {
    /// Whether `node` stays lit on the map.
    #[inline]
    pub fn is_lit(&self, node: u32) -> bool {
        get(&self.lit, node)
    }
}

/// Lower-cased name test: ASCII without allocating, anything else through
/// Unicode lower-casing.
fn name_matches(name: &[u8], needle: &str, whole_suffix: bool) -> bool {
    if needle.is_ascii() && name.is_ascii() {
        let needle = needle.as_bytes();
        if whole_suffix {
            return name.len() >= needle.len()
                && name[name.len() - needle.len()..].eq_ignore_ascii_case(needle);
        }
        return needle.len() <= name.len()
            && name.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle));
    }
    let lower = String::from_utf8_lossy(name).to_lowercase();
    if whole_suffix {
        lower.ends_with(needle)
    } else {
        lower.contains(needle)
    }
}

/// Runs `query` over `tree`. `*.ext` matches by ending; anything else is a
/// case-insensitive substring of the name. Removed nodes never match.
pub fn run(tree: &Tree, query: &str) -> SearchResult {
    let trimmed = query.trim().to_lowercase();
    let (needle, suffix) = match trimmed.strip_prefix('*') {
        Some(rest) => (rest.to_owned(), true),
        None => (trimmed.clone(), false),
    };

    let mut matches: Vec<u32> = (1..tree.len() as u32)
        .into_par_iter()
        .filter(|&i| !tree.is_removed(i) && name_matches(tree.name_bytes(i), &needle, suffix))
        .collect();
    let total = matches.len() as u64;

    let words = tree.len() / 64 + 1;
    let mut matched = vec![0_u64; words];
    for &m in &matches {
        set(&mut matched, m);
    }

    // Parents come first, so one forward pass spreads a match down its folder
    // and finds the matches that sit inside another one.
    let mut inside = vec![0_u64; words];
    let mut bytes = 0_u64;
    for i in 1..tree.len() as u32 {
        let parent_inside = tree.parent(i).is_some_and(|p| get(&inside, p));
        if get(&matched, i) && !parent_inside {
            bytes += tree.size(i);
        }
        if get(&matched, i) || parent_inside {
            set(&mut inside, i);
        }
    }

    // And each match lights the chain above it, stopping at a folder already lit.
    let mut lit = inside;
    for &m in &matches {
        let mut at = tree.parent(m);
        while let Some(link) = at {
            if get(&lit, link) {
                break;
            }
            set(&mut lit, link);
            at = tree.parent(link);
        }
    }

    matches.par_sort_unstable_by(|a, b| tree.size(*b).cmp(&tree.size(*a)));
    matches.truncate(MAX_LISTED);
    SearchResult {
        query: query.to_owned(),
        listed: matches,
        total,
        bytes,
        lit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::test_tree::sample;

    #[test]
    fn substrings_ignore_case() {
        let t = sample();
        assert_eq!(run(&t, "B.MKV").listed, vec![5]);
    }

    #[test]
    fn a_star_matches_by_ending() {
        let t = sample();
        let found = run(&t, "*.zip");
        assert_eq!(found.listed, vec![7]);
        assert!(run(&t, "*.zi").listed.is_empty());
    }

    #[test]
    fn matches_light_their_chain_and_nothing_else() {
        let t = sample();
        let found = run(&t, "c.zip");
        assert!(found.is_lit(7) && found.is_lit(6) && found.is_lit(2) && found.is_lit(0));
        assert!(!found.is_lit(5) && !found.is_lit(1));
    }

    #[test]
    fn a_matched_folder_lights_everything_inside_it() {
        let t = sample();
        assert!(run(&t, "deep").is_lit(7));
    }

    #[test]
    fn a_folder_and_what_it_holds_count_once() {
        let t = crate::tree::test_tree::tree(&[
            (0, "root", true, 0),
            (0, "cache", true, 0),
            (1, "cache.bin", false, 40),
        ]);
        let found = run(&t, "cache");
        assert_eq!(found.total, 2);
        assert_eq!(found.bytes, 40);
    }

    #[test]
    fn removed_nodes_do_not_match() {
        let mut t = sample();
        t.remove(5);
        assert!(run(&t, "b.mkv").listed.is_empty());
    }

    #[test]
    fn unicode_names_match_case_insensitively() {
        let t = crate::tree::test_tree::tree(&[(0, "root", true, 0), (0, "ÑANDÚ.txt", false, 1)]);
        assert_eq!(run(&t, "ñandú").listed, vec![1]);
    }
}
