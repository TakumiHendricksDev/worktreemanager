//! Find in Files' file mask: `*.py`, `*.vue, *.ts`, `!*.min.js`, `apps/**/tests/*.py`.

use regex::Regex;

use crate::CodeError;

/// Which files a search looks in, from what was typed in the mask box.
///
/// Comma-separated globs. A glob without a `/` matches the file's name, the way an IDE's file mask
/// does — `*.py` is every Python file at any depth. One with a `/` matches the whole path from the
/// worktree root. A leading `!` excludes. With no including glob, everything not excluded is in.
#[derive(Debug, Clone, Default)]
pub struct Mask {
    include: Vec<Glob>,
    exclude: Vec<Glob>,
}

#[derive(Debug, Clone)]
struct Glob {
    regex: Regex,
    whole_path: bool,
}

impl Mask {
    /// Parse a mask. An empty one admits every file.
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        let mut mask = Self::default();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (negated, glob) = match part.strip_prefix('!') {
                Some(rest) => (true, rest.trim()),
                None => (false, part),
            };
            if glob.is_empty() {
                continue;
            }
            let compiled = Glob {
                regex: Regex::new(&to_regex(glob))
                    .map_err(|e| CodeError::BadQuery(format!("file mask `{glob}`: {e}")))?,
                whole_path: glob.contains('/'),
            };
            if negated {
                mask.exclude.push(compiled);
            } else {
                mask.include.push(compiled);
            }
        }
        Ok(mask)
    }

    /// Whether a worktree-relative path is one the search should read.
    #[must_use]
    pub fn admits(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        let hit = |glob: &Glob| {
            glob.regex
                .is_match(if glob.whole_path { path } else { name })
        };
        (self.include.is_empty() || self.include.iter().any(hit)) && !self.exclude.iter().any(hit)
    }
}

/// A glob as an anchored regex: `**` crosses folders, `*` and `?` stay inside one.
fn to_regex(glob: &str) -> String {
    let mut out = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                // `**/` is zero or more whole folders, so `apps/**/tests` also matches `apps/tests`.
                if chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn an_empty_mask_admits_everything() {
        let mask = Mask::parse("  ").unwrap();
        assert!(mask.admits("src/app.py"));
    }

    #[test]
    fn a_name_glob_matches_at_any_depth_and_only_the_name() {
        let mask = Mask::parse("*.py").unwrap();
        assert!(mask.admits("manage.py"));
        assert!(mask.admits("apps/events/views.py"));
        assert!(!mask.admits("apps/events/views.pyc"));
        assert!(!mask.admits("py/readme.md"), "the folder is not the name");
    }

    #[test]
    fn several_globs_are_alternatives_and_a_bang_excludes() {
        let mask = Mask::parse("*.js, *.vue, !*.min.js").unwrap();
        assert!(mask.admits("src/app.js"));
        assert!(mask.admits("src/Table.vue"));
        assert!(!mask.admits("static/vendor.min.js"));
        assert!(!mask.admits("src/app.ts"));
    }

    #[test]
    fn an_exclusion_alone_admits_everything_else() {
        let mask = Mask::parse("!*.lock").unwrap();
        assert!(mask.admits("src/app.rs"));
        assert!(!mask.admits("Cargo.lock"));
    }

    #[test]
    fn a_glob_with_a_slash_is_a_path_and_double_star_crosses_folders() {
        let mask = Mask::parse("apps/**/tests/*.py").unwrap();
        assert!(mask.admits("apps/events/tests/test_views.py"));
        assert!(
            mask.admits("apps/tests/test_models.py"),
            "** may be no folders at all"
        );
        assert!(
            !mask.admits("lib/apps/events/tests/test_views.py"),
            "anchored at the root"
        );
        assert!(
            !mask.admits("apps/events/tests/nested/test_views.py"),
            "* stays in one folder"
        );
    }

    #[test]
    fn regex_characters_in_a_glob_are_literal() {
        let mask = Mask::parse("a+b(1).txt").unwrap();
        assert!(mask.admits("a+b(1).txt"));
        assert!(!mask.admits("aab1.txt"));
    }
}
