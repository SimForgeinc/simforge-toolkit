//! What a refused cloud request means for the person running the command.
//!
//! The host's `402 budget_exceeded` names which limit refused the run in `detail.limit`
//! (`max_cost` or `daily_cap`, docs/cloud-api.md "Billing and usage"); the CLI keeps the code
//! and the host's detail and says, in dollars, what to do next. A workspace short of credit is
//! `402 insufficient_credits`, whose reason and hint (the credits page) the host writes itself.

use serde_json::{json, Value};

use crate::contract::CliError;

/// The scopes `simforge login` requests when no `--scope` is given.
pub const DEFAULT_LOGIN_SCOPES: &[&str] = &[
    "scenarios:read",
    "scenarios:write",
    "maps:read",
    "runtime:read",
    "datasets:read",
    "runs:read",
    "runs:write",
    "artifacts:read",
    "usage:read",
];

/// `1234` cents as `$12.34`.
pub fn dollars(cents: u64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

fn cents(detail: &Value, key: &str) -> Option<u64> {
    detail.get(key).and_then(Value::as_u64)
}

/// Studio's workspace settings page `view` on `origin` (e.g. `https://simforge.ai`).
pub fn settings_url(origin: &str, view: &str) -> String {
    format!(
        "{}/dashboard/workspace/settings?view={view}",
        origin.trim_end_matches('/')
    )
}

/// The reason and hint for a `budget_exceeded` refusal, from its `detail`.
fn budget(detail: &Value) -> Option<(String, String)> {
    match detail.get("limit").and_then(Value::as_str)? {
        "max_cost" => {
            let estimate = cents(detail, "costEstimateCents")?;
            let cap = cents(detail, "limitCents")?;
            Some((
                format!(
                    "Estimated cost {} exceeds --max-cost {cap} ({}).",
                    dollars(estimate),
                    dollars(cap)
                ),
                format!("Rerun with --max-cost {estimate}, or with --dry-run to see the per-job breakdown."),
            ))
        }
        "daily_cap" => {
            let cap = cents(detail, "limitCents")?;
            let used = cents(detail, "usedCents")?;
            let requested = cents(detail, "requestedCents")?;
            Some((
                format!(
                    "Workspace daily spending cap {} reached ({} used today, this run needs {}).",
                    dollars(cap),
                    dollars(used),
                    dollars(requested)
                ),
                "The cap resets at 00:00 UTC; ask SimForge support to raise it.".to_owned(),
            ))
        }
        _ => None,
    }
}

/// A host refusal made actionable: budget refusals get their limit in dollars and the next
/// step, and a missing `keys:admin` scope names the login that grants it. The code and the
/// host's detail are kept; anything else passes through unchanged. `origin` is the host's
/// Studio origin, for the pages a hint links to.
pub fn refine(error: CliError, origin: &str) -> CliError {
    let Some(detail) = error.detail.clone() else {
        return error;
    };
    match error.code.as_str() {
        "budget_exceeded" => match budget(&detail) {
            Some((reason, hint)) => {
                let mut detail = detail;
                detail["hint"] = json!(hint);
                CliError {
                    reason,
                    detail: Some(detail),
                    ..error
                }
            }
            None => error,
        },
        "insufficient_scope"
            if detail
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|r| r.iter().any(|s| s == "keys:admin")) =>
        {
            let scopes: Vec<String> = DEFAULT_LOGIN_SCOPES
                .iter()
                .chain(&["keys:admin"])
                .map(|s| format!("--scope {s}"))
                .collect();
            let mut detail = detail;
            detail["hint"] = json!(format!(
                "Managing API keys needs the keys:admin scope: simforge login {}, or manage them at {}",
                scopes.join(" "),
                settings_url(origin, "api-keys")
            ));
            CliError {
                detail: Some(detail),
                ..error
            }
        }
        _ => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(detail: Value) -> CliError {
        refine(
            CliError::findings("budget_exceeded", "host reason").with_detail(detail),
            "https://dev.simforge.ai/",
        )
    }

    #[test]
    fn a_run_over_its_cap_names_the_estimate_and_the_rerun() {
        let e = refused(json!({"limit":"max_cost","limitCents":10,"costEstimateCents":34}));
        assert_eq!(e.code, "budget_exceeded");
        assert_eq!(
            e.reason,
            "Estimated cost $0.34 exceeds --max-cost 10 ($0.10)."
        );
        let out = e.to_json();
        assert_eq!(
            out["hint"],
            "Rerun with --max-cost 34, or with --dry-run to see the per-job breakdown."
        );
        assert_eq!(out["detail"]["limit"], "max_cost");
        assert_eq!(out["detail"]["costEstimateCents"], 34);
    }

    #[test]
    fn the_daily_cap_is_shown_in_dollars() {
        let cap = refused(
            json!({"limit":"daily_cap","limitCents":1000,"usedCents":990,"requestedCents":35}),
        );
        assert_eq!(
            cap.reason,
            "Workspace daily spending cap $10.00 reached ($9.90 used today, this run needs $0.35)."
        );
        assert!(cap.to_json()["hint"]
            .as_str()
            .unwrap()
            .contains("00:00 UTC"));
    }

    #[test]
    fn an_unknown_limit_keeps_the_hosts_reason() {
        let e = refused(json!({"limit":"something_new"}));
        assert_eq!(e.reason, "host reason");
    }

    #[test]
    fn a_missing_keys_admin_scope_names_the_login_that_grants_it() {
        let e = refine(
            CliError::findings("insufficient_scope", "no")
                .with_detail(json!({"required":["keys:admin"]})),
            "https://simforge.ai",
        );
        let hint = e.to_json()["hint"].as_str().unwrap().to_owned();
        assert!(hint.contains("--scope keys:admin"), "{hint}");
        assert!(hint.contains("--scope runs:write"), "{hint}");
        assert!(
            hint.contains("https://simforge.ai/dashboard/workspace/settings?view=api-keys"),
            "{hint}"
        );
        let other = refine(
            CliError::findings("insufficient_scope", "no")
                .with_detail(json!({"required":["runs:write"]})),
            "https://simforge.ai",
        );
        assert!(other.to_json()["hint"]
            .as_str()
            .unwrap()
            .starts_with("Run simforge login"));
    }

    #[test]
    fn dollars_pad_cents() {
        assert_eq!(dollars(0), "$0.00");
        assert_eq!(dollars(5), "$0.05");
        assert_eq!(dollars(12345), "$123.45");
    }
}
