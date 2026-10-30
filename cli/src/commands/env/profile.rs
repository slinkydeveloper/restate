// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! The sections of the CLI config file (one per environment), as `restate env add` and
//! `restate cloud login` write them.

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, table, value};

use restate_types::net::address::{AdminPort, AdvertisedAddress, HttpIngressPort};

use crate::cli_env::CliEnv;

/// What a section of the config file says about its environment.
pub(crate) enum ProfileSpec<'a> {
    SelfHosted {
        admin_base_url: &'a AdvertisedAddress<AdminPort>,
        ingress_base_url: &'a AdvertisedAddress<HttpIngressPort>,
        bearer_token: Option<&'a str>,
    },
    #[cfg(feature = "cloud")]
    Cloud {
        account_id: &'a str,
        environment: &'a crate::clients::cloud::types::DescribeEnvironmentResponse,
    },
}

/// The config file as a TOML document, empty when there is none yet.
pub(crate) fn load_config_document(env: &CliEnv) -> Result<DocumentMut> {
    let config_data = if env.config_file.is_file() {
        std::fs::read_to_string(env.config_file.as_path())
            .with_context(|| format!("Failed to read {}", env.config_file.display()))?
    } else {
        String::new()
    };
    config_data
        .parse::<DocumentMut>()
        .with_context(|| format!("Failed to parse {} as TOML", env.config_file.display()))
}

/// The names of the environments the config file defines.
pub(crate) fn list_profiles(doc: &DocumentMut) -> Vec<String> {
    doc.iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("global") && !k.eq_ignore_ascii_case("default"))
        .map(|(k, _)| k.to_string())
        .collect()
}

/// Write the section of environment `name`, replacing an existing one.
pub(crate) fn write_profile(doc: &mut DocumentMut, name: &str, spec: &ProfileSpec<'_>) {
    doc[name] = table();
    let profile = &mut doc[name];
    match spec {
        ProfileSpec::SelfHosted {
            admin_base_url,
            ingress_base_url,
            bearer_token,
        } => {
            profile["admin_base_url"] = value(admin_base_url.to_string());
            profile["ingress_base_url"] = value(ingress_base_url.to_string());
            if let Some(bearer_token) = bearer_token {
                profile["bearer_token"] = value(*bearer_token);
            }
        }
        #[cfg(feature = "cloud")]
        ProfileSpec::Cloud {
            account_id,
            environment,
        } => {
            profile["environment_type"] = value("cloud");
            profile["ingress_base_url"] = value(&environment.ingress_base_url);
            profile["admin_base_url"] = value(&environment.admin_base_url);

            let cloud = profile["cloud"].or_insert(table());
            cloud["account_id"] = value(*account_id);
            cloud["environment_id"] = value(&environment.environment_id);
        }
    }
}

/// The name in the `environment` file (the current environment), if any.
pub(crate) fn current_profile(env: &CliEnv) -> Result<Option<String>> {
    if !env.environment_file.is_file() {
        return Ok(None);
    }
    let name = std::fs::read_to_string(&env.environment_file)
        .with_context(|| format!("Failed to read {}", env.environment_file.display()))?;
    Ok(Some(name.trim().to_owned()))
}

/// Make `name` the current environment. Returns whether it changed.
pub(crate) fn select_profile(env: &CliEnv, name: &str) -> Result<bool> {
    if current_profile(env)?.as_deref() == Some(name) {
        return Ok(false);
    }
    env.write_environment(name)
        .with_context(|| format!("Failed to write {}", env.environment_file.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_profile_replaces_the_section() {
        let mut doc = "[global]\nx = 1\n\n[staging]\nenvironment_type = \"cloud\"\nadmin_base_url = \"http://old:9070\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        let admin: AdvertisedAddress<AdminPort> =
            "https://restate.example.com:9070".parse().unwrap();
        let ingress: AdvertisedAddress<HttpIngressPort> =
            "https://restate.example.com:8080".parse().unwrap();
        write_profile(
            &mut doc,
            "staging",
            &ProfileSpec::SelfHosted {
                admin_base_url: &admin,
                ingress_base_url: &ingress,
                bearer_token: Some("tok"),
            },
        );
        assert_eq!(list_profiles(&doc), vec!["staging"]);
        let staging = doc["staging"].as_table().unwrap();
        assert!(staging.get("environment_type").is_none());
        assert_eq!(
            staging["admin_base_url"].as_str(),
            Some("https://restate.example.com:9070/")
        );
        assert_eq!(staging["bearer_token"].as_str(), Some("tok"));
    }
}
