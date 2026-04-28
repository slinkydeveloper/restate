// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=proto/");

    let serde = "#[derive(::serde::Serialize, ::serde::Deserialize)]";

    // `type_attribute` matches messages, enums, and oneofs by path prefix; "." matches every type.
    prost_build::Config::new()
        .bytes(["."])
        .protoc_arg("--experimental_allow_proto3_optional")
        .type_attribute(".", serde)
        .compile_protos(&["proto/restate/ingress/push.proto"], &["proto"])
}
