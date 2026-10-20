//! Path globs for the Work view: which changed files a role or a rule is about.
//!
//! # Why not the matcher in the create pipeline
//!
//! That one knows `*` and nothing else, because it matches branch names, where `/` is just a
//! character. A path glob has to treat `/` as a boundary: `src/*.ts` must not reach into
//! `src/lib/`, and `**` is the one way to say "at any depth". Teaching the branch matcher that
//! would change what every existing `existing_branch_match` pattern means.
//!
//! # The dialect, and what it leaves out
//!
//! `*` and `?` match within one path segment, and a `**` segment matches any number of segments,
//! none included. Globs are relative to the repository root and anchored there, so `*.svelte`
//! means the top level only and `**/*.svelte` means anywhere. A glob that matches a directory
//! matches everything under it, so `docs` covers `docs/guide.md`.
//!
//! No character classes and no negation. Both are easy to add and neither has a use yet, and a
//! dialect is much harder to take back than to extend.

/// Whether `pattern` covers `path`. Both use `/` and are relative to the repository root.
#[must_use]
pub fn matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.trim_end_matches('/').split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

/// Whether `matches` will accept `pattern` as written, or why not.
///
/// Config validation calls this so a glob that could never match anything is an error with the
/// key named, rather than a role that silently never runs.
pub fn check(pattern: &str) -> Result<(), &'static str> {
    if pattern.trim().is_empty() {
        return Err("a path glob cannot be empty");
    }
    if pattern.starts_with('/') {
        return Err("a path glob is relative to the repository root, so it cannot start with `/`");
    }
    if pattern.split('/').any(|segment| segment == "..") {
        return Err("a path glob cannot climb out of the repository with `..`");
    }
    Ok(())
}

/// Match path segments. The pattern running out first is a match: what is left of the path is
/// inside the directory the pattern named.
fn segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => true,
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments(rest, &path[skip..])),
        Some((head, rest)) => match path.split_first() {
            Some((first, tail)) => segment(head, first) && segments(rest, tail),
            None => false,
        },
    }
}

/// Match one segment, with `*` for any run of characters and `?` for exactly one.
///
/// The usual backtracking matcher: on a mismatch after a `*`, let that star swallow one more
/// character and try again. Linear in practice, and a segment is a file name.
fn segment(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;

    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_stays_inside_one_directory() {
        assert!(matches("src/*.ts", "src/main.ts"));
        assert!(!matches("src/*.ts", "src/lib/main.ts"));
    }

    #[test]
    fn a_double_star_reaches_any_depth_including_none() {
        assert!(matches("src/**/*.svelte", "src/App.svelte"));
        assert!(matches("src/**/*.svelte", "src/lib/work/Queue.svelte"));
        assert!(matches("**/*.svelte", "src/App.svelte"));
        assert!(!matches("src/**/*.svelte", "docs/App.svelte"));
    }

    #[test]
    fn a_glob_is_anchored_at_the_repository_root() {
        // Unlike a .gitignore pattern, a bare `*.md` does not float to every depth. Saying where
        // is cheap with `**/`, and a role that reviews more than you meant costs a review.
        assert!(matches("*.md", "README.md"));
        assert!(!matches("*.md", "docs/guide.md"));
    }

    #[test]
    fn naming_a_directory_covers_everything_inside_it() {
        assert!(matches("docs", "docs/guide.md"));
        assert!(matches("docs/", "docs/deep/guide.md"));
        assert!(!matches("docs", "documentation/guide.md"));
    }

    #[test]
    fn a_question_mark_is_exactly_one_character() {
        assert!(matches("v?.txt", "v1.txt"));
        assert!(!matches("v?.txt", "v10.txt"));
    }

    #[test]
    fn a_star_backtracks_past_an_early_partial_match() {
        assert!(matches("*.test.ts", "user.test.test.ts"));
        assert!(!matches("*.test.ts", "user.test.tsx"));
    }

    #[test]
    fn a_glob_that_could_never_match_inside_the_repository_is_refused() {
        assert!(check("src/**").is_ok());
        assert!(check("").is_err());
        assert!(check("/etc/passwd").is_err());
        assert!(check("../sibling/**").is_err());
    }
}
