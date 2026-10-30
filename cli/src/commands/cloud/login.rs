// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use anyhow::{Context, Result, anyhow};
use cling::prelude::*;
use indicatif::ProgressBar;
use serde::Deserialize;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item, table, value};

use restate_cli_util::{CliContext, c_println};

use crate::commands::env::profile::{
    ProfileSpec, load_config_document, select_profile, write_profile,
};
use crate::ui::fmt::{Field, Formatter, IncludeFormatting, Outcome, OutputFormatter};
use crate::{build_info, cli_env::CliEnv, clients::cloud::CloudClient};

use super::profile::{Choice, choose_environment};

#[derive(Run, Parser, Collect, Clone)]
#[cling(run = "run_login")]
#[command(after_help = after_help!(
    examples: [
        "restate cloud login",
        "restate cloud login --no-env    # only log in, pick the environment later with `restate env add`",
    ],
    learn_more: "https://docs.restate.dev/cloud",
))]
pub struct Login {
    /// Only log in: don't select a Cloud environment afterwards
    #[clap(long)]
    no_env: bool,
}

pub async fn run_login(State(env): State<CliEnv>, opts: &Login) -> Result<()> {
    let mut doc = load_config_document(&env)?;

    let client_id = env.config.cloud.resolve_workos_client_id().await?;
    let access_token = workos_auth_flow(&client_id).await?;

    write_access_token(&mut doc, &access_token)?;
    env.write_config(&doc.to_string())
        .context("Failed to write to config file")?;

    let mut f = Formatter::new();
    f.outcome(
        "credentials",
        Field::with_display(
            json!({ "config_file": env.config_file.display().to_string() }),
            format!(
                "Updated {} with Restate Cloud credentials",
                env.config_file.display()
            ),
        ),
        Outcome::Success,
    );
    if opts.no_env {
        return f.finish();
    }

    let mut env = env;
    env.config.cloud.credentials = Some(super::Credentials { access_token });
    // The login succeeded whatever happens next: a failure to select an environment is
    // reported, not an error.
    if let Err(err) = select_environment(&env, &mut f).await {
        f.warning(&format!(
            "Logged in, but couldn't select an environment: {err:#}"
        ));
        f.next_step(
            "restate env add",
            "pick the environment to use",
            IncludeFormatting::Yes,
        );
    }
    f.finish()
}

/// Select the Cloud environment to use: silently when there is one account with one
/// environment, by asking when possible, else leave it to `restate env add`.
async fn select_environment(env: &CliEnv, f: &mut Formatter) -> Result<()> {
    let client = CloudClient::new(env)?;
    let no_environment = |f: &mut Formatter, message: String| {
        f.outcome(
            "environment",
            Field::with_display(Value::Null, message),
            Outcome::NothingToDo,
        );
    };
    let chosen = match choose_environment(&client, None, CliContext::get().can_prompt()).await? {
        Choice::Environment(chosen) => chosen,
        Choice::NoAccounts => {
            no_environment(
                f,
                "No account yet: create one in the Cloud UI at https://cloud.restate.dev"
                    .to_owned(),
            );
            return Ok(());
        }
        Choice::NoEnvironments { account_name } => {
            no_environment(
                f,
                format!(
                    "Account {account_name} has no environment yet: create one in the Cloud UI at https://cloud.restate.dev"
                ),
            );
            f.next_step("restate env add", "then select it", IncludeFormatting::Yes);
            return Ok(());
        }
        Choice::Ambiguous => {
            no_environment(
                f,
                "No environment selected: you have several accounts or environments".to_owned(),
            );
            f.next_step(
                "restate env add",
                "pick the one to use",
                IncludeFormatting::Yes,
            );
            return Ok(());
        }
    };

    let profile = chosen.environment.name.clone();
    let mut doc = load_config_document(env)?;
    if let Some(existing) = doc.get(&profile)
        && !is_cloud_environment(existing, &chosen.environment.environment_id)
    {
        no_environment(
            f,
            format!(
                "No environment selected: the CLI config file already has an environment named \"{profile}\" that isn't this one"
            ),
        );
        f.next_step(
            &format!("restate env add {}/{}", chosen.account_name, profile),
            "add it under another name",
            IncludeFormatting::Yes,
        );
        return Ok(());
    }
    write_profile(
        &mut doc,
        &profile,
        &ProfileSpec::Cloud {
            account_id: &chosen.account_id,
            environment: &chosen.environment,
        },
    );
    env.write_config(&doc.to_string())
        .context("Failed to write to config file")?;
    select_profile(env, &profile)?;
    f.outcome(
        "environment",
        Field::with_display(
            json!({
                "name": profile,
                "environment_id": chosen.environment.environment_id,
                "region": chosen.environment.region,
                "account_id": chosen.account_id,
                "account_name": chosen.account_name,
            }),
            format!(
                "Using environment \"{profile}\" ({}, {}) of account {}",
                chosen.environment.environment_id, chosen.environment.region, chosen.account_name
            ),
        ),
        Outcome::Success,
    );
    f.next_step(
        "restate cloud link",
        "connect a service in the current directory to it",
        IncludeFormatting::Yes,
    );
    Ok(())
}

/// Whether a section of the config file is the Cloud environment `environment_id`.
fn is_cloud_environment(section: &Item, environment_id: &str) -> bool {
    section
        .get("cloud")
        .and_then(|cloud| cloud.get("environment_id"))
        .and_then(Item::as_str)
        == Some(environment_id)
}

fn write_access_token(doc: &mut DocumentMut, access_token: &str) -> Result<()> {
    let cloud = doc["global"].or_insert(table())["cloud"].or_insert(table());
    cloud["access_token"] = value(access_token);

    Ok(())
}

async fn workos_auth_flow(client_id: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent(format!(
            "{}/{} {}-{}",
            env!("CARGO_PKG_NAME"),
            build_info::RESTATE_CLI_VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH,
        ))
        .https_only(true)
        .connect_timeout(CliContext::get().connect_timeout())
        .build()
        .context("Failed to build HTTP client")?;

    let device_auth_response: DeviceAuthorizationResponse = client
        .post("https://api.workos.com/user_management/authorize/device")
        .form(&[("client_id", client_id)])
        .send()
        .await
        .context("Failed to request device authorization")?
        .error_for_status()
        .context("Bad status code from device authorization endpoint")?
        .json()
        .await
        .context("Failed to decode device authorization response")?;

    c_println!(
        "Please visit {} and enter code: {}",
        device_auth_response.verification_uri,
        device_auth_response.user_code
    );

    if let Err(_err) = open::that(device_auth_response.verification_uri_complete.clone()) {
        c_println!("Failed to open browser automatically. Please open the above URL manually.")
    }

    let progress = ProgressBar::new_spinner();
    progress.set_style(indicatif::ProgressStyle::with_template("{spinner} {msg}").unwrap());
    progress.enable_steady_tick(std::time::Duration::from_millis(120));
    progress.set_message("Waiting for authentication...");

    let mut interval = std::time::Duration::from_secs(device_auth_response.interval);
    let expires_at =
        std::time::Instant::now() + std::time::Duration::from_secs(device_auth_response.expires_in);

    loop {
        if std::time::Instant::now() > expires_at {
            progress.finish_and_clear();
            return Err(anyhow!("Device authorization expired. Please try again."));
        }

        tokio::time::sleep(interval).await;

        let token_result: Result<WorkOSAuthenticateResponse, _> = client
            .post("https://api.workos.com/user_management/authenticate")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &device_auth_response.device_code),
                ("client_id", client_id),
            ])
            .send()
            .await
            .context("Failed to poll for authentication")?
            .json()
            .await;

        match token_result {
            Ok(response) if response.access_token.is_some() => {
                progress.finish_and_clear();
                return Ok(response.access_token.unwrap());
            }
            Ok(response) if response.error.as_deref() == Some("authorization_pending") => {
                continue;
            }
            Ok(response) if response.error.as_deref() == Some("slow_down") => {
                interval += std::time::Duration::from_secs(1);
                continue;
            }
            Ok(response) if response.error.is_some() => {
                progress.finish_and_clear();
                return Err(anyhow!(
                    "Authentication failed: {}",
                    response.error.unwrap_or_else(|| "unknown error".into())
                ));
            }
            Ok(_) => {
                progress.finish_and_clear();
                return Err(anyhow!("Unexpected response from authentication endpoint"));
            }
            Err(err) => {
                progress.finish_and_clear();
                return Err(err.into());
            }
        }
    }
}

#[derive(Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct WorkOSAuthenticateResponse {
    access_token: Option<String>,
    error: Option<String>,
}
