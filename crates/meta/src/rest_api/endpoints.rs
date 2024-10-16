// Copyright (c) 2023 -  Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::sync::Arc;

use super::error::*;
use super::state::*;

use restate_meta_rest_model::endpoints::*;
use restate_schema_api::endpoint::EndpointMetadataResolver;
use restate_service_client::ServiceEndpointAddress;
use restate_service_protocol::discovery::DiscoverEndpoint;
use restate_types::identifiers::InvalidLambdaARN;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{http, Json};
use okapi_operation::*;
use serde::Deserialize;

/// Create service endpoint and return discovered services.
#[openapi(
    summary = "Create service endpoint",
    description = "Create service endpoint. Restate will invoke the endpoint to gather additional information required for registration, such as the services exposed by the service endpoint and their Protobuf descriptor. If the service endpoint is already registered, this method will fail unless `force` is set to `true`.",
    operation_id = "create_service_endpoint",
    tags = "service_endpoint",
    responses(
        ignore_return_type = true,
        response(
            status = "201",
            description = "Created",
            content = "Json<RegisterServiceEndpointResponse>",
        ),
        from_type = "MetaApiError",
    )
)]
pub async fn create_service_endpoint<S, W>(
    State(state): State<Arc<RestEndpointState<S, W>>>,
    #[request_body(required = true)] Json(payload): Json<RegisterServiceEndpointRequest>,
) -> Result<impl IntoResponse, MetaApiError> {
    let address = match payload.endpoint_metadata {
        RegisterServiceEndpointMetadata::Http { uri } => {
            ServiceEndpointAddress::Http(uri, Default::default())
        }
        RegisterServiceEndpointMetadata::Lambda {
            arn,
            assume_role_arn,
        } => ServiceEndpointAddress::Lambda(
            arn.parse()
                .map_err(|e: InvalidLambdaARN| MetaApiError::InvalidField("arn", e.to_string()))?,
            assume_role_arn.map(Into::into),
        ),
    };
    let endpoint = DiscoverEndpoint::new(
        address,
        payload.additional_headers.unwrap_or_default().into(),
    );
    let registration_result = state
        .meta_handle()
        .register_endpoint(endpoint, payload.force)
        .await?;

    let response_body = RegisterServiceEndpointResponse {
        id: registration_result.endpoint,
        services: registration_result
            .services
            .into_iter()
            .map(|(name, revision)| RegisterServiceResponse { name, revision })
            .collect(),
    };

    Ok((
        StatusCode::CREATED,
        [(
            http::header::LOCATION,
            format!("/endpoints/{}", response_body.id),
        )],
        Json(response_body),
    ))
}

/// Return discovered endpoints.
#[openapi(
    summary = "Get service endpoint",
    description = "Get service endpoint metadata",
    operation_id = "get_service_endpoint",
    tags = "service_endpoint",
    parameters(path(
        name = "endpoint",
        description = "Endpoint identifier",
        schema = "std::string::String"
    ))
)]
pub async fn get_service_endpoint<S: EndpointMetadataResolver, W>(
    State(state): State<Arc<RestEndpointState<S, W>>>,
    Path(endpoint_id): Path<String>,
) -> Result<Json<ServiceEndpointResponse>, MetaApiError> {
    let (endpoint_meta, services) = state
        .schemas()
        .get_endpoint_and_services(&endpoint_id)
        .ok_or_else(|| MetaApiError::ServiceEndpointNotFound(endpoint_id.clone()))?;

    Ok(ServiceEndpointResponse {
        id: endpoint_id,
        service_endpoint: endpoint_meta.into(),
        services: services
            .into_iter()
            .map(|(name, revision)| RegisterServiceResponse { name, revision })
            .collect(),
    }
    .into())
}

/// List services
#[openapi(
    summary = "List service endpoints",
    description = "List all registered endpoints.",
    operation_id = "list_service_endpoints",
    tags = "service_endpoint"
)]
pub async fn list_service_endpoints<S: EndpointMetadataResolver, W>(
    State(state): State<Arc<RestEndpointState<S, W>>>,
) -> Json<ListServiceEndpointsResponse> {
    ListServiceEndpointsResponse {
        endpoints: state
            .schemas()
            .get_endpoints()
            .into_iter()
            .map(|(endpoint_meta, services)| ServiceEndpointResponse {
                id: endpoint_meta.id(),
                service_endpoint: endpoint_meta.into(),
                services: services
                    .into_iter()
                    .map(|(name, revision)| RegisterServiceResponse { name, revision })
                    .collect(),
            })
            .collect(),
    }
    .into()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteServiceEndpointParams {
    pub force: Option<bool>,
}

/// Discover endpoint and return discovered endpoints.
#[openapi(
    summary = "Delete service endpoint",
    description = "Delete service endpoint. Currently it's supported to remove a service endpoint only using the force flag",
    operation_id = "delete_service_endpoint",
    tags = "service_endpoint",
    parameters(
        path(
            name = "endpoint",
            description = "Endpoint identifier",
            schema = "std::string::String"
        ),
        query(
            name = "force",
            description = "If true, the service endpoint will be forcefully deleted. This might break in-flight invocations, use with caution.",
            required = false,
            style = "simple",
            allow_empty_value = false,
            schema = "bool",
        )
    ),
    responses(
        ignore_return_type = true,
        response(
            status = "202",
            description = "Accepted",
            content = "okapi_operation::Empty",
        ),
        response(
            status = "501",
            description = "Not implemented. Only using the force flag is supported at the moment.",
            content = "okapi_operation::Empty",
        ),
        from_type = "MetaApiError",
    )
)]
pub async fn delete_service_endpoint<S, W>(
    State(state): State<Arc<RestEndpointState<S, W>>>,
    Path(endpoint_id): Path<String>,
    Query(DeleteServiceEndpointParams { force }): Query<DeleteServiceEndpointParams>,
) -> Result<StatusCode, MetaApiError> {
    if let Some(true) = force {
        state.meta_handle().remove_endpoint(endpoint_id).await?;
        Ok(StatusCode::ACCEPTED)
    } else {
        Ok(StatusCode::NOT_IMPLEMENTED)
    }
}
