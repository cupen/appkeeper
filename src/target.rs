//! The unified management target grammar (add-standalone-programs D6):
//!
//! ```text
//! <target> := <program> | <app>/<program> | <app>/all
//! ```
//!
//! A single token is always a **program** name (apps are only addressed
//! through `<app>/all`); `all` alone is the reserved keyword and a usage
//! error. `<app> <program>` (two positionals) is the legacy alias of
//! `<app>/<program>`.
//!
//! The parser is pure and lives here so the CLI (main.rs, shell.rs) and any
//! future control-plane caller share one spelling of the grammar and one set
//! of error messages.

use std::fmt;

/// A parsed management target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// One globally unique program (app-internal or standalone).
    Program(String),
    /// One program of a named app.
    AppProgram { app: String, program: String },
    /// The whole app (fan-out / app-scoped action).
    AppAll(String),
}

/// A usage error (exit code 2 at the CLI boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetError(pub String);

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TargetError {}

/// Parse `<first> [<second>]` per D6.
///
/// Errors are usage errors: empty segment, more than one `/`, an empty
/// value, or the reserved bare `all`.
pub fn parse(first: &str, second: Option<&str>) -> Result<Target, TargetError> {
    if let Some(program) = second {
        if first.contains('/') {
            return Err(TargetError(format!(
                "target {first:?} {program:?} is ambiguous: use either \
                 `<app>/<program>` or `<app> <program>`, not both"
            )));
        }
        if first.is_empty() || program.is_empty() {
            return Err(TargetError(
                "target segments must not be empty; use `<program>`, `<app>/<program>` \
                 or `<app>/all`"
                    .into(),
            ));
        }
        if program == "all" {
            return Err(TargetError(
                "`<app> all` is not a target; the whole app is `<app>/all`".into(),
            ));
        }
        return Ok(Target::AppProgram {
            app: first.to_string(),
            program: program.to_string(),
        });
    }
    if first.is_empty() {
        return Err(TargetError(
            "a target must not be empty; use `<program>`, `<app>/<program>` or `<app>/all`".into(),
        ));
    }
    if !first.contains('/') {
        if first == "all" {
            return Err(TargetError(
                "`all` alone is reserved; the whole app is `<app>/all` and a single \
                 token is a program name"
                    .into(),
            ));
        }
        return Ok(Target::Program(first.to_string()));
    }
    let parts: Vec<&str> = first.split('/').collect();
    if parts.len() != 2 {
        return Err(TargetError(format!(
            "target {first:?} has too many `/`; use `<app>/<program>` or `<app>/all`"
        )));
    }
    let (app, program) = (parts[0], parts[1]);
    if app.is_empty() || program.is_empty() {
        return Err(TargetError(format!(
            "target {first:?} has an empty segment; use `<app>/<program>` or `<app>/all`"
        )));
    }
    if program == "all" {
        return Ok(Target::AppAll(app.to_string()));
    }
    Ok(Target::AppProgram {
        app: app.to_string(),
        program: program.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_token_is_a_program() {
        assert_eq!(parse("api", None).unwrap(), Target::Program("api".into()));
        // A name that happens to equal an app is still a program.
        assert_eq!(
            parse("gateway", None).unwrap(),
            Target::Program("gateway".into())
        );
    }

    #[test]
    fn slash_forms_split_app_and_program() {
        assert_eq!(
            parse("gateway/api", None).unwrap(),
            Target::AppProgram {
                app: "gateway".into(),
                program: "api".into(),
            }
        );
        assert_eq!(
            parse("gateway/all", None).unwrap(),
            Target::AppAll("gateway".into())
        );
    }

    #[test]
    fn two_positionals_are_the_legacy_alias() {
        assert_eq!(
            parse("gateway", Some("api")).unwrap(),
            Target::AppProgram {
                app: "gateway".into(),
                program: "api".into(),
            }
        );
        assert_eq!(
            parse("gateway", Some("all")).unwrap_err(),
            TargetError("`<app> all` is not a target; the whole app is `<app>/all`".into())
        );
    }

    #[test]
    fn malformed_forms_are_usage_errors() {
        for bad in ["", "/api", "gateway/", "a/b/c", "all"] {
            assert!(parse(bad, None).is_err(), "{bad:?} must be rejected");
        }
        assert!(parse("gateway/api", Some("extra")).is_err());
        assert!(parse("", Some("api")).is_err());
        assert!(parse("gateway", Some("")).is_err());
    }
}
