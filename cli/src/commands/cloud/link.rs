// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use cling::prelude::*;
use serde_json::json;

use restate_cli_util::{CliContext, c_eprintln, c_print};

use crate::cli_env::{CliEnv, EnvironmentType};
use crate::clients::cloud::types::{CreateApiKeyRequest, DescribeEnvironmentResponse};
use crate::clients::cloud::{CloudClient, CloudClientInterface};
use crate::error::{ErrorKind, RestateCliError};
use crate::ui::fmt::{Field, Formatter, IncludeFormatting, Outcome, OutputFormatter};

use super::profile::{Choice, EnvironmentSpecifier, choose_environment, with_progress};

/// The role of the key `restate cloud link` mints. Full access until the Connect role
/// (tunnel + invoke, no admin) exists; then `rst:role::ConnectAccess`.
const LINK_KEY_ROLE: &str = "rst:role::FullAccess";

/// The variables the file defines, in order. All pre-existing names: what the SDK's
/// `serve()` and `clients.connect()` read.
const ENVIRONMENT_ID: &str = "RESTATE_ENVIRONMENT_ID";
const AUTH_TOKEN: &str = "RESTATE_AUTH_TOKEN";
const CLOUD_REGION: &str = "RESTATE_CLOUD_REGION";
const SIGNING_PUBLIC_KEY: &str = "RESTATE_SIGNING_PUBLIC_KEY";
const INGRESS_URL: &str = "RESTATE_INGRESS_URL";
const TUNNEL_NAME: &str = "RESTATE_TUNNEL_NAME";

/// Link this project to a Restate Cloud environment: mint a key, write `.env`
///
/// Writes the connection settings of the current Cloud environment (or --env) to `.env` in
/// the current directory: a new API key, the environment id, region and signing key, the
/// ingress URL, and the deployment name. A service started in this directory with the
/// SDK's `serve()` then connects to the environment, with no code change, and prints how
/// to register it (`restate deployments register tunnel://<deployment name>`);
/// `clients.connect()` reaches the environment's ingress. The file's other lines are
/// kept; its `RESTATE_*` lines are replaced.
///
/// Needs `restate cloud login`, and a Cloud environment as the current one (`restate env
/// add`) or named with --env. The key has full access for now: keep the file out of git.
/// The CLI itself never reads this file.
#[derive(Run, Parser, Collect, Clone)]
#[cling(run = "run_link")]
#[command(after_help = after_help!(
    examples: [
        "restate cloud link",
        "restate cloud link --env my-account/staging --name greeter-dev",
        "restate cloud link --print > .env.staging    # e.g. to store as a CI secret",
    ],
    learn_more: "https://docs.restate.dev/cloud/getting-started",
))]
pub struct Link {
    /// The Cloud environment to link to, `[ACCOUNT/]ENVIRONMENT` (names or ids). Default:
    /// the current environment, which must be a Cloud one
    #[clap(long, value_name = "ENVIRONMENT")]
    env: Option<EnvironmentSpecifier>,

    /// The deployment name, written as RESTATE_TUNNEL_NAME: how the environment knows this
    /// service, and what `restate deployments register tunnel://<name>` takes. Letters,
    /// digits, `.`, `_` and `-`. Default: `<directory>-<hostname>`
    #[clap(long)]
    name: Option<String>,

    /// The file to write
    #[clap(long, short, default_value = ".env", value_name = "PATH")]
    output: PathBuf,

    /// Print the file on stdout instead of writing it (e.g. to store it as a CI secret)
    #[clap(long, conflicts_with = "output")]
    print: bool,

    /// Description of the API key, shown in the Cloud UI. Default: `connect from
    /// <hostname> on <date>`
    #[clap(long)]
    description: Option<String>,

    /// Mint a new key even if the file already has one for this environment (the old key
    /// stays valid until deleted in the Cloud UI)
    #[clap(long)]
    force: bool,
}

pub async fn run_link(State(env): State<CliEnv>, opts: &Link) -> Result<()> {
    if env.config.cloud.credentials.is_none() {
        return Err(
            RestateCliError::new(ErrorKind::Auth, "Not logged in to Restate Cloud")
                .with_next_step("restate cloud login", "log in, then link the project")
                .into(),
        );
    }
    let client = CloudClient::new(&env)?;
    let (account_id, environment) = target_environment(&env, &client, opts.env.as_ref()).await?;

    let deployment_name = match &opts.name {
        Some(name) => validate_deployment_name(name)?,
        None => default_deployment_name(),
    };

    let existing = if opts.print {
        None
    } else {
        read_existing(&opts.output)?
    };
    if let Some(content) = &existing
        && !opts.force
        && has_key_for(content, &environment.environment_id)
    {
        return Err(RestateCliError::bad_input(format!(
            "{} already has a key for environment {} ({}); each run mints a new key",
            opts.output.display(),
            environment.name,
            environment.environment_id
        ))
        .with_next_step(
            format!("{} --force", crate::ui::fmt::rerun_command()),
            "mint a new key anyway",
        )
        .into());
    }

    let description = opts
        .description
        .clone()
        .unwrap_or_else(|| format!("connect from {} on {}", hostname(), today()));
    let key = with_progress("Creating API key...", async {
        client
            .create_api_key(
                &account_id,
                CreateApiKeyRequest {
                    environment_id: environment.environment_id.clone(),
                    role_id: LINK_KEY_ROLE.to_owned(),
                    description: Some(description.clone()),
                },
            )
            .await?
            .into_body()
            .await
    })
    .await?;

    let values = [
        (ENVIRONMENT_ID, environment.environment_id.as_str()),
        (AUTH_TOKEN, key.api_key.as_str()),
        (CLOUD_REGION, environment.region.as_str()),
        (SIGNING_PUBLIC_KEY, environment.signing_public_key.as_str()),
        (INGRESS_URL, environment.ingress_base_url.as_str()),
        (TUNNEL_NAME, deployment_name.as_str()),
    ];
    let content = render_env_file(existing.as_deref(), &environment.name, &values);

    let environment_line = format!(
        "Environment: {} ({}, {})",
        environment.name, environment.environment_id, environment.region
    );
    let api_key_line = format!(
        "Created API key \"{description}\" ({})",
        role_label(&key.role_id)
    );
    if opts.print && !CliContext::get().json_output() {
        // The file is the output, alone on stdout so it can be redirected.
        c_eprintln!("{environment_line}");
        c_eprintln!("{api_key_line}");
        c_print!("{content}");
        return Ok(());
    }

    let mut f = Formatter::new();
    f.outcome(
        "environment",
        Field::with_display(
            json!({
                "name": environment.name,
                "environment_id": environment.environment_id,
                "region": environment.region,
                "account_id": account_id,
            }),
            environment_line,
        ),
        Outcome::Success,
    );
    f.outcome(
        "api_key",
        Field::with_display(
            json!({
                "key_id": key.key_id,
                "description": description,
                "role_id": key.role_id,
            }),
            api_key_line,
        ),
        Outcome::Success,
    );
    if opts.print {
        f.value("env_file", Field::json_only(json!({ "content": content })));
        return f.finish();
    }

    std::fs::write(&opts.output, &content)
        .with_context(|| format!("Failed to write {}", opts.output.display()))?;
    let path = opts.output.display().to_string();
    let variables: Vec<&str> = values.iter().map(|(name, _)| *name).collect();
    f.outcome(
        "env_file",
        Field::with_display(
            json!({
                "path": path,
                "variables": variables,
                "deployment_name": deployment_name,
                "existing_lines_kept": existing.is_some(),
            }),
            format!(
                "Wrote {path} ({} variables, deployment name {deployment_name}{})",
                variables.len(),
                if existing.is_some() {
                    "; existing lines preserved"
                } else {
                    ""
                }
            ),
        ),
        Outcome::Success,
    );
    match git_ignored(&opts.output) {
        Some(true) => f.outcome(
            "git_ignored",
            Field::with_display(true, format!("{path} is ignored by git")),
            Outcome::Success,
        ),
        Some(false) => {
            f.value("git_ignored", Field::json_only(false));
            f.warning(&format!(
                "{path} is not ignored by git, and it holds a key: run `echo {path} >> .gitignore`"
            ));
        }
        None => f.value("git_ignored", Field::json_only(serde_json::Value::Null)),
    }
    f.next_step(
        "npm run dev",
        "start your service: `serve()` reads the file and connects to the environment",
        IncludeFormatting::No,
    );
    f.next_step(
        &format!("restate deployments register tunnel://{deployment_name}"),
        "register the service once it is running",
        IncludeFormatting::Yes,
    );
    f.finish()
}

/// The environment to link: `--env`, else the current (Cloud) environment.
async fn target_environment(
    env: &CliEnv,
    client: &CloudClient,
    specifier: Option<&EnvironmentSpecifier>,
) -> Result<(String, DescribeEnvironmentResponse)> {
    if let Some(specifier) = specifier {
        return match choose_environment(client, Some(specifier), false).await? {
            Choice::Environment(chosen) => Ok((chosen.account_id, chosen.environment)),
            Choice::NoAccounts => Err(RestateCliError::not_found(
                "No accounts set up; use the Restate Cloud UI at https://cloud.restate.dev to create your first account",
            )
            .into()),
            Choice::NoEnvironments { account_name } => Err(RestateCliError::not_found(
                format!("No environments in account {account_name}"),
            )
            .into()),
            Choice::Ambiguous => Err(RestateCliError::bad_input(
                "You have several accounts: pass --env ACCOUNT/ENVIRONMENT",
            )
            .into()),
        };
    }
    let (EnvironmentType::Cloud, Some(info)) = (
        &env.config.environment_type,
        &env.config.cloud.environment_info,
    ) else {
        return Err(RestateCliError::bad_input(format!(
            "The current environment \"{}\" is not a Restate Cloud one",
            env.environment
        ))
        .with_next_step("restate env add", "select a Cloud environment")
        .with_next_step(
            "restate cloud link --env ACCOUNT/ENVIRONMENT",
            "link to a Cloud environment without selecting it",
        )
        .into());
    };
    let environment = with_progress("Describing environment...", async {
        client
            .describe_environment(&info.account_id, &info.environment_id)
            .await?
            .into_body()
            .await
    })
    .await?;
    Ok((info.account_id.clone(), environment))
}

fn role_label(role_id: &str) -> &'static str {
    match role_id {
        "rst:role::FullAccess" => "full access for now; the Connect role once it exists",
        "rst:role::ConnectAccess" => "Connect role: tunnel and invoke, no admin",
        _ => "see the Cloud UI for its role",
    }
}

fn hostname() -> String {
    let host = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default();
    host.split('.').next().unwrap_or_default().to_owned()
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// `<directory>-<hostname>`, sanitised to what the tunnel server accepts.
fn default_deployment_name() -> String {
    let dir = std::env::current_dir()
        .ok()
        .and_then(|dir| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "service".to_owned());
    let host = hostname();
    let raw = if host.is_empty() {
        dir
    } else {
        format!("{dir}-{host}")
    };
    sanitize_deployment_name(&raw)
}

fn is_deployment_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Lowercase, every other character replaced by `-`, no leading, trailing or doubled `-`.
fn sanitize_deployment_name(raw: &str) -> String {
    let mut name = String::with_capacity(raw.len());
    for c in raw.chars().flat_map(char::to_lowercase) {
        if is_deployment_name_char(c) {
            name.push(c);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches('-');
    if name.is_empty() {
        "service".to_owned()
    } else {
        name.to_owned()
    }
}

fn validate_deployment_name(name: &str) -> Result<String> {
    if name.is_empty() || !name.chars().all(is_deployment_name_char) {
        return Err(RestateCliError::bad_input(format!(
            "Invalid deployment name \"{name}\": use letters, digits, `.`, `_` and `-`"
        ))
        .into());
    }
    Ok(name.to_owned())
}

fn read_existing(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(path)
        .map(Some)
        .with_context(|| format!("Failed to read {}", path.display()))
}

/// The variable name a dotenv line defines, if any.
fn dotenv_key(line: &str) -> Option<&str> {
    let line = line.trim_start().trim_start_matches("export ").trim_start();
    if line.starts_with('#') {
        return None;
    }
    line.split_once('=').map(|(key, _)| key.trim_end())
}

/// Whether the file already holds a key for `environment_id`.
fn has_key_for(content: &str, environment_id: &str) -> bool {
    let value_of = |name: &str| {
        content
            .lines()
            .find(|line| dotenv_key(line) == Some(name))
            .and_then(|line| line.split_once('='))
            .map(|(_, value)| value.trim())
    };
    value_of(ENVIRONMENT_ID) == Some(environment_id)
        && value_of(AUTH_TOKEN).is_some_and(|token| !token.is_empty())
}

/// The file: `existing` with each of `values` replacing the line that defined it, the
/// rest appended under the header. Other lines are kept as they are.
fn render_env_file(
    existing: Option<&str>,
    environment_name: &str,
    values: &[(&str, &str); 6],
) -> String {
    let header = format!(
        "# Restate Cloud — environment \"{environment_name}\". Generated by `restate cloud link`. Keep out of git."
    );
    let mut lines: Vec<String> = Vec::new();
    let mut pending: Vec<(&str, &str)> = values.to_vec();
    for line in existing.unwrap_or_default().lines() {
        let key = dotenv_key(line);
        if let Some(i) = pending.iter().position(|(name, _)| Some(*name) == key) {
            let (name, value) = pending.remove(i);
            lines.push(format!("{name}={value}"));
        } else if key.is_some_and(|key| values.iter().any(|(name, _)| *name == key)) {
            // A duplicate definition of a variable already replaced above.
        } else if line.starts_with("# Restate Cloud — environment ") && !pending.is_empty() {
            lines.push(header.clone());
        } else {
            lines.push(line.to_owned());
        }
    }
    if !pending.is_empty() {
        if !lines.contains(&header) {
            if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(header);
        }
        for (name, value) in pending {
            lines.push(format!("{name}={value}"));
        }
    }
    let mut content = lines.join("\n");
    content.push('\n');
    content
}

/// Whether git ignores `path`: `None` outside a git repository, or without git.
fn git_ignored(path: &Path) -> Option<bool> {
    let status = Command::new("git")
        .args(["check-ignore", "-q", "--"])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUES: [(&str, &str); 6] = [
        (ENVIRONMENT_ID, "env_1"),
        (AUTH_TOKEN, "key_1.secret"),
        (CLOUD_REGION, "eu"),
        (SIGNING_PUBLIC_KEY, "publickeyv1_x"),
        (INGRESS_URL, "https://1.env.eu.restate.cloud:8080"),
        (TUNNEL_NAME, "greeter-laptop"),
    ];

    #[test]
    fn renders_a_new_file_in_the_contract_shape() {
        assert_eq!(
            render_env_file(None, "my-env", &VALUES),
            "# Restate Cloud — environment \"my-env\". Generated by `restate cloud link`. Keep out of git.\n\
             RESTATE_ENVIRONMENT_ID=env_1\n\
             RESTATE_AUTH_TOKEN=key_1.secret\n\
             RESTATE_CLOUD_REGION=eu\n\
             RESTATE_SIGNING_PUBLIC_KEY=publickeyv1_x\n\
             RESTATE_INGRESS_URL=https://1.env.eu.restate.cloud:8080\n\
             RESTATE_TUNNEL_NAME=greeter-laptop\n"
        );
    }

    #[test]
    fn merges_into_an_existing_file() {
        let existing = "DATABASE_URL=postgres://x\nexport RESTATE_AUTH_TOKEN=old\nRESTATE_AUTH_TOKEN=older\n# keep me\n";
        let content = render_env_file(Some(existing), "my-env", &VALUES);
        assert_eq!(
            content,
            "DATABASE_URL=postgres://x\n\
             RESTATE_AUTH_TOKEN=key_1.secret\n\
             # keep me\n\
             \n\
             # Restate Cloud — environment \"my-env\". Generated by `restate cloud link`. Keep out of git.\n\
             RESTATE_ENVIRONMENT_ID=env_1\n\
             RESTATE_CLOUD_REGION=eu\n\
             RESTATE_SIGNING_PUBLIC_KEY=publickeyv1_x\n\
             RESTATE_INGRESS_URL=https://1.env.eu.restate.cloud:8080\n\
             RESTATE_TUNNEL_NAME=greeter-laptop\n"
        );
        assert!(has_key_for(&content, "env_1"));
        assert!(!has_key_for(&content, "env_2"));
        // Linking again replaces every line in place, header included.
        let again = render_env_file(Some(&content), "other-env", &VALUES);
        assert_eq!(again.matches("RESTATE_AUTH_TOKEN=").count(), 1);
        assert!(again.contains("environment \"other-env\""));
        assert!(!again.contains("environment \"my-env\""));
    }

    #[test]
    fn deployment_names_are_sanitised_or_validated() {
        assert_eq!(
            sanitize_deployment_name("My Project-Francesco's MBP"),
            "my-project-francesco-s-mbp"
        );
        assert_eq!(sanitize_deployment_name("--/--"), "service");
        assert!(validate_deployment_name("greeter_v1.0-dev").is_ok());
        assert!(validate_deployment_name("greeter laptop").is_err());
    }
}
