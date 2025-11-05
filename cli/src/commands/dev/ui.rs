// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use crate::build_info::VersionCheckResult;
use ansi_to_tui::IntoText;
use chrono::{DateTime, Local};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures::StreamExt;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};
use reqwest::Client;
use restate_lite::Restate;
use restate_types::{art, SemanticRestateVersion};
use std::collections::VecDeque;
use std::fmt::format;
use std::time::Duration;
use thiserror::__private17::AsDisplay;
use tokio::io::{AsyncBufReadExt, BufReader, ReadHalf, SimplexStream};
use tokio::pin;
use tokio::sync::mpsc::Receiver;
use tokio::sync::oneshot;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

const MAX_LOG_LINES: usize = 5000;
const LOG_GENERATION_MIN_MS: u64 = 100;
const LOG_GENERATION_MAX_MS: u64 = 500;

struct TuiState {
    auto_registration_state: String,
    restate_version_check_state: String,
    /// Log buffer with max size
    logs: VecDeque<String>,
    /// Current scroll position (0 = bottom/latest)
    scroll_offset: usize,
    /// Auto-scroll enabled
    auto_scroll: bool,
    /// Start time for uptime calculation
    start_time: DateTime<Local>,
    /// Mock request counter
    request_count: u64,
    /// Mock service counter
    service_count: u32,
    /// Mock memory usage in MB
    memory_usage: f64,
    /// Last known viewport height for log viewer
    viewport_height: usize,
    /// Accumulated scroll delta (for debouncing)
    scroll_accumulator: i32,
}

impl TuiState {
    pub fn new() -> Self {
        Self {
            auto_registration_state: "Discovering...".to_string(),
            restate_version_check_state: "Checking updates...".to_string(),
            logs: VecDeque::new(),
            scroll_offset: 0,
            auto_scroll: true,
            start_time: Local::now(),
            request_count: 0,
            service_count: 3,
            memory_usage: 45.2,
            viewport_height: 10, // Will be updated on first render
            scroll_accumulator: 0,
        }
    }

    /// Renders the user interface.
    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Create main layout: top info section, log viewer, bottom help bar
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(15), // Top info boxes
                Constraint::Min(10),    // Log viewer (takes remaining space)
                Constraint::Length(3),  // Bottom help bar
            ])
            .split(area);

        // Split top section into left (logo/info) and right (status)
        let top_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(main_chunks[0]);

        // Render top left box (logo + info)
        self.render_logo_box(frame, top_chunks[0]);

        // Render top right box (status)
        self.render_status_box(frame, top_chunks[1]);

        // Render log viewer
        self.render_log_viewer(frame, main_chunks[1]);

        // Render bottom help bar
        self.render_help_bar(frame, main_chunks[2]);
    }

    fn render_logo_box(&self, frame: &mut Frame, area: Rect) {
        let mut text = art::render_restate_logo_small(true).into_text().unwrap();
            text.push_line(Line::default());
            text.push_line(
                Line::from(vec![
                    Span::styled("Version: ", Style::default().fg(Color::Gray)),
                    Span::styled(SemanticRestateVersion::current().to_string(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" ({})", self.restate_version_check_state), Style::default().fg(Color::Gray)),
                ]),
        );


        let paragraph = Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(" Restate dev ")
                .title_style(
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
        );

        frame.render_widget(paragraph, area);
    }

    fn render_status_box(&self, frame: &mut Frame, area: Rect) {
        let uptime = Local::now().signed_duration_since(self.start_time);
        let uptime_str = format!(
            "{}h {}m {}s",
            uptime.num_hours(),
            uptime.num_minutes() % 60,
            uptime.num_seconds() % 60
        );

        let status_text = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("Uptime:          ", Style::default().fg(Color::Gray)),
                Span::styled(
                    &uptime_str,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Requests:        ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{}", self.request_count),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Services:        ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{}", self.service_count),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Memory Usage:    ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{:.1} MB", self.memory_usage),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Registration:    ", Style::default().fg(Color::Gray)),
                Span::styled(
                    "Ready",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        ];

        let paragraph = Paragraph::new(status_text).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(" Status ")
                .title_style(
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
        );

        frame.render_widget(paragraph, area);
    }

    fn render_log_viewer(&mut self, frame: &mut Frame, area: Rect) {
        let log_count = self.logs.len();

        // Calculate visible range based on scroll offset
        let visible_height = area.height.saturating_sub(2) as usize; // Subtract borders

        // Update viewport height for scroll calculations
        self.viewport_height = visible_height;

        let scroll_indicator = if self.auto_scroll {
            " [Auto-scroll: ON] "
        } else {
            " [Auto-scroll: OFF - Press r to resume] "
        };

        let title = format!(" Logs ({} lines) {}", log_count, scroll_indicator);

        // Determine which logs to show
        let logs_to_show: Vec<Line> = if self.auto_scroll {
            // Show the most recent logs
            self.logs
                .iter()
                .rev()
                .take(visible_height)
                .rev()
                .map(|log| {
                    // Parse ANSI escape codes in log lines
                    match log.as_str().into_text() {
                        Ok(text) => text
                            .lines
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Line::from(log.clone())),
                        Err(_) => Line::from(log.clone()),
                    }
                })
                .collect()
        } else {
            // Show logs based on scroll offset
            let start_idx = log_count.saturating_sub(self.scroll_offset + visible_height);
            let end_idx = log_count.saturating_sub(self.scroll_offset);

            self.logs
                .iter()
                .skip(start_idx)
                .take(end_idx - start_idx)
                .map(|log| {
                    // Parse ANSI escape codes in log lines
                    match log.as_str().into_text() {
                        Ok(text) => text
                            .lines
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Line::from(log.clone())),
                        Err(_) => Line::from(log.clone()),
                    }
                })
                .collect()
        };

        let paragraph = Paragraph::new(logs_to_show)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue))
                    .title(title)
                    .title_style(
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::BOLD),
                    ),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);

        // Render scrollbar if not in auto-scroll mode
        if !self.auto_scroll && log_count > visible_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(Color::Blue));

            let mut scrollbar_state = ScrollbarState::new(log_count.saturating_sub(visible_height))
                .position(self.scroll_offset);

            frame.render_stateful_widget(
                scrollbar,
                area.inner(ratatui::layout::Margin {
                    horizontal: 0,
                    vertical: 1,
                }),
                &mut scrollbar_state,
            );
        }
    }

    fn render_help_bar(&self, frame: &mut Frame, area: Rect) {
        let help_text = Line::from(vec![
            Span::styled(
                " [q] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Quit  "),
            Span::styled(
                " [x] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Kill Invocations  "),
            Span::styled(
                " [f] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Register Service  "),
            Span::styled(
                " [↑↓/PgUp/PgDn] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Scroll  "),
            Span::styled(
                " [r] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Auto-scroll  "),
            Span::styled(
                " [End] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Top "),
        ]);

        let paragraph = Paragraph::new(help_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue)),
            )
            .alignment(Alignment::Center);

        frame.render_widget(paragraph, area);
    }

    fn scroll_up(&mut self, lines: usize) {
        self.auto_scroll = false;
        // Maximum scroll offset should keep viewport filled
        let max_scroll = self.logs.len().saturating_sub(self.viewport_height);
        self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);
    }

    fn scroll_down(&mut self, lines: usize) {
        if self.scroll_offset >= lines {
            self.scroll_offset -= lines;
            if self.scroll_offset == 0 {
                self.auto_scroll = true;
            }
        } else {
            self.enable_auto_scroll();
        }
    }

    fn scroll_to_top(&mut self) {
        self.auto_scroll = false;
        // Scroll to top but keep viewport filled
        self.scroll_offset = self.logs.len().saturating_sub(self.viewport_height);
    }

    fn enable_auto_scroll(&mut self) {
        self.auto_scroll = true;
        self.scroll_offset = 0;
    }

    fn append_log_message(&mut self, log_line: String) {
        // If not in auto-scroll mode, increment scroll offset to maintain current view
        if !self.auto_scroll {
            self.scroll_offset += 1;
        }

        self.logs.push_back(log_line);

        // Trim logs if exceeding max size
        if self.logs.len() > MAX_LOG_LINES {
            self.logs.pop_front();
            // If we trimmed from the front while scrolling, adjust offset
            if !self.auto_scroll && self.scroll_offset > 0 {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
        }
    }

    /// Apply accumulated scroll with debouncing
    fn apply_accumulated_scroll(&mut self) {
        if self.scroll_accumulator > 0 {
            self.scroll_up(self.scroll_accumulator as usize);
        } else {
            self.scroll_down((-self.scroll_accumulator) as usize);
        }
        self.scroll_accumulator = 0;
    }
}

pub async fn run(
     terminal: DefaultTerminal,
    restate: Restate,
    cancellation_token: CancellationToken,
    admin_url: String,
    ingress_url: String,
    auto_registration_status_rx: Receiver<String>,
    restate_version_rx: oneshot::Receiver<VersionCheckResult>,
    stdout_reader: ReadHalf<SimplexStream>,
    stderr_reader: ReadHalf<SimplexStream>,
)-> anyhow::Result<()> {
    AppState::new(restate, cancellation_token, admin_url, ingress_url, ).run(terminal,   auto_registration_status_rx, restate_version_rx,stdout_reader, stderr_reader).await
}

struct AppState {
    running: bool,

    // --- Restate server stuff
    restate: Restate,
    cancellation_token: CancellationToken,
    admin_url: String,
    ingress_url: String,

    // --- Admin client
    admin_client: Client,

    tui_state: TuiState,
}

impl AppState {
     fn new(
        restate: Restate,
        cancellation_token: CancellationToken,
        admin_url: String,
        ingress_url: String,
    ) -> Self {
        Self {
            running: true,
            restate,
            cancellation_token,
            admin_url,
            ingress_url,
            admin_client: Default::default(),
            tui_state: TuiState::new(),
        }
    }

    /// Run the application's main loop.
    async fn run(mut self, mut terminal: DefaultTerminal,       auto_registration_status_rx: Receiver<String>, restate_version_rx: oneshot::Receiver<VersionCheckResult>,        stdout_reader: ReadHalf<SimplexStream>,
                 stderr_reader: ReadHalf<SimplexStream>,  ) -> anyhow::Result<()> {
        // Create event stream for crossterm
        let mut event_stream = EventStream::new();

        // Create debounce interval (50ms)
        let mut debounce_interval = interval(Duration::from_millis(50));

        let mut stdout_lines = BufReader::new(stdout_reader).lines();
        pin!(stdout_lines);
        let mut stderr_lines = BufReader::new(stderr_reader).lines();
        pin!(stderr_lines);
        pin!(restate_version_rx);

        while self.running {
            terminal.draw(|frame| self.tui_state.render(frame))?;

            tokio::select! {
                // Handle log messages
                Ok(Some(log_line)) = stdout_lines.next_line() => {
                    self.tui_state.append_log_message(log_line);
                }
                Ok(Some(log_line)) = stderr_lines.next_line() => {
                    self.tui_state.append_log_message(log_line);
                }

                // Handle crossterm events (keyboard/mouse)
                Some(Ok(event)) = event_stream.next() => {
                    self.handle_event(event);
                }

                // Handle cancellation signal
                _ = self.cancellation_token.cancelled() => {
                    self.quit();
                },

                // Handle version check
                Ok(version) = &mut restate_version_rx => {
                    self.handle_version_check(version);
                }

                // Handle scroll debounce timer
                _ = debounce_interval.tick(), if self.tui_state.scroll_accumulator != 0 => {
                    self.tui_state.apply_accumulated_scroll();
                }
            }
        }

        // TODO to do things cleanly here, we should continue in the loop to stream out the logs during shutdown.
        self.restate.stop().await?;
        Ok(())
    }

    /// Handle crossterm events (keyboard, mouse, resize)
    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key_event(key),
            Event::Mouse(mouse) => self.handle_mouse_event(mouse),
            Event::Resize(_, _) => {}
            _ => {}
        }
    }

    /// Handles the key events and updates the state of [`Tui`].
    fn handle_key_event(&mut self, key: KeyEvent) {
        match (key.modifiers, key.code) {
            // Quit
            (_, KeyCode::Esc | KeyCode::Char('q'))
            | (KeyModifiers::CONTROL, KeyCode::Char('c') | KeyCode::Char('C')) => self.quit(),

            // Kill invocations
            (_, KeyCode::Char('x')) => self.kill_invocations(),

            // Scroll controls
            (_, KeyCode::Up) => self.tui_state.scroll_up(1),
            (_, KeyCode::Down) => self.tui_state.scroll_down(1),
            (_, KeyCode::PageUp) => self.tui_state.scroll_up(10),
            (_, KeyCode::PageDown) => self.tui_state.scroll_down(10),
            (_, KeyCode::Char('r')) => self.tui_state.enable_auto_scroll(),
            (_, KeyCode::End) => self.tui_state.scroll_to_top(),

            _ => {}
        }
    }

    fn handle_mouse_event(&mut self, mouse: crossterm::event::MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                // Accumulate scroll events for debouncing
                self.tui_state.scroll_accumulator += 3;
            }
            MouseEventKind::ScrollDown => {
                // Accumulate scroll events for debouncing
                self.tui_state.scroll_accumulator -= 3;
            }
            _ => {}
        }
    }

    fn handle_version_check(&mut self, version_check_result: VersionCheckResult) {
        match version_check_result {
            VersionCheckResult::OnLatestVersion => {
                self.tui_state.restate_version_check_state = "Latest".to_string();
            }
            VersionCheckResult::ShouldUpdate { latest_version, .. } => {
                self.tui_state.restate_version_check_state = format!("Version {latest_version} available")
            }
        }
    }

    fn kill_invocations(&mut self) {}

    fn open_ui(&mut self) {
        let _ = open::that(&self.admin_url);
    }

    /// Set running to false to quit the application.
    fn quit(&mut self) {
        self.cancellation_token.cancel();
        self.running = false;
    }
}
