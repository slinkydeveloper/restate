// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::{Context, Result};
use cling::prelude::*;
use serde::Deserialize;
use tempfile::tempdir;

use restate_cli_util::CliContext;
use restate_types::net::address::{AdminPort, AdvertisedAddress, HttpIngressPort};

use crate::cli_env::CliEnv;
use crate::console::confirm_or_exit;
use crate::error::RestateCliError;
use crate::ui::fmt::{Field, Formatter, IncludeFormatting, Outcome, OutputFormatter};

use super::profile::{
    ProfileSpec, list_profiles, load_config_document, select_profile, write_profile,
};

/// Add an environment to the CLI config file, and select it
///
/// By default a Restate Cloud environment: the account and environment are asked for (or
/// given as `ACCOUNT/ENVIRONMENT`), their URLs fetched from Restate Cloud, and the section
/// written under the environment's name unless you pick another. Needs `restate cloud login`.
///
/// With --manual, a self-hosted server. In a terminal, a commented template opens in
/// $EDITOR (see `restate config edit`); otherwise, or with --yes, the URLs come from
/// --admin-url and --ingress-url (giving any of the self-hosted flags implies --manual).
/// Overwriting an existing environment asks for confirmation, or needs --yes.
#[derive(Run, Parser, Collect, Clone)]
#[cling(run = "run_add")]
#[command(after_help = after_help!(
    examples: [
        "restate env add                        # pick a Cloud account and environment",
        "restate env add my-account/my-env      # a Cloud environment, by name or id",
        "restate env add --manual staging       # a self-hosted server, edited in $EDITOR",
        "restate env add staging --admin-url https://restate.example.com:9070 --ingress-url https://restate.example.com:8080 --bearer-token \"$TOKEN\" --yes",
    ],
    learn_more: "https://docs.restate.dev/references/cli-config",
))]
pub struct Add {
    /// The Cloud environment to add, `[ACCOUNT/]ENVIRONMENT` (names or ids; asked for when
    /// omitted). With --manual, the name of the environment
    name: Option<String>,

    /// Add a self-hosted server instead of a Cloud environment
    #[clap(long, help_heading = "Self-hosted server")]
    manual: bool,

    /// Admin API base URL of the server, e.g. http://localhost:9070 (implies --manual)
    #[clap(long, help_heading = "Self-hosted server", value_name = "URL")]
    admin_url: Option<AdvertisedAddress<AdminPort>>,

    /// Ingress base URL of the server, e.g. http://localhost:8080 (implies --manual)
    #[clap(long, help_heading = "Self-hosted server", value_name = "URL")]
    ingress_url: Option<AdvertisedAddress<HttpIngressPort>>,

    /// Bearer token the server requires, if any (implies --manual)
    #[clap(long, help_heading = "Self-hosted server", value_name = "TOKEN")]
    bearer_token: Option<String>,

    /// Add the environment without making it the current one
    #[clap(long)]
    no_use: bool,
}

pub async fn run_add(State(env): State<CliEnv>, opts: &Add) -> Result<()> {
    let manual = opts.manual
        || opts.admin_url.is_some()
        || opts.ingress_url.is_some()
        || opts.bearer_token.is_some();
    if manual {
        return add_manual(&env, opts).await;
    }

    #[cfg(feature = "cloud")]
    {
        let specifier = opts.name.as_deref().map(|name| {
            name.parse()
                .expect("an environment specifier parses from any string")
        });
        add_cloud(&env, specifier.as_ref(), opts.no_use).await
    }
    #[cfg(not(feature = "cloud"))]
    {
        Err(RestateCliError::bad_input(
            "This build of the CLI has no Restate Cloud support; add a self-hosted server with --manual",
        )
        .into())
    }
}

/// `restate env add` for a Cloud environment (also behind the deprecated
/// `restate cloud env configure`).
#[cfg(feature = "cloud")]
pub(crate) async fn add_cloud(
    env: &CliEnv,
    specifier: Option<&crate::commands::cloud::profile::EnvironmentSpecifier>,
    no_use: bool,
) -> Result<()> {
    use crate::clients::cloud::CloudClient;
    use crate::commands::cloud::profile::{Choice, choose_environment};
    use crate::console::input;
    use crate::error::ErrorKind;

    if env.config.cloud.credentials.is_none() {
        return Err(
            RestateCliError::new(ErrorKind::Auth, "Not logged in to Restate Cloud")
                .with_next_step("restate cloud login", "log in, then add the environment")
                .with_next_step(
                    "restate env add --manual <name>",
                    "add a self-hosted server instead",
                )
                .into(),
        );
    }
    let client = CloudClient::new(env)?;

    let chosen = match choose_environment(&client, specifier, true).await? {
        Choice::Environment(chosen) => chosen,
        Choice::NoAccounts => {
            return Err(RestateCliError::not_found(
                "No accounts set up; use the Restate Cloud UI at https://cloud.restate.dev to create your first account",
            )
            .into());
        }
        Choice::NoEnvironments { account_name } => {
            return Err(RestateCliError::not_found(format!(
                "No environments in account {account_name}; use the Restate Cloud UI at https://cloud.restate.dev to create your first environment"
            ))
            .into());
        }
        // `prompt` was true: ambiguity is resolved by asking, or fails there.
        Choice::Ambiguous => unreachable!("choose_environment prompts when ambiguous"),
    };

    let mut doc = load_config_document(env)?;
    let profiles = list_profiles(&doc);

    // The section name: asked for, defaulting to the environment's name (used as is when
    // prompting isn't possible, e.g. with `--yes`).
    let profile = if CliContext::get().can_prompt() {
        input(
            &format!(
                "Choose a name for the environment for use with the CLI.\n  Current names: [{}]\n",
                profiles.join(", ")
            ),
            chosen.environment.name.clone(),
        )?
    } else {
        chosen.environment.name.clone()
    };

    let existed = profiles.contains(&profile);
    if existed {
        confirm_or_exit(&format!("Overwrite existing environment \"{profile}\"?"))?;
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
        .with_context(|| format!("Failed to write {}", env.config_file.display()))?;

    let mut f = Formatter::new();
    f.detail(
        "environment",
        [
            ("name", Field::new(profile.as_str())),
            ("type", Field::new("cloud")),
            ("account", Field::new(chosen.account_name.as_str())),
            ("account_id", Field::new(chosen.account_id.as_str())),
            (
                "environment_id",
                Field::new(chosen.environment.environment_id.as_str()),
            ),
            (
                "environment_name",
                Field::new(chosen.environment.name.as_str()),
            ),
            (
                "admin_base_url",
                Field::new(chosen.environment.admin_base_url.as_str()),
            ),
            (
                "ingress_base_url",
                Field::new(chosen.environment.ingress_base_url.as_str()),
            ),
        ],
    );
    finish_add(f, env, &profile, existed, no_use)
}

/// `restate env add --manual`: a self-hosted server.
async fn add_manual(env: &CliEnv, opts: &Add) -> Result<()> {
    let flags_given =
        opts.admin_url.is_some() || opts.ingress_url.is_some() || opts.bearer_token.is_some();
    let (name, profile) = if !flags_given && CliContext::get().can_prompt() {
        edit_manual_profile(env, opts.name.as_deref())?
    } else {
        manual_profile_from_flags(opts)?
    };

    let mut doc = load_config_document(env)?;
    let existed = list_profiles(&doc).contains(&name);
    if existed {
        confirm_or_exit(&format!("Overwrite existing environment \"{name}\"?"))?;
    }
    write_profile(
        &mut doc,
        &name,
        &ProfileSpec::SelfHosted {
            admin_base_url: &profile.admin_base_url,
            ingress_base_url: &profile.ingress_base_url,
            bearer_token: profile.bearer_token.as_deref(),
        },
    );
    env.write_config(&doc.to_string())
        .with_context(|| format!("Failed to write {}", env.config_file.display()))?;

    let mut f = Formatter::new();
    f.detail(
        "environment",
        [
            ("name", Field::new(name.as_str())),
            ("type", Field::new("self_hosted")),
            (
                "admin_base_url",
                Field::new(profile.admin_base_url.to_string()),
            ),
            (
                "ingress_base_url",
                Field::new(profile.ingress_base_url.to_string()),
            ),
            (
                "bearer_token",
                Field::with_display(profile.bearer_token.is_some(), "(set)"),
            ),
        ],
    );
    finish_add(f, env, &name, existed, opts.no_use)
}

/// Report the written section, select it unless `no_use`, and suggest checking it.
fn finish_add(
    mut f: Formatter,
    env: &CliEnv,
    name: &str,
    existed: bool,
    no_use: bool,
) -> Result<()> {
    let (result, verb) = if existed {
        ("updated", "Updated")
    } else {
        ("added", "Added")
    };
    f.outcome(
        "result",
        Field::with_display(
            result,
            format!(
                "{verb} environment \"{name}\" in {}",
                env.config_file.display()
            ),
        ),
        Outcome::Success,
    );
    if !no_use {
        select_profile(env, name)?;
        f.outcome(
            "current_environment",
            Field::with_display(name, format!("Current environment is now \"{name}\"")),
            Outcome::Success,
        );
    }
    f.next_step(
        "restate whoami",
        "check the connection",
        IncludeFormatting::Yes,
    );
    f.finish()
}

/// A self-hosted environment's section, as edited or given with flags.
#[derive(Debug, Deserialize)]
struct ManualProfile {
    admin_base_url: AdvertisedAddress<AdminPort>,
    ingress_base_url: AdvertisedAddress<HttpIngressPort>,
    #[serde(default, deserialize_with = "empty_as_none")]
    bearer_token: Option<String>,
}

fn empty_as_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.filter(|s| !s.trim().is_empty()))
}

fn manual_profile_from_flags(opts: &Add) -> Result<(String, ManualProfile)> {
    let mut missing = Vec::new();
    if opts.name.is_none() {
        missing.push("the environment name");
    }
    if opts.admin_url.is_none() {
        missing.push("--admin-url");
    }
    if opts.ingress_url.is_none() {
        missing.push("--ingress-url");
    }
    let (Some(name), Some(admin_base_url), Some(ingress_base_url)) =
        (&opts.name, &opts.admin_url, &opts.ingress_url)
    else {
        return Err(RestateCliError::bad_input(format!(
            "Missing {}; pass them as arguments, or run `restate env add --manual` in a terminal to edit a template",
            missing.join(", ")
        ))
        .into());
    };
    Ok((
        name.clone(),
        ManualProfile {
            admin_base_url: admin_base_url.clone(),
            ingress_base_url: ingress_base_url.clone(),
            bearer_token: opts.bearer_token.clone().filter(|s| !s.is_empty()),
        },
    ))
}

/// The section name used in the template when none was given; the user renames it.
const TEMPLATE_NAME: &str = "my-server";

/// Open a commented template in the editor and read the section back.
fn edit_manual_profile(env: &CliEnv, name: Option<&str>) -> Result<(String, ManualProfile)> {
    let tempdir = tempdir().context("unable to create a temporary directory")?;
    let edit_file_path = tempdir.path().join("environment.toml");
    let mut file =
        std::fs::File::create(&edit_file_path).context("Failed to create a temp file")?;
    write!(
        file,
        "{}",
        manual_template(
            name.unwrap_or(TEMPLATE_NAME),
            &env.config_file.display().to_string()
        )
    )?;
    drop(file);

    env.open_default_editor(
        &edit_file_path,
        "pass --admin-url, --ingress-url and --bearer-token instead",
    )?;

    let edited = std::fs::read_to_string(&edit_file_path).context("Cannot read the edited file")?;
    parse_manual_profile(&edited)
}

fn manual_template(name: &str, config_file: &str) -> String {
    format!(
        "# Environment \"{name}\": save and close to add it to {config_file}.\n\
         # The section name is the environment's name for the CLI (`restate env use {name}`).\n\
         [{name}]\n\
         # Admin API base URL (scheme + host + port), e.g. http://localhost:9070\n\
         admin_base_url = \"http://localhost:9070\"\n\
         # Ingress base URL, e.g. http://localhost:8080\n\
         ingress_base_url = \"http://localhost:8080\"\n\
         # Bearer token, if the server requires authentication. Leave empty for none.\n\
         bearer_token = \"\"\n"
    )
}

/// The one section of the edited template: its name and settings.
fn parse_manual_profile(edited: &str) -> Result<(String, ManualProfile)> {
    let mut sections: BTreeMap<String, ManualProfile> = toml::from_str(edited)
        .map_err(|e| RestateCliError::bad_input(format!("Cannot parse the edited file: {e}")))?;
    let Some(name) = sections.keys().next().cloned() else {
        return Err(RestateCliError::bad_input(
            "The edited file names no environment: keep the `[name]` section",
        )
        .into());
    };
    if sections.len() > 1 {
        return Err(RestateCliError::bad_input(
            "The edited file has several sections; keep one, for the environment to add",
        )
        .into());
    }
    Ok((
        name.clone(),
        sections.remove(&name).expect("the key exists"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_round_trips_and_an_empty_token_is_none() {
        let template = manual_template("staging", "/home/me/.config/restate/config.toml");
        let (name, profile) = parse_manual_profile(&template).unwrap();
        assert_eq!(name, "staging");
        assert_eq!(profile.admin_base_url.to_string(), "http://localhost:9070/");
        assert_eq!(
            profile.ingress_base_url.to_string(),
            "http://localhost:8080/"
        );
        assert!(profile.bearer_token.is_none());

        let err = parse_manual_profile("[a]\nadmin_base_url = \"http://a:9070\"\ningress_base_url = \"http://a:8080\"\n[b]\nadmin_base_url = \"http://b:9070\"\ningress_base_url = \"http://b:8080\"\n")
            .unwrap_err();
        assert!(err.to_string().contains("several sections"));
    }
}
