//! `simforge keys create|list|revoke`: the workspace's `sfk_` API keys (`/api/v1/api-keys`).
//!
//! A key acts only in the workspace it was created in, with exactly the scopes it was
//! created with (never more than the creating credential holds, never `runtime:read`).
//! Managing keys needs the `keys:admin` scope: a Studio session has it, a CLI login only
//! when it asked for it (`simforge login --scope keys:admin ...`).

use clap::{Args, Subcommand};
use serde_json::json;

use super::cloud::{seg, Client};
use crate::contract::{CliError, CmdResult, Outcome};

/// What a key gets when `--scope` is not given: everything a `--cloud` run needs, but not
/// the right to mint more keys.
pub const DEFAULT_KEY_SCOPES: &[&str] = &[
    "maps:read",
    "scenarios:read",
    "scenarios:write",
    "datasets:read",
    "runs:read",
    "runs:write",
    "artifacts:read",
    "usage:read",
];

#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// Create an API key for this workspace; its secret is printed once.
    Create(CreateArgs),
    /// List the workspace's API keys (never their secrets).
    List,
    /// Revoke an API key now; requests with it stop resolving immediately.
    Revoke(RevokeArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// A name to recognize the key by (1-200 characters).
    #[arg(long)]
    pub name: String,
    /// A scope the key holds (repeatable). Default: maps:read scenarios:read scenarios:write
    /// datasets:read runs:read runs:write artifacts:read usage:read.
    #[arg(long = "scope", value_name = "SCOPE")]
    pub scopes: Vec<String>,
    /// Expire the key after this many days (1-3650). Default: no expiry.
    #[arg(long, value_name = "DAYS", value_parser = clap::value_parser!(u32).range(1..=3650))]
    pub expires_in_days: Option<u32>,
}

#[derive(Debug, Args)]
pub struct RevokeArgs {
    /// The key's id (`key_…`, from `simforge keys list`).
    pub id: String,
}

fn create_body(a: &CreateArgs) -> serde_json::Value {
    let scopes: Vec<&str> = if a.scopes.is_empty() {
        DEFAULT_KEY_SCOPES.to_vec()
    } else {
        a.scopes.iter().map(String::as_str).collect()
    };
    let mut body = json!({ "name": a.name, "scopes": scopes });
    if let Some(days) = a.expires_in_days {
        body["expiresInDays"] = json!(days);
    }
    body
}

pub fn run(c: &Client, command: KeysCommand) -> CmdResult {
    match command {
        KeysCommand::Create(a) => {
            let mut created = c.post("/api-keys", &create_body(&a))?;
            created["note"] = json!("The secret is shown only now: store it (e.g. as SIMFORGE_API_KEY with SIMFORGE_API_URL) before closing this output.");
            Ok(Outcome::ok(created))
        }
        KeysCommand::List => Ok(Outcome::ok(c.get("/api-keys")?)),
        KeysCommand::Revoke(a) => {
            if !a.id.starts_with("key_") {
                return Err(CliError::new(
                    "bad_value",
                    "an API key id starts with key_ (see simforge keys list)",
                )
                .with_path("ID"));
            }
            c.delete(&format!("/api-keys/{}", seg(&a.id)))?;
            Ok(Outcome::ok(json!({ "id": a.id, "revoked": true })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_defaults_to_run_scopes_without_key_admin() {
        let body = create_body(&CreateArgs {
            name: "ci".into(),
            scopes: vec![],
            expires_in_days: None,
        });
        let scopes: Vec<&str> = body["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        assert!(scopes.contains(&"runs:write"));
        assert!(!scopes.contains(&"keys:admin"));
        assert!(!scopes.contains(&"runtime:read"));
        assert!(body.get("expiresInDays").is_none());
    }

    #[test]
    fn explicit_scopes_and_expiry_are_sent_as_given() {
        let body = create_body(&CreateArgs {
            name: "read".into(),
            scopes: vec!["runs:read".into()],
            expires_in_days: Some(30),
        });
        assert_eq!(
            body,
            json!({"name":"read","scopes":["runs:read"],"expiresInDays":30})
        );
    }
}
