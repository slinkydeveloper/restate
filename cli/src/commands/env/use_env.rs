// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use anyhow::Result;
use cling::prelude::*;

use crate::cli_env::CliEnv;
use crate::ui::fmt::{Field, Formatter, IncludeFormatting, Outcome, OutputFormatter};

/// Select the environment for later commands; overridden by -e and $RESTATE_ENVIRONMENT
///
/// Writes the name to the `environment` file of the CLI config directory
/// (`$HOME/.config/restate`, or $RESTATE_CLI_CONFIG_HOME). The name isn't checked: an unknown
/// one makes later commands fail with no admin URL configured (see `restate env list`).
#[derive(Run, Parser, Collect, Clone)]
#[cling(run = "run_use")]
#[command(after_help = after_help!(
    examples: [
        "restate env use prod",
        "restate env use local     # back to the server on this machine",
    ],
    learn_more: "https://docs.restate.dev/references/cli-config",
))]
pub struct Use {
    /// Name of the environment (a section of the CLI config file) to switch to
    #[clap(index = 1)]
    pub(crate) environment_name: String,
}

pub async fn run_use(State(env): State<CliEnv>, opts: &Use) -> Result<()> {
    env.write_environment(&opts.environment_name)?;

    let mut f = Formatter::new();
    f.detail(
        "environment",
        [
            ("name", Field::new(opts.environment_name.as_str())),
            (
                "environment_file",
                Field::new(env.environment_file.display().to_string()),
            ),
        ],
    );
    f.outcome(
        "result",
        Field::with_display(
            "selected",
            format!(
                "Current environment is now \"{}\" (written to {})",
                opts.environment_name,
                env.environment_file.display()
            ),
        ),
        Outcome::Success,
    );
    f.next_step(
        "restate whoami",
        "check the server it points to",
        IncludeFormatting::Yes,
    );
    f.finish()
}
