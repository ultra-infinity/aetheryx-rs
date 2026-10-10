//! Nix's configuration settings. `nix` accepts them as `--option NAME
//! VALUE`, and as flags: `--NAME` / `--no-NAME` for Booleans, `--NAME VALUE`
//! for the others, and `--extra-NAME VALUE` to add to a list.

/// What a setting holds, which decides its flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `--NAME`, `--no-NAME`.
    Bool,
    /// `--NAME VALUE`.
    Value,
    /// `--NAME VALUE` and `--extra-NAME VALUE`.
    List,
}

/// Nix 2.28.3's settings (`nix config show`), with the flags `nix` accepts
/// for each, as probed from `nix` itself.
pub const SETTINGS: &[(&str, Kind)] = &[
    ("abort-on-warn", Kind::Bool),
    ("accept-flake-config", Kind::Bool),
    ("access-tokens", Kind::List),
    ("allow-dirty", Kind::Bool),
    ("allow-dirty-locks", Kind::Bool),
    ("allow-import-from-derivation", Kind::Bool),
    ("allow-new-privileges", Kind::Bool),
    ("allow-symlinked-store", Kind::Bool),
    ("allow-unsafe-native-code-during-evaluation", Kind::Bool),
    ("allowed-impure-host-deps", Kind::List),
    ("allowed-uris", Kind::List),
    ("allowed-users", Kind::List),
    ("always-allow-substitutes", Kind::Bool),
    ("auto-optimise-store", Kind::Bool),
    ("bash-prompt", Kind::Value),
    ("bash-prompt-prefix", Kind::Value),
    ("bash-prompt-suffix", Kind::Value),
    ("build-dir", Kind::Value),
    ("build-hook", Kind::List),
    ("build-poll-interval", Kind::Value),
    ("build-users-group", Kind::Value),
    ("builders", Kind::Value),
    ("builders-use-substitutes", Kind::Bool),
    ("commit-lock-file-summary", Kind::Value),
    ("compress-build-log", Kind::Bool),
    ("connect-timeout", Kind::Value),
    ("cores", Kind::Value),
    ("debugger-on-trace", Kind::Bool),
    ("debugger-on-warn", Kind::Bool),
    ("diff-hook", Kind::Value),
    ("download-attempts", Kind::Value),
    ("download-buffer-size", Kind::Value),
    ("download-speed", Kind::Value),
    ("eval-cache", Kind::Bool),
    ("eval-system", Kind::Value),
    ("experimental-features", Kind::List),
    ("extra-platforms", Kind::List),
    ("fallback", Kind::Bool),
    ("filter-syscalls", Kind::Bool),
    ("flake-registry", Kind::Value),
    ("fsync-metadata", Kind::Bool),
    ("fsync-store-paths", Kind::Bool),
    ("gc-reserved-space", Kind::Value),
    ("hashed-mirrors", Kind::List),
    ("http-connections", Kind::Value),
    ("http2", Kind::Bool),
    ("id-count", Kind::Value),
    ("ignore-try", Kind::Bool),
    ("ignored-acls", Kind::List),
    ("impersonate-linux-26", Kind::Bool),
    ("keep-build-log", Kind::Bool),
    ("keep-derivations", Kind::Bool),
    ("keep-env-derivations", Kind::Bool),
    ("keep-failed", Kind::Bool),
    ("keep-going", Kind::Bool),
    ("keep-outputs", Kind::Bool),
    ("log-lines", Kind::Value),
    ("max-build-log-size", Kind::Value),
    ("max-call-depth", Kind::Value),
    ("max-free", Kind::Value),
    ("max-jobs", Kind::Value),
    ("max-silent-time", Kind::Value),
    ("max-substitution-jobs", Kind::Value),
    ("min-free", Kind::Value),
    ("min-free-check-interval", Kind::Value),
    ("nar-buffer-size", Kind::Value),
    ("narinfo-cache-negative-ttl", Kind::Value),
    ("narinfo-cache-positive-ttl", Kind::Value),
    ("netrc-file", Kind::Value),
    ("nix-path", Kind::List),
    ("nix-shell-always-looks-for-shell-nix", Kind::Bool),
    ("nix-shell-shebang-arguments-relative-to-script", Kind::Bool),
    ("plugin-files", Kind::List),
    ("post-build-hook", Kind::Value),
    ("pre-build-hook", Kind::Value),
    ("preallocate-contents", Kind::Bool),
    ("print-missing", Kind::Bool),
    ("pure-eval", Kind::Bool),
    ("require-drop-supplementary-groups", Kind::Bool),
    ("require-sigs", Kind::Bool),
    ("restrict-eval", Kind::Bool),
    ("run-diff-hook", Kind::Bool),
    ("sandbox", Kind::Bool),
    ("sandbox-build-dir", Kind::Value),
    ("sandbox-dev-shm-size", Kind::Value),
    ("sandbox-fallback", Kind::Bool),
    ("sandbox-paths", Kind::List),
    ("secret-key-files", Kind::List),
    ("show-trace", Kind::Bool),
    ("ssl-cert-file", Kind::Value),
    ("stalled-download-timeout", Kind::Value),
    ("start-id", Kind::Value),
    ("store", Kind::Value),
    ("substitute", Kind::Bool),
    ("substituters", Kind::List),
    ("sync-before-registering", Kind::Bool),
    ("system", Kind::Value),
    ("system-features", Kind::List),
    ("tarball-ttl", Kind::Value),
    ("timeout", Kind::Value),
    ("trace-function-calls", Kind::Bool),
    ("trace-verbose", Kind::Bool),
    ("trust-tarballs-from-git-forges", Kind::Bool),
    ("trusted-public-keys", Kind::List),
    ("trusted-substituters", Kind::List),
    ("trusted-users", Kind::List),
    ("upgrade-nix-store-path-url", Kind::Value),
    ("use-case-hack", Kind::Bool),
    ("use-cgroups", Kind::Bool),
    ("use-registries", Kind::Bool),
    ("use-sqlite-wal", Kind::Bool),
    ("use-xdg-base-directories", Kind::Bool),
    ("user-agent-suffix", Kind::Value),
    ("warn-dirty", Kind::Bool),
    ("warn-large-path-threshold", Kind::Value),
];

/// Settings that change what an evaluation produces, which aetheryx does not
/// implement yet. The others have no effect on evaluation.
const AFFECT_EVALUATION: &[&str] = &[
    "abort-on-warn",
    "allow-unsafe-native-code-during-evaluation",
    "debugger-on-trace",
    "debugger-on-warn",
    "eval-system",
    "max-call-depth",
    "nix-path",
    "pure-eval",
    "restrict-eval",
    "system",
    // Not about evaluation, but they would need something aetheryx cannot
    // do: load plugins, use another store.
    "plugin-files",
    "store",
];

/// Nix 2.28.3's experimental features.
const EXPERIMENTAL_FEATURES: [&str; 20] = [
    "ca-derivations",
    "impure-derivations",
    "flakes",
    "fetch-tree",
    "nix-command",
    "git-hashing",
    "recursive-nix",
    "no-url-literals",
    "fetch-closure",
    "auto-allocate-uids",
    "cgroups",
    "daemon-trust-override",
    "dynamic-derivations",
    "parse-toml-timestamps",
    "read-only-local-store",
    "local-overlay-store",
    "configurable-impure-env",
    "mounted-ssh-store",
    "verified-fetches",
    "pipe-operators",
];

/// The type of a setting's value, which decides what values are valid, as
/// probed from `nix` 2.28.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueType {
    Any,
    Bool,
    /// `true`, `relaxed` or `false`.
    Sandbox,
    U32,
    U64,
    I64,
    /// `auto` or an unsigned 32-bit integer.
    MaxJobs,
    AbsolutePath,
    /// A list of experimental feature names.
    Features,
}

fn value_type(name: &str) -> ValueType {
    match name {
        "sandbox" => ValueType::Sandbox,
        "build-poll-interval"
        | "cores"
        | "download-attempts"
        | "download-speed"
        | "id-count"
        | "max-call-depth"
        | "max-substitution-jobs"
        | "narinfo-cache-negative-ttl"
        | "narinfo-cache-positive-ttl"
        | "start-id"
        | "tarball-ttl" => ValueType::U32,
        "connect-timeout"
        | "download-buffer-size"
        | "http-connections"
        | "log-lines"
        | "max-build-log-size"
        | "max-free"
        | "min-free"
        | "min-free-check-interval"
        | "nar-buffer-size"
        | "stalled-download-timeout"
        | "warn-large-path-threshold" => ValueType::U64,
        "gc-reserved-space" | "max-silent-time" | "timeout" => ValueType::I64,
        "max-jobs" => ValueType::MaxJobs,
        "diff-hook" => ValueType::AbsolutePath,
        "experimental-features" => ValueType::Features,
        _ if kind(name) == Some(Kind::Bool) => ValueType::Bool,
        _ => ValueType::Any,
    }
}

/// Why a setting's value is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidValue {
    pub message: String,
    /// Whether `nix` reports it as a usage error (followed by "Try 'nix
    /// --help'").
    pub usage: bool,
}

/// Checks the value of the setting `name` (possibly `extra-NAME`) as Nix
/// does; returns the warnings it gives for a valid value.
pub fn validate(name: &str, value: &str) -> Result<Vec<String>, InvalidValue> {
    let base = name.strip_prefix("extra-").unwrap_or(name);
    let invalid = |message: String, usage| Err(InvalidValue { message, usage });
    let not_integer = || {
        invalid(
            format!("setting '{name}' has invalid value '{value}'"),
            true,
        )
    };
    match value_type(base) {
        ValueType::Any => {}
        ValueType::Bool => {
            if !matches!(value, "true" | "yes" | "1" | "false" | "no" | "0") {
                return invalid(
                    format!("Boolean setting '{name}' has invalid value '{value}'"),
                    true,
                );
            }
        }
        ValueType::Sandbox => {
            if !matches!(value, "true" | "relaxed" | "false") {
                return invalid(format!("option '{name}' has invalid value '{value}'"), true);
            }
        }
        ValueType::U32 if value.parse::<u32>().is_err() => return not_integer(),
        ValueType::U64 if value.parse::<u64>().is_err() => return not_integer(),
        ValueType::I64 if value.parse::<i64>().is_err() => return not_integer(),
        ValueType::U32 | ValueType::U64 | ValueType::I64 => {}
        ValueType::MaxJobs => {
            if value != "auto" && value.parse::<u32>().is_err() {
                return invalid(
                    format!("configuration setting '{name}' should be 'auto' or an integer"),
                    true,
                );
            }
        }
        ValueType::AbsolutePath => {
            if !value.starts_with('/') {
                return invalid(format!("not an absolute path: '{value}'"), false);
            }
        }
        ValueType::Features => {
            return Ok(value
                .split_whitespace()
                .filter(|feature| !EXPERIMENTAL_FEATURES.contains(feature))
                .map(|feature| format!("unknown experimental feature '{feature}'"))
                .collect());
        }
    }
    Ok(Vec::new())
}

fn kind(name: &str) -> Option<Kind> {
    SETTINGS
        .iter()
        .find(|(setting, _)| *setting == name)
        .map(|&(_, kind)| kind)
}

/// A flag that sets a setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingFlag {
    /// `--NAME`, `--no-NAME`, `--relaxed-sandbox`: no argument.
    Fixed {
        setting: String,
        value: &'static str,
    },
    /// `--NAME VALUE`, `--extra-NAME VALUE` (where `setting` is
    /// `extra-NAME`).
    WithValue { setting: String },
}

/// The setting flag `--name`, if `name` is one.
pub fn flag(name: &str) -> Option<SettingFlag> {
    if name == "relaxed-sandbox" {
        return Some(SettingFlag::Fixed {
            setting: "sandbox".into(),
            value: "relaxed",
        });
    }
    match kind(name) {
        Some(Kind::Bool) => {
            return Some(SettingFlag::Fixed {
                setting: name.into(),
                value: "true",
            });
        }
        Some(Kind::Value | Kind::List) => {
            return Some(SettingFlag::WithValue {
                setting: name.into(),
            });
        }
        None => {}
    }
    if let Some(base) = name.strip_prefix("no-")
        && kind(base) == Some(Kind::Bool)
    {
        return Some(SettingFlag::Fixed {
            setting: base.into(),
            value: "false",
        });
    }
    if let Some(base) = name.strip_prefix("extra-")
        && kind(base) == Some(Kind::List)
    {
        return Some(SettingFlag::WithValue {
            setting: name.into(),
        });
    }
    None
}

/// What aetheryx does with a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// It has no effect on evaluation.
    Accepted,
    /// It would change the result: fail.
    Unsupported,
    /// Nix does not know it either: warn, as Nix does, and go on.
    Unknown,
}

/// What to do with the setting `name` (possibly `extra-NAME`).
pub fn verdict(name: &str) -> Verdict {
    let base = name
        .strip_prefix("extra-")
        .filter(|base| kind(base) == Some(Kind::List))
        .unwrap_or(name);
    if AFFECT_EVALUATION.contains(&base) {
        Verdict::Unsupported
    } else if kind(base).is_some() {
        Verdict::Accepted
    } else {
        Verdict::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_follow_the_kind_of_setting() {
        let fixed = |setting: &str, value| {
            Some(SettingFlag::Fixed {
                setting: setting.into(),
                value,
            })
        };
        let with_value = |setting: &str| {
            Some(SettingFlag::WithValue {
                setting: setting.into(),
            })
        };
        assert_eq!(flag("show-trace"), fixed("show-trace", "true"));
        assert_eq!(flag("no-sandbox"), fixed("sandbox", "false"));
        assert_eq!(flag("relaxed-sandbox"), fixed("sandbox", "relaxed"));
        assert_eq!(flag("max-jobs"), with_value("max-jobs"));
        assert_eq!(flag("extra-substituters"), with_value("extra-substituters"));
        // Only lists can be extended, only Booleans negated.
        assert_eq!(flag("extra-cores"), None);
        assert_eq!(flag("no-cores"), None);
        assert_eq!(flag(""), None);
        assert_eq!(flag("bogus"), None);
    }

    #[test]
    fn verdicts() {
        assert_eq!(verdict("cores"), Verdict::Accepted);
        assert_eq!(verdict("extra-substituters"), Verdict::Accepted);
        assert_eq!(verdict("extra-experimental-features"), Verdict::Accepted);
        assert_eq!(verdict("pure-eval"), Verdict::Unsupported);
        assert_eq!(verdict("extra-nix-path"), Verdict::Unsupported);
        assert_eq!(verdict("foo"), Verdict::Unknown);
        assert_eq!(verdict(""), Verdict::Unknown);
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn message(name: &str, value: &str) -> String {
        validate(name, value).unwrap_err().message
    }

    #[test]
    fn values_are_checked_like_nix() {
        // Each expected message is what `nix` 2.28.3 prints.
        assert_eq!(
            message("cores", "X"),
            "setting 'cores' has invalid value 'X'"
        );
        assert_eq!(
            message("cores", "-1"),
            "setting 'cores' has invalid value '-1'"
        );
        assert_eq!(
            message("cores", "4294967296"),
            "setting 'cores' has invalid value '4294967296'"
        );
        assert!(validate("cores", "+3").is_ok() && validate("cores", "007").is_ok());
        assert!(validate("log-lines", "4294967296").is_ok());
        assert!(validate("timeout", "-1").is_ok());
        assert_eq!(
            message("max-jobs", "-1"),
            "configuration setting 'max-jobs' should be 'auto' or an integer"
        );
        assert!(validate("max-jobs", "auto").is_ok());
        assert_eq!(message("diff-hook", "X"), "not an absolute path: 'X'");
        assert!(!validate("diff-hook", "X").unwrap_err().usage);
        assert_eq!(
            message("show-trace", "maybe"),
            "Boolean setting 'show-trace' has invalid value 'maybe'"
        );
        assert!(validate("show-trace", "yes").is_ok());
        assert_eq!(
            message("sandbox", "maybe"),
            "option 'sandbox' has invalid value 'maybe'"
        );
        assert!(validate("sandbox", "relaxed").is_ok());
        assert_eq!(
            validate("extra-experimental-features", "nix-command bogus-feature"),
            Ok(vec![
                "unknown experimental feature 'bogus-feature'".to_string()
            ])
        );
    }
}
