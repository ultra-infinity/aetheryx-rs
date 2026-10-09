//! Nix path resolution, as done by the Nix parser (verified against Nix 2.28).
//!
//! Nix resolves path literals when a file is parsed, not when it is evaluated:
//! - `./x`, `../x` are made absolute against the directory of the file being
//!   parsed (or the current directory for in-memory sources), then normalized.
//! - `/x` is normalized.
//! - `~/x` becomes `$HOME/x` and is *not* normalized: `~/a/../b` stays
//!   `/home/user/a/../b`.
//!
//! Normalization is purely textual: empty and `.` components are dropped and
//! `..` removes the previous component (never going above `/`). Symlinks are
//! not resolved.
//!
//! For interpolated paths (`./a/${x}`) only the leading literal is resolved
//! here, keeping a trailing `/`. The concatenated result is normalized at
//! evaluation time, which is the evaluator's job (`nix.interpolate Path`).

/// Textually normalizes an absolute path, like Nix's `canonPath`.
pub fn canon_path(path: &str) -> String {
    let mut components: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            c => components.push(c),
        }
    }
    format!("/{}", components.join("/"))
}

/// Makes `path` absolute against the directory `base` and normalizes it,
/// like Nix's `absPath`.
pub fn abs_path(path: &str, base: &str) -> String {
    if path.starts_with('/') {
        canon_path(path)
    } else {
        canon_path(&format!("{base}/{path}"))
    }
}

/// The directory containing the normalized absolute path `path`, like Nix's
/// `dirOf`.
pub fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// Resolves the text of a path literal (or the leading literal of an
/// interpolated path) the way the Nix parser does.
///
/// Returns `None` for a `~/` path when the home directory is unknown.
pub fn resolve_literal(text: &str, base: &str, home: Option<&str>, interpolated: bool) -> Option<String> {
    if let Some(rest) = text.strip_prefix('~') {
        return home.map(|home| format!("{home}{rest}"));
    }
    let mut resolved = abs_path(text, base);
    // The leading literal of an interpolated path keeps its trailing slash.
    if interpolated && text.len() > 1 && text.ends_with('/') {
        resolved.push('/');
    }
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values are what Nix 2.28 produces for the same literal
    // (checked with `nix-instantiate --parse`).

    #[test]
    fn canon_path_matches_nix() {
        assert_eq!(canon_path("/.."), "/");
        assert_eq!(canon_path("/a/../.."), "/");
        assert_eq!(canon_path("/./a"), "/a");
        assert_eq!(canon_path("/etc/../etc/hosts"), "/etc/hosts");
        assert_eq!(canon_path("/x/y//z"), "/x/y/z");
        assert_eq!(canon_path("/x/"), "/x");
    }

    #[test]
    fn relative_literals_resolve_against_base() {
        let base = "/tmp";
        let r = |t| resolve_literal(t, base, None, false).unwrap();
        assert_eq!(r("./."), "/tmp");
        assert_eq!(r("../."), "/");
        assert_eq!(r("./a/../../.."), "/");
        assert_eq!(r("./a/./b"), "/tmp/a/b");
        assert_eq!(r("./a/../b"), "/tmp/b");
        assert_eq!(r("/etc/../etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn home_literals_are_not_normalized() {
        let r = |t| resolve_literal(t, "/tmp", Some("/home/u"), false);
        assert_eq!(r("~/foo/../bar").unwrap(), "/home/u/foo/../bar");
        assert_eq!(r("~/.").unwrap(), "/home/u/.");
        assert_eq!(resolve_literal("~/x", "/tmp", None, false), None);
    }

    #[test]
    fn interpolated_prefix_keeps_trailing_slash() {
        let r = |t| resolve_literal(t, "/tmp", Some("/home/u"), true).unwrap();
        assert_eq!(r("./a/"), "/tmp/a/");
        // `nix-instantiate --parse -E 'x: ./../${x}'` in /tmp prints `(// + x)`.
        assert_eq!(r("./../"), "//");
        assert_eq!(r("/x"), "/x");
        assert_eq!(r("/"), "/");
        assert_eq!(r("~/a/"), "/home/u/a/");
    }

    #[test]
    fn dir_of_matches_nix() {
        assert_eq!(dir_of("/a/b/c.nix"), "/a/b");
        assert_eq!(dir_of("/c.nix"), "/");
        assert_eq!(dir_of("/"), "/");
    }
}
