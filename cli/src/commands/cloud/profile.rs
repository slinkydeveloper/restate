// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Choosing a Restate Cloud environment (account and environment), as `restate env add`,
//! `restate cloud login` and `restate cloud link` do.

use std::{future::Future, str::FromStr};

use anyhow::Result;
use indicatif::ProgressBar;
use itertools::Itertools;

use restate_cli_util::CliContext;

use crate::clients::cloud::{
    CloudClient, CloudClientInterface,
    types::{
        DescribeEnvironmentResponse, ListAccountsResponseAccountsItem,
        ListEnvironmentsResponseEnvironmentsItem,
    },
};
use crate::console::choose;
use crate::error::RestateCliError;

/// `[ACCOUNT/]ENVIRONMENT`, each a name or an id.
#[derive(Clone, Debug)]
pub(crate) struct EnvironmentSpecifier(pub Option<String>, pub String);

impl FromStr for EnvironmentSpecifier {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            Some((account, environment)) => Ok(Self(Some(account.into()), environment.into())),
            None => Ok(Self(None, s.into())),
        }
    }
}

/// A Cloud environment, with its account and the details from `DescribeEnvironment`.
pub(crate) struct ChosenEnvironment {
    pub account_id: String,
    pub account_name: String,
    pub environment: DescribeEnvironmentResponse,
}

/// The result of [`choose_environment`].
pub(crate) enum Choice {
    Environment(ChosenEnvironment),
    /// The user has no account yet.
    NoAccounts,
    /// The account has no environment yet.
    NoEnvironments {
        account_name: String,
    },
    /// Several accounts or environments to pick from, and `prompt` was false.
    Ambiguous,
}

/// Find the environment `specifier` names, or pick one: silently when the user has one
/// account with one environment, by asking when `prompt` (which fails when prompting
/// isn't possible, see [`choose`]), else [`Choice::Ambiguous`].
pub(crate) async fn choose_environment(
    client: &CloudClient,
    specifier: Option<&EnvironmentSpecifier>,
    prompt: bool,
) -> Result<Choice> {
    let accounts = with_progress("Listing accounts...", async {
        client.list_accounts().await?.into_body().await
    })
    .await?
    .accounts;

    let (account, environment) = match specifier {
        Some(EnvironmentSpecifier(account, environment)) => {
            (account.as_deref(), Some(environment.as_str()))
        }
        None => (None, None),
    };

    let account_i = if let Some(account) = account {
        match accounts
            .iter()
            .find_position(|acc| acc.account_id == account)
            .or_else(|| accounts.iter().find_position(|acc| acc.name == account))
        {
            Some((i, _)) => i,
            None => {
                return Err(RestateCliError::not_found(format!(
                    "Couldn't find account {account}; the accounts are: [{}]",
                    accounts.iter().map(|acc| &acc.name).join(", ")
                ))
                .into());
            }
        }
    } else {
        match accounts.len() {
            0 => return Ok(Choice::NoAccounts),
            1 => 0,
            _ if prompt => account_picker(&accounts)?,
            _ => return Ok(Choice::Ambiguous),
        }
    };
    let account = &accounts[account_i];

    let environments = with_progress("Listing environments...", async {
        client
            .list_environments(&account.account_id)
            .await?
            .into_body()
            .await
    })
    .await?
    .environments;

    let environment_i = if let Some(environment) = environment {
        match environments
            .iter()
            .find_position(|env| env.environment_id == environment)
            .or_else(|| {
                environments
                    .iter()
                    .find_position(|env| env.name == environment)
            }) {
            Some((i, _)) => i,
            None => {
                return Err(RestateCliError::not_found(format!(
                    "Couldn't find environment {environment} in account {}; its environments are: [{}]",
                    account.name,
                    environments.iter().map(|env| &env.name).join(", ")
                ))
                .into());
            }
        }
    } else {
        match environments.len() {
            0 => {
                return Ok(Choice::NoEnvironments {
                    account_name: account.name.clone(),
                });
            }
            1 => 0,
            _ if prompt => environment_picker(&environments)?,
            _ => return Ok(Choice::Ambiguous),
        }
    };

    let environment = with_progress("Describing environment...", async {
        client
            .describe_environment(
                &account.account_id,
                &environments[environment_i].environment_id,
            )
            .await?
            .into_body()
            .await
    })
    .await?;

    Ok(Choice::Environment(ChosenEnvironment {
        account_id: account.account_id.clone(),
        account_name: account.name.clone(),
        environment,
    }))
}

/// A spinner with `msg` while `f` runs; nothing when prompts aren't possible (e.g. `--json`),
/// the spinner would just be noise there.
pub(crate) async fn with_progress<T>(msg: &'static str, f: impl Future<Output = T>) -> T {
    if !CliContext::get().is_interactive() {
        return f.await;
    }
    let progress = ProgressBar::new_spinner();
    progress
        .set_style(indicatif::ProgressStyle::with_template("{spinner} [{elapsed}] {msg}").unwrap());
    progress.enable_steady_tick(std::time::Duration::from_millis(120));

    progress.set_message(msg);
    let result = f.await;
    progress.finish_and_clear();
    result
}

fn account_picker(accounts: &[ListAccountsResponseAccountsItem]) -> Result<usize> {
    choose(
        "Select an Account:",
        accounts.iter().map(|acc| &acc.name).collect_vec().as_ref(),
    )
}

fn environment_picker(environments: &[ListEnvironmentsResponseEnvironmentsItem]) -> Result<usize> {
    choose(
        "Select an Environment:",
        environments
            .iter()
            .map(|env| &env.name)
            .collect_vec()
            .as_ref(),
    )
}
