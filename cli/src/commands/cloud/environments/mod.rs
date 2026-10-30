// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

mod tunnel;

use anyhow::Result;
use cling::prelude::*;

use crate::cli_env::CliEnv;

use super::profile::EnvironmentSpecifier;

/// Deprecated: use `restate env add` and `restate cloud tunnel`
#[derive(Run, Subcommand, Clone)]
#[clap(alias = "env", alias = "environment")]
pub enum Environments {
    /// Deprecated: use `restate env add`
    #[command(hide = true)]
    Configure(Configure),
    /// Connect a Cloud environment and this machine through a tunnel
    ///
    /// Uses the current environment (or -e), which must be a Cloud one. Exposes the
    /// environment's ports (ingress 8080, admin 9070) on localhost, and lets the environment
    /// call services running on this machine: register them with
    /// `restate deployments register --tunnel-name <name> http://localhost:9080` (use your
    /// service's URL). Runs until interrupted. By default, all remote ports are exposed and
    /// inbound calls are allowed.
    Tunnel(tunnel::Tunnel),
}

/// Deprecated: use `restate env add`
#[derive(Run, Parser, Collect, Clone)]
#[cling(run = "run_configure")]
pub struct Configure {
    /// The Cloud environment to configure the CLI for: `[ACCOUNT/]ENVIRONMENT`, where each is a
    /// name or an id. If omitted, you're asked to pick one
    environment_specifier: Option<EnvironmentSpecifier>,
}

async fn run_configure(State(env): State<CliEnv>, opts: &Configure) -> Result<()> {
    crate::commands::deprecated("restate cloud env configure", "restate env add");
    crate::commands::env::add::add_cloud(&env, opts.environment_specifier.as_ref(), false).await
}
