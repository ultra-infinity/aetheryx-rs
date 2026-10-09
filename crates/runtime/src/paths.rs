//! Path normalization at runtime (`./a/${b}`, `path + "str"`).

/// Textually normalizes an absolute path like Nix's `canonPath`: empty and
/// `.` components are dropped and `..` removes the previous component, never
/// going above `/`. Symlinks are not resolved.
///
/// This is the same rule as `ir::paths::canon_path`, which the compiler
/// applies to path literals; a test in the `eval` crate checks that they
/// agree.
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
