// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use super::{RequestDispatcher, RequestDispatcherError};

use futures::future::try_join_all;
use restate_core::network::TransportConnect;
use restate_ingestion_client::IngestionClient;
use restate_types::identifiers::{InvocationId, PartitionProcessorRpcRequestId, WithInvocationId};
use restate_types::identifiers::WithPartitionKey;
use restate_types::invocation::client::{
    AttachInvocationResponse, GetInvocationOutputResponse, InvocationClient, InvocationClientError,
    InvocationOutput, SubmittedInvocationNotification,
};
use restate_types::invocation::{InvocationQuery, InvocationRequest, InvocationResponse};
use restate_types::journal_v2::Signal;
use restate_types::retries::RetryPolicy;
use restate_wal_protocol::Envelope;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tracing::{Instrument, debug_span, trace};

pub struct InvocationClientRequestDispatcher<IC, T> {
    invocation_client: IC,
    ingestion_client: IngestionClient<T, Envelope>,
    retry_policy: RetryPolicy,
}

impl<IC: Clone, T: Clone> Clone for InvocationClientRequestDispatcher<IC, T> {
    fn clone(&self) -> Self {
        InvocationClientRequestDispatcher {
            invocation_client: self.invocation_client.clone(),
            ingestion_client: self.ingestion_client.clone(),
            retry_policy: self.retry_policy.clone(),
        }
    }
}

impl<IC, T> InvocationClientRequestDispatcher<IC, T> {
    pub fn new(invocation_client: IC, ingestion_client: IngestionClient<T, Envelope>) -> Self {
        Self {
            invocation_client,
            ingestion_client,
            // TODO figure out how to tune this?
            retry_policy: RetryPolicy::fixed_delay(Duration::from_millis(50), None),
        }
    }

    async fn execute_rpc<Fn, Fut, U>(
        &self,
        is_idempotent: bool,
        operation: Fn,
    ) -> Result<U, RequestDispatcherError>
    where
        Fn: FnMut() -> Fut,
        Fut: Future<Output = Result<U, InvocationClientError>>,
    {
        Ok(self
            .retry_policy
            .clone()
            .retry_if(operation, |e| {
                let retry = is_idempotent || e.is_safe_to_retry();

                if retry {
                    trace!("Retrying rpc because of error: {e}.");
                } else {
                    trace!("Rpc failed: {e}");
                }

                retry
            })
            .await
            .map_err(|e| e.into_inner())?)
    }
}

impl<IC, T> RequestDispatcher for InvocationClientRequestDispatcher<IC, T>
where
    IC: InvocationClient + Clone + Send + Sync + 'static,
    T: TransportConnect,
{
    async fn send(
        &self,
        invocation_request: Arc<InvocationRequest>,
    ) -> Result<SubmittedInvocationNotification, RequestDispatcherError> {
        let request_id = PartitionProcessorRpcRequestId::default();
        let is_idempotent = invocation_request.is_idempotent();
        self.execute_rpc(is_idempotent, || {
            self.invocation_client
                .append_invocation_and_wait_submit_notification(
                    request_id,
                    invocation_request.clone(),
                )
        })
        .instrument(debug_span!("send invocation", %request_id, invocation_id = %invocation_request.invocation_id()))
        .await
    }

    async fn call(
        &self,
        invocation_request: Arc<InvocationRequest>,
    ) -> Result<InvocationOutput, RequestDispatcherError> {
        let request_id = PartitionProcessorRpcRequestId::default();
        let is_idempotent = invocation_request.is_idempotent();
        self.execute_rpc(is_idempotent, || {
            self.invocation_client
                .append_invocation_and_wait_output(request_id, invocation_request.clone())
        })
        .instrument(debug_span!("call invocation", %request_id, invocation_id = %invocation_request.invocation_id()))
        .await
    }

    async fn attach_invocation(
        &self,
        invocation_query: InvocationQuery,
    ) -> Result<AttachInvocationResponse, RequestDispatcherError> {
        let request_id = PartitionProcessorRpcRequestId::default();
        self.execute_rpc(true, || {
            self.invocation_client
                .attach_invocation(request_id, invocation_query.clone())
        })
        .instrument(debug_span!("attach to invocation", %request_id, invocation_id = %invocation_query.to_invocation_id()))
        .await
    }

    async fn get_invocation_output(
        &self,
        invocation_query: InvocationQuery,
    ) -> Result<GetInvocationOutputResponse, RequestDispatcherError> {
        let request_id = PartitionProcessorRpcRequestId::default();
        self.execute_rpc(true, || {
            self.invocation_client
                .get_invocation_output(request_id, invocation_query.clone())
        })
        .instrument(debug_span!("get invocation output", %request_id, invocation_id = %invocation_query.to_invocation_id()))
        .await
    }

    async fn send_invocation_response(
        &self,
        invocation_response: InvocationResponse,
    ) -> Result<(), RequestDispatcherError> {
        let request_id = PartitionProcessorRpcRequestId::default();
        self.execute_rpc(true, || {
            self.invocation_client
                .append_invocation_response(request_id, invocation_response.clone())
        })
        .instrument(debug_span!("send invocation response", %request_id, invocation_id = %invocation_response.target.caller_id))
        .await
    }

    async fn send_signal(
        &self,
        target_invocation: InvocationId,
        signal: Signal,
    ) -> Result<(), RequestDispatcherError> {
        let request_id = PartitionProcessorRpcRequestId::default();
        self.execute_rpc(true, || {
            self.invocation_client
                .append_signal(request_id, target_invocation, signal.clone())
        })
            .instrument(debug_span!("send invocation response", %request_id, invocation_id = %target_invocation))
            .await
    }

    async fn push_batch(&self, envelopes: Vec<Envelope>) -> Result<(), RequestDispatcherError> {
        // Submit every envelope into the ingestion session manager, then await all
        // partition-processor commits as a single joined future. The caller hands
        // us the whole batch and gets back one Future for the lot.
        let mut client = self.ingestion_client.clone();
        let mut commits = Vec::with_capacity(envelopes.len());
        for env in envelopes {
            let pk = env.partition_key();
            let commit = client
                .ingest(pk, env)
                .await
                .map_err(|err| RequestDispatcherError::Internal(anyhow::Error::from(err)))?;
            commits.push(commit);
        }
        try_join_all(commits)
            .await
            .map_err(|err| RequestDispatcherError::Internal(anyhow::Error::from(err)))?;
        Ok(())
    }
}
