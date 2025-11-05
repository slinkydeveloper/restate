// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

mod auto_registration_task;
mod ui;

use ansi_to_tui::IntoText;
use anyhow::{Result, anyhow};
use chrono::{DateTime, Local};
use cling::prelude::*;
use comfy_table::{Cell, Table};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};
use std::collections::VecDeque;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::io::SyncIoBridge;
use tokio_util::sync::CancellationToken;

use restate_cli_util::ui::console::StyledTable;
use restate_cli_util::ui::stylesheet;
use restate_cli_util::{CliContext, c_indent_table, c_println};
use restate_lite::{AddressKind, AddressMeta, LoggingOptions, Options, Restate};
use restate_types::art::render_restate_logo;
use restate_types::net::address::{AdminPort, HttpIngressPort, ListenerPort};

use crate::build_info;
use crate::cli_env::CliEnv;
use crate::commands::dev::auto_registration_task::AutoRegistrationTask;

#[derive(Run, Parser, Collect, Clone)]
#[cling(run = "run")]
pub struct Dev {
    /// Start restate on a set of random ports
    #[clap(long, short = 'r')]
    use_random_ports: bool,

    /// Data will be saved in a temporary directory.
    #[clap(long)]
    temp: bool,
}

pub async fn run(State(_env): State<CliEnv>, opts: &Dev) -> Result<()> {
    let data_dir = if opts.temp {
        tempfile::tempdir()?.path().to_path_buf()
    } else {
        std::env::current_dir()?.join(".restate")
    };

    let (stdout_reader, stdout_writer) = tokio::io::simplex(1024);
    let (stderr_reader, stderr_writer) = tokio::io::simplex(1024);
    let options = Options {
        enable_tcp: true,
        use_random_ports: opts.use_random_ports,
        data_dir: Some(data_dir.clone()),
        logging: Some(LoggingOptions {
            log_filter: "info".to_string(),
            stdout: stdout_writer,
            stderr: stderr_writer,
        }),
        // TODO crank up inactivity timeout and abort timeout!
        ..Default::default()
    };

    let cancellation = CancellationToken::new();
    {
        let cancellation = cancellation.clone();
        let boxed: Box<dyn Fn() + Send> = Box::new(move || cancellation.cancel());
        *crate::EXIT_HANDLER.lock().unwrap() = Some(boxed);
    }

    let (latest_release_check_tx, latest_release_check_rx) = oneshot::channel();
    tokio::spawn(async move {
        latest_release_check_tx.send(build_info::check_if_latest_version().await)
    });

    let restate = Restate::start(options).await?;

    let addresses = restate.get_advertised_addresses();
    let admin_url = addresses
        .iter()
        .find_map(|address| {
            if address.name == AdminPort::NAME && address.kind == AddressKind::Http {
                Some(address.address.clone())
            } else {
                None
            }
        })
        .expect("Admin port is always set");
    let ingress_url = addresses
        .iter()
        .find_map(|address| {
            if address.name == HttpIngressPort::NAME && address.kind == AddressKind::Http {
                Some(address.address.clone())
            } else {
                None
            }
        })
        .expect("Ingress port is always set");

    let (auto_registration_task, auto_registration_status_rx) =
        AutoRegistrationTask::new(admin_url.clone());
    tokio::spawn(async move {
        auto_registration_task.run().await;
    });

    let res = ui::run(
        ratatui::init(),
        restate,
        cancellation,
        admin_url,
        ingress_url,
        auto_registration_status_rx,
        latest_release_check_rx,
        stdout_reader,
        stderr_reader,
    ).await;
    ratatui::restore();
    res?;

    Ok(())
}
