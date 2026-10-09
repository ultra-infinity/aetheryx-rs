//! The builtin registry.

use std::sync::OnceLock;

use crate::generated::ENTRIES;

/// How a builtin is bound in Nix's global scope.
///
/// Every builtin `x` is also `builtins.x`. In the global scope, which is
/// lexical and therefore takes precedence over `with`, it is bound either as
/// `x` (bare) or only as `__x` (prefixed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Global {
    /// Bound as `x`, e.g. `map`, `toString`, `import`, `true`.
    Bare,
    /// Bound only as `__x`, e.g. `__add`, `__length`, `__findFile`.
    Prefixed,
}

/// What kind of value a builtin is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A function implemented natively (`<PRIMOP>` in Nix).
    PrimOp,
    /// A function that Nix itself implements in Nix code (`<LAMBDA>`), such
    /// as `derivation`.
    NixDefined,
    /// A plain value, e.g. `true`, `storeDir`, `builtins`.
    Value,
}

/// One attribute of `builtins`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Builtin {
    /// The name in `builtins`, e.g. `add` for `__add`.
    pub name: &'static str,
    pub global: Global,
    pub kind: Kind,
    /// Whether it exists when evaluating with `pure-eval`.
    pub in_pure_eval: bool,
}

impl Builtin {
    /// The identifier this builtin is bound to in the global scope.
    pub fn global_name(&self) -> String {
        match self.global {
            Global::Bare => self.name.to_string(),
            Global::Prefixed => format!("__{}", self.name),
        }
    }
}

/// Every builtin, sorted by name.
pub fn all() -> &'static [Builtin] {
    static ALL: OnceLock<Vec<Builtin>> = OnceLock::new();
    ALL.get_or_init(|| {
        ENTRIES
            .iter()
            .map(|&(name, global, kind, in_pure_eval)| Builtin {
                name,
                global,
                kind,
                in_pure_eval,
            })
            .collect()
    })
}

/// The builtin `builtins.<name>`.
pub fn get(name: &str) -> Option<&'static Builtin> {
    all().iter().find(|b| b.name == name)
}

/// The builtin that the identifier `ident` refers to in Nix's global scope,
/// if any: `map` -> `map`, `__add` -> `add`, but `add` -> `None`.
pub fn lookup_global(ident: &str) -> Option<&'static Builtin> {
    if let Some(builtin) = get(ident).filter(|b| b.global == Global::Bare) {
        return Some(builtin);
    }
    let name = ident.strip_prefix("__")?;
    get(name).filter(|b| b.global == Global::Prefixed)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    // Counts are those of Nix 2.28.3, which generated the registry.
    #[test]
    fn registry_matches_nix() {
        assert_eq!(all().len(), 118);
        let bare: Vec<_> = all()
            .iter()
            .filter(|b| b.global == Global::Bare)
            .map(|b| b.name)
            .collect();
        assert_eq!(
            bare,
            [
                "abort", "baseNameOf", "break", "builtins", "derivation", "derivationStrict",
                "dirOf", "false", "fetchGit", "fetchMercurial", "fetchTarball", "fetchTree",
                "fromTOML", "import", "isNull", "map", "null", "placeholder", "removeAttrs",
                "scopedImport", "throw", "toString", "true",
            ]
        );
        let names: HashSet<_> = all().iter().map(|b| b.name).collect();
        assert_eq!(names.len(), all().len(), "duplicate names");
    }

    #[test]
    fn lookup_global_follows_the_binding_form() {
        assert_eq!(lookup_global("map").map(|b| b.name), Some("map"));
        assert_eq!(lookup_global("__add").map(|b| b.name), Some("add"));
        assert_eq!(lookup_global("__findFile").map(|b| b.name), Some("findFile"));
        assert_eq!(lookup_global("add"), None);
        assert_eq!(lookup_global("length"), None);
        // Bare globals are not also bound with the prefix.
        assert_eq!(lookup_global("__map"), None);
        assert_eq!(lookup_global("x"), None);
    }

    #[test]
    fn kinds_match_nix() {
        assert_eq!(get("map").unwrap().kind, Kind::PrimOp);
        assert_eq!(get("derivation").unwrap().kind, Kind::NixDefined);
        assert_eq!(get("true").unwrap().kind, Kind::Value);
        assert_eq!(get("storeDir").unwrap().kind, Kind::Value);
        assert!(!get("currentTime").unwrap().in_pure_eval);
        assert!(get("getEnv").unwrap().in_pure_eval);
    }
}
