//! Nix's configuration settings. `nix` accepts them as `--option NAME
//! VALUE`, and as flags: `--NAME VALUE`, `--NAME` / `--no-NAME` for
//! Booleans, and `--extra-NAME VALUE` to add to a list.

use std::ffi::OsString;

/// What a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Value,
}

/// Nix 2.28.3's settings, as `nix config show` lists them, with whether
/// each is a Boolean (shown as `true` / `false`).
pub const SETTINGS: &[(&str, Kind)] = &[
    ("abort-on-warn", Kind::Bool),
    ("accept-flake-config", Kind::Bool),
    ("access-tokens", Kind::Value),
    ("allow-dirty", Kind::Bool),
    ("allow-dirty-locks", Kind::Bool),
    ("allow-import-from-derivation", Kind::Bool),
    ("allow-new-privileges", Kind::Bool),
    ("allow-symlinked-store", Kind::Bool),
    ("allow-unsafe-native-code-during-evaluation", Kind::Bool),
    ("allowed-impure-host-deps", Kind::Value),
    ("allowed-uris", Kind::Value),
    ("allowed-users", Kind::Value),
    ("always-allow-substitutes", Kind::Bool),
    ("auto-optimise-store", Kind::Bool),
    ("bash-prompt", Kind::Value),
    ("bash-prompt-prefix", Kind::Value),
    ("bash-prompt-suffix", Kind::Value),
    ("build-dir", Kind::Value),
    ("build-hook", Kind::Value),
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
    ("experimental-features", Kind::Value),
    ("extra-platforms", Kind::Value),
    ("fallback", Kind::Bool),
    ("filter-syscalls", Kind::Bool),
    ("flake-registry", Kind::Value),
    ("fsync-metadata", Kind::Bool),
    ("fsync-store-paths", Kind::Bool),
    ("gc-reserved-space", Kind::Value),
    ("hashed-mirrors", Kind::Value),
    ("http-connections", Kind::Value),
    ("http2", Kind::Bool),
    ("id-count", Kind::Value),
    ("ignore-try", Kind::Bool),
    ("ignored-acls", Kind::Value),
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
    ("nix-path", Kind::Value),
    ("nix-shell-always-looks-for-shell-nix", Kind::Bool),
    ("nix-shell-shebang-arguments-relative-to-script", Kind::Bool),
    ("plugin-files", Kind::Value),
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
    ("sandbox-paths", Kind::Value),
    ("secret-key-files", Kind::Value),
    ("show-trace", Kind::Bool),
    ("ssl-cert-file", Kind::Value),
    ("stalled-download-timeout", Kind::Value),
    ("start-id", Kind::Value),
    ("store", Kind::Value),
    ("substitute", Kind::Bool),
    ("substituters", Kind::Value),
    ("sync-before-registering", Kind::Bool),
    ("system", Kind::Value),
    ("system-features", Kind::Value),
    ("tarball-ttl", Kind::Value),
    ("timeout", Kind::Value),
    ("trace-function-calls", Kind::Bool),
    ("trace-verbose", Kind::Bool),
    ("trust-tarballs-from-git-forges", Kind::Bool),
    ("trusted-public-keys", Kind::Value),
    ("trusted-substituters", Kind::Value),
    ("trusted-users", Kind::Value),
    ("upgrade-nix-store-path-url", Kind::Value),
    ("use-case-hack", Kind::Bool),
    ("use-cgroups", Kind::Bool),
    ("use-registries", Kind::Bool),
    ("use-sqlite-wal", Kind::Bool),
    ("use-xdg-base-directories", Kind::Bool),
    ("user-agent-suffix", Kind::Value),
    ("warn-dirty", Kind::Bool),
    ("warn-large-path-threshold", Kind::Value),
    ("", Kind::Value),
];

/// Settings that change what an evaluation produces, which aetheryx does not
/// implement yet. Any other setting has no effect on it.
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
];

fn kind(name: &str) -> Option<Kind> {
    SETTINGS
        .iter()
        .find(|(setting, _)| *setting == name)
        .map(|&(_, kind)| kind)
}

/// Rewrites setting flags in `args` into `--option NAME VALUE`, as `nix`
/// interprets them; other arguments are left as they are.
pub fn expand_flags(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut out = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let Some(name) = arg.to_str().and_then(|a| a.strip_prefix("--")) else {
            out.push(arg);
            continue;
        };
        let setting = match (
            kind(name),
            name.strip_prefix("no-"),
            name.strip_prefix("extra-"),
        ) {
            (Some(Kind::Bool), _, _) => Some((name.to_string(), Some("true".into()))),
            (Some(Kind::Value), _, _) => Some((name.to_string(), None)),
            (None, Some(base), _) if kind(base) == Some(Kind::Bool) => {
                Some((base.to_string(), Some("false".into())))
            }
            (None, _, Some(base)) if kind(base) == Some(Kind::Value) => {
                Some((name.to_string(), None))
            }
            _ => None,
        };
        match setting {
            Some((name, value)) => {
                out.push("--option".into());
                out.push(name.into());
                // A missing value is left for the parser to report.
                if let Some(value) = value.or_else(|| args.next()) {
                    out.push(value);
                }
            }
            None => out.push(arg),
        }
    }
    out
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

/// What to do with `--option NAME ...`.
pub fn verdict(name: &str) -> Verdict {
    let base = name
        .strip_prefix("extra-")
        .filter(|base| kind(base).is_some())
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

    fn expand(args: &[&str]) -> Vec<String> {
        expand_flags(args.iter().map(OsString::from))
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    #[test]
    fn setting_flags_become_options() {
        assert_eq!(
            expand(&[
                "eval",
                "--show-trace",
                "--max-jobs",
                "2",
                "--no-sandbox",
                "--expr",
                "1"
            ]),
            [
                "eval",
                "--option",
                "show-trace",
                "true",
                "--option",
                "max-jobs",
                "2",
                "--option",
                "sandbox",
                "false",
                "--expr",
                "1"
            ]
        );
        assert_eq!(
            expand(&["--extra-substituters", "https://x"]),
            ["--option", "extra-substituters", "https://x"]
        );
        // Not settings.
        assert_eq!(expand(&["--expr", "--bogus"]), ["--expr", "--bogus"]);
    }

    #[test]
    fn verdicts() {
        assert_eq!(verdict("cores"), Verdict::Accepted);
        assert_eq!(verdict("extra-substituters"), Verdict::Accepted);
        assert_eq!(verdict("experimental-features"), Verdict::Accepted);
        assert_eq!(verdict("pure-eval"), Verdict::Unsupported);
        assert_eq!(verdict("extra-nix-path"), Verdict::Unsupported);
        assert_eq!(verdict("foo"), Verdict::Unknown);
    }
}
