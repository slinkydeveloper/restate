// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use anyhow::Result;
use reqwest::Client;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

const DISCOVERY_PORT: u16 = 9080;
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(5);
const DISCOVERY_RETRY_INTERVAL: Duration = Duration::from_secs(2);

pub struct AutoRegistrationTask {
    admin_client: Client,
    update_sender: mpsc::Sender<String>,
    admin_url: String,
}

#[derive(Debug, Clone)]
struct DiscoveredEndpoint {
    url: String,
    path: String,
    http2: bool,
}

impl AutoRegistrationTask {
    pub fn new(admin_url: String) -> (Self, mpsc::Receiver<String>) {
        let (update_sender, update_receiver) = mpsc::channel(10);

        (
            Self {
                admin_url,
                admin_client: Default::default(),
                update_sender,
            },
            update_receiver,
        )
    }

    pub async fn run(mut self) {
        loop {
            // Try to ping the endpoint to check if it exists, and on which port
            match self.ping_endpoint().await {
                Ok(endpoint) => {
                    let _ = self
                        .update_sender
                        .send(format!(
                            "Discovered port {} at {} (HTTP/{})",
                            DISCOVERY_PORT,
                            endpoint.path,
                            if endpoint.http2 { "2" } else { "1.1" }
                        ))
                        .await;

                    // Register the deployment
                    if let Err(e) = self.register_deployment(&endpoint.url).await {
                        let _ = self
                            .update_sender
                            .send(format!("Registration failure: {}", e))
                            .await;
                        sleep(DISCOVERY_RETRY_INTERVAL).await;
                        continue;
                    }

                    let _ = self
                        .update_sender
                        .send(format!("Registered deployment at {}", endpoint.url))
                        .await;

                    // Monitor for changes
                    self.monitor_endpoint(&endpoint).await;

                    let _ = self
                        .update_sender
                        .send("Service restarted, re-discovering...".to_string())
                        .await;
                }
                Err(_) => {
                    let _ = self
                        .update_sender
                        .send("No deployment found, start your service on port 9080".to_string())
                        .await;
                    sleep(DISCOVERY_RETRY_INTERVAL).await;
                }
            }
        }
    }

    async fn ping_endpoint(&self) -> Result<DiscoveredEndpoint> {
        let base_url = format!("http://localhost:{}", DISCOVERY_PORT);
        let paths = vec!["/health", "/restate/health"];

        // Try HTTP/2 with prior knowledge first, then HTTP/1.1
        for &http2 in &[true, false] {
            for path in &paths {
                let url = format!("{}{}", base_url, path);
                let client = if http2 {
                    Client::builder()
                        .http2_prior_knowledge()
                        .timeout(Duration::from_secs(1))
                        .build()?
                } else {
                    Client::builder().timeout(Duration::from_secs(1)).build()?
                };

                if let Ok(response) = client.get(&url).send().await {
                    if response.status().is_success() {
                        return Ok(DiscoveredEndpoint {
                            url: base_url.clone(),
                            path: path.to_string(),
                            http2,
                        });
                    }
                }
            }
        }

        anyhow::bail!("No healthy endpoint found on port {}", DISCOVERY_PORT)
    }

    async fn register_deployment(&self, url: &str) -> Result<()> {
        let discovery_payload =
            serde_json::json!({"uri": url.to_owned(), "force": true}).to_string();
        let discovery_result = self
            .admin_client
            .post(format!("http://{}/deployments", self.admin_url))
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(discovery_payload)
            .send()
            .await?;

        discovery_result.error_for_status()?;
        Ok(())
    }

    async fn monitor_endpoint(&self, endpoint: &DiscoveredEndpoint) {
        let url = format!("{}{}", endpoint.url, endpoint.path);
        let client = if endpoint.http2 {
            Client::builder()
                .http2_prior_knowledge()
                .timeout(Duration::from_secs(2))
                .build()
        } else {
            Client::builder().timeout(Duration::from_secs(2)).build()
        };

        let Ok(client) = client else {
            return;
        };

        let mut last_uptime: Option<String> = None;

        loop {
            sleep(HEALTH_POLL_INTERVAL).await;

            match client.get(&url).send().await {
                Ok(response) => {
                    if let Some(uptime) = response.headers().get("x-uptime") {
                        let uptime_str = uptime.to_str().unwrap_or("").to_string();

                        if let Some(last) = &last_uptime {
                            if &uptime_str != last {
                                // Uptime changed, service restarted
                                return;
                            }
                        } else {
                            last_uptime = Some(uptime_str);
                        }
                    }
                }
                Err(_) => {
                    // Service is down, trigger re-discovery
                    return;
                }
            }
        }
    }
}
