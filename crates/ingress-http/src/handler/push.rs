// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use bytes::Bytes;
use bytestring::ByteString;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use prost::Message as _;
use tracing::trace;

use restate_storage_api::deduplication_table::DedupInformation;
use restate_types::Scope;
use restate_types::identifiers::{
    InvocationId, PartitionProcessorRpcRequestId, WithPartitionKey, partitioner,
};
use restate_types::invocation::{
    Header, InvocationTarget, InvocationTargetType, ServiceInvocation, Source,
};
use restate_types::limit_key::LimitKey;
use restate_types::schema::invocation_target::InvocationTargetResolver;
use restate_types::time::MillisSinceEpoch;
use restate_wal_protocol::{Command, Destination, Envelope, Source as EnvelopeSource};

use super::{Handler, HandlerError};
use crate::RequestDispatcher;
use proto::{BatchPushRequest, BatchPushResponse, PushItem};

/// Generated prost types for the `/restate/push` proto. Lives next to the handler
/// instead of as a top-level crate module since nothing else in the crate consumes it.
mod proto {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/restate.ingress.push.v1.rs"));
}

const APPLICATION_PROTO: &str = "application/proto";
const APPLICATION_PROTOBUF: &str = "application/protobuf";
const APPLICATION_JSON: &str = "application/json";

#[derive(Copy, Clone)]
enum WireFormat {
    Proto,
    Json,
}

impl WireFormat {
    fn as_content_type(self) -> HeaderValue {
        match self {
            WireFormat::Proto => HeaderValue::from_static(APPLICATION_PROTO),
            WireFormat::Json => HeaderValue::from_static(APPLICATION_JSON),
        }
    }
}

fn negotiate(headers: &http::HeaderMap) -> Result<WireFormat, HandlerError> {
    let raw = headers
        .get(header::CONTENT_TYPE)
        .ok_or_else(|| HandlerError::UnsupportedMediaType(String::from("(missing)")))?;
    let s = raw
        .to_str()
        .map_err(|e| HandlerError::BadHeader(header::CONTENT_TYPE, e))?;
    let main = s.split(';').next().unwrap_or(s).trim();
    match main {
        APPLICATION_PROTO | APPLICATION_PROTOBUF => Ok(WireFormat::Proto),
        APPLICATION_JSON => Ok(WireFormat::Json),
        other => Err(HandlerError::UnsupportedMediaType(other.to_owned())),
    }
}

impl<Schemas, Dispatcher> Handler<Schemas, Dispatcher>
where
    Schemas: InvocationTargetResolver + Clone + Send + Sync + 'static,
    Dispatcher: RequestDispatcher + Clone + Send + Sync + 'static,
{
    pub(crate) async fn handle_push<B: http_body::Body>(
        mut self,
        req: Request<B>,
    ) -> Result<Response<Full<Bytes>>, HandlerError>
    where
        <B as http_body::Body>::Error: std::error::Error + Send + Sync + 'static,
    {
        if req.method() != Method::POST {
            return Err(HandlerError::MethodNotAllowed);
        }

        let wire = negotiate(req.headers())?;

        let (_parts, body) = req.into_parts();
        let body = body
            .collect()
            .await
            .map_err(|e| HandlerError::Body(e.into()))?
            .to_bytes();

        let request: BatchPushRequest = match wire {
            WireFormat::Proto => BatchPushRequest::decode(body)?,
            WireFormat::Json => serde_json::from_slice(&body).map_err(HandlerError::BadJson)?,
        };

        if request.items.is_empty() {
            return Err(HandlerError::BatchEmpty);
        }

        let producer_id = decode_producer_id(&request.producer_id)?;

        trace!(items = request.items.len(), %producer_id, "Building push batch");

        let schema = self.schemas.live_load().clone();
        let envelopes = request
            .items
            .into_iter()
            .enumerate()
            .map(|(idx, item)| build_envelope(&schema, idx, producer_id, item))
            .collect::<Result<Vec<_>, _>>()?;

        // Single Future for the whole batch — the dispatcher owns the ingest
        // scheduling and commit synchronization.
        self.dispatcher
            .push_batch(envelopes)
            .await
            .map_err(|err| HandlerError::IngestionFailed {
                message: err.to_string(),
            })?;

        let resp = BatchPushResponse::default();
        let body = match wire {
            WireFormat::Proto => Bytes::from(resp.encode_to_vec()),
            WireFormat::Json => Bytes::from(
                serde_json::to_vec(&resp).expect("BatchPushResponse JSON encoding cannot fail"),
            ),
        };

        Ok(Response::builder()
            .status(StatusCode::ACCEPTED)
            .header(header::CONTENT_TYPE, wire.as_content_type())
            .body(Full::new(body))
            .expect("constructing 202 response cannot fail"))
    }
}

fn build_envelope<S>(
    schema: &S,
    _idx: usize,
    producer_id: u128,
    item: PushItem,
) -> Result<Envelope, HandlerError>
where
    S: InvocationTargetResolver,
{
    let proto_target = item.target.ok_or(HandlerError::MissingTarget)?;
    let target_meta = schema
        .resolve_latest_invocation_target(&proto_target.service, &proto_target.handler)
        .ok_or_else(|| {
            HandlerError::ServiceHandlerNotFound(
                proto_target.service.clone(),
                proto_target.handler.clone(),
            )
        })?;

    // Server-side resolution of the concrete InvocationTarget shape from schema metadata.
    let invocation_target = match target_meta.target_ty {
        InvocationTargetType::Service => {
            if proto_target.key.is_some() {
                return Err(HandlerError::BadPath(format!(
                    "service '{}' is not keyed; remove `key`",
                    proto_target.service
                )));
            }
            InvocationTarget::service(proto_target.service.clone(), proto_target.handler.clone())
        }
        InvocationTargetType::VirtualObject(handler_ty) => {
            let key = proto_target.key.clone().ok_or_else(|| {
                HandlerError::BadPath(format!(
                    "virtual object '{}' requires `key`",
                    proto_target.service
                ))
            })?;
            InvocationTarget::virtual_object(
                proto_target.service.clone(),
                key,
                proto_target.handler.clone(),
                handler_ty,
            )
        }
        InvocationTargetType::Workflow(handler_ty) => {
            let key = proto_target.key.clone().ok_or_else(|| {
                HandlerError::BadPath(format!(
                    "workflow '{}' requires `key`",
                    proto_target.service
                ))
            })?;
            InvocationTarget::workflow(
                proto_target.service.clone(),
                key,
                proto_target.handler.clone(),
                handler_ty,
            )
        }
    };
    let scope = proto_target
        .scope
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(Scope::new);
    let invocation_target = invocation_target.with_scope(scope);

    // Retention is always schema-resolved (the producer doesn't know or care).
    let invocation_retention = target_meta.compute_retention(false);

    // producer_id is mandatory and is always carried for observability. Dedup
    // information is added per-item iff `sequence_number` is also present;
    // items without a sequence_number get a fresh InvocationId.
    let idempotency_key: Option<ByteString> = item.idempotency_key.map(ByteString::from);
    let invocation_id = if let Some(seq) = item.sequence_number {
        let seed = PushPartitionKeySeed {
            producer: &producer_id,
            sequence_number: &seq,
        };
        InvocationId::generate_or_else(&invocation_target, idempotency_key.as_deref(), || {
            partitioner::HashPartitioner::compute_partition_key(seed)
        })
    } else {
        InvocationId::generate(&invocation_target, idempotency_key.as_deref())
    };

    let mut si = Box::new(ServiceInvocation::initialize(
        invocation_id,
        invocation_target,
        Source::Ingress(PartitionProcessorRpcRequestId::default()),
    ));
    si.argument = item.body;
    si.headers = item
        .headers
        .into_iter()
        .map(|h| Header::new(h.name, h.value))
        .collect();
    si.idempotency_key = idempotency_key;
    si.execution_time = item
        .execution_time_unix_millis
        .map(MillisSinceEpoch::from);
    si.limit_key = item
        .limit_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<LimitKey<_>>())
        .transpose()
        .map_err(|e| HandlerError::InvalidLimitKey(e.to_string()))?
        .unwrap_or(LimitKey::None);
    si.with_retention(invocation_retention);

    let dedup_info = item
        .sequence_number
        .map(|seq| DedupInformation::producer(producer_id, seq));
    let header = restate_wal_protocol::Header {
        source: EnvelopeSource::Ingress {},
        dest: Destination::Processor {
            partition_key: si.partition_key(),
            dedup: dedup_info,
        },
    };

    Ok(Envelope::new(header, Command::Invoke(si)))
}

fn decode_producer_id(raw: &Bytes) -> Result<u128, HandlerError> {
    if raw.len() != 16 {
        return Err(HandlerError::BadProducerId(raw.len()));
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(raw);
    Ok(u128::from_be_bytes(buf))
}

#[derive(std::hash::Hash)]
struct PushPartitionKeySeed<'a> {
    producer: &'a u128,
    sequence_number: &'a u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockRequestDispatcher;
    use crate::mocks::MockSchemas;
    use super::proto::{
        BatchPushRequest, BatchPushResponse, Header as ProtoHeader,
        InvocationTarget as ProtoTarget, PushItem,
    };
    use bytes::Bytes;
    use futures::FutureExt;
    use http::{Method, Request, StatusCode, header};
    use http_body_util::{BodyExt, Full};
    use restate_core::TestCoreEnv;
    use restate_test_util::assert_eq;
    use restate_types::invocation::InvocationTargetType;
    use restate_types::live::Live;
    use restate_types::net::address::SocketAddress;
    use restate_types::schema::invocation_target::InvocationTargetMetadata;
    use restate_wal_protocol::Command;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;
    use tracing_test::traced_test;

    fn target_service(service: &str, handler: &str) -> ProtoTarget {
        ProtoTarget {
            service: service.to_string(),
            handler: handler.to_string(),
            key: None,
            scope: None,
        }
    }

    fn schemas() -> MockSchemas {
        MockSchemas::default().with_service_and_target(
            "greeter.Greeter",
            "greet",
            InvocationTargetMetadata::mock(InvocationTargetType::Service),
        )
    }

    fn item(seq: Option<u64>) -> PushItem {
        PushItem {
            sequence_number: seq,
            target: Some(target_service("greeter.Greeter", "greet")),
            headers: vec![ProtoHeader {
                name: "x-test".into(),
                value: "1".into(),
            }],
            idempotency_key: None,
            execution_time_unix_millis: None,
            limit_key: None,
            body: Bytes::from_static(br#"{"person":"Francesco"}"#),
        }
    }

    fn pid_bytes(pid: u128) -> Bytes {
        Bytes::copy_from_slice(&pid.to_be_bytes())
    }

    /// `MockRequestDispatcher::expect_push_batch` records the envelopes and returns Ok.
    fn dispatcher_recording(
        captured: Arc<Mutex<Vec<Envelope>>>,
    ) -> MockRequestDispatcher {
        let mut d = MockRequestDispatcher::default();
        d.expect_push_batch().returning(move |envs| {
            captured.lock().unwrap().extend(envs);
            async { Ok(()) }.boxed()
        });
        d
    }

    fn dispatcher_failing() -> MockRequestDispatcher {
        let mut d = MockRequestDispatcher::default();
        d.expect_push_batch().returning(|_| {
            async {
                Err(crate::RequestDispatcherError::Internal(anyhow::anyhow!(
                    "partition unavailable"
                )))
            }
            .boxed()
        });
        d
    }

    /// Drives `handle_push` against the chosen dispatcher mock; returns the response and
    /// any envelopes the dispatcher recorded.
    async fn drive(
        req: Request<Full<Bytes>>,
        dispatcher: MockRequestDispatcher,
        captured: Arc<Mutex<Vec<Envelope>>>,
    ) -> (http::Response<Full<Bytes>>, Vec<Envelope>) {
        let _env = TestCoreEnv::create_with_single_node(1, 1).await;

        let mut req = req;
        req.extensions_mut()
            .insert(crate::ConnectInfo::new(SocketAddress::Anonymous));
        req.extensions_mut().insert(opentelemetry::Context::new());

        let handler = Handler::new(Live::from_value(schemas()), Arc::new(dispatcher));
        let response = handler.oneshot(req).await.unwrap();
        let appended = std::mem::take(&mut *captured.lock().unwrap());
        (response, appended)
    }

    #[restate_core::test]
    #[traced_test]
    async fn happy_path_proto_with_dedup() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let body = BatchPushRequest {
            producer_id: pid_bytes(0xdead_beef_cafe_babe_0123_4567_89ab_cdefu128),
            items: vec![item(Some(7))],
        }
        .encode_to_vec();

        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body)))
            .unwrap();

        let (resp, appended) = drive(req, dispatcher_recording(captured.clone()), captured).await;

        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            APPLICATION_PROTO
        );
        let resp_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(BatchPushResponse::decode(resp_bytes).is_ok());
        assert_eq!(appended.len(), 1);
        let env = &appended[0];
        match &env.header.dest {
            restate_wal_protocol::Destination::Processor { dedup: Some(_), .. } => {}
            other => panic!("expected dedup, got {other:?}"),
        }
        match &env.command {
            Command::Invoke(si) => {
                assert_eq!(si.invocation_target.service_name(), "greeter.Greeter");
                assert_eq!(si.invocation_target.handler_name(), "greet");
                assert_eq!(si.headers.len(), 1);
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[restate_core::test]
    #[traced_test]
    async fn happy_path_json_without_per_item_dedup() {
        // producer_id is mandatory and present; this item has no sequence_number,
        // so it should be ingested with no DedupInformation (producer is still
        // recorded for observability via the envelope's source attribution).
        let captured = Arc::new(Mutex::new(Vec::new()));
        let body = serde_json::to_vec(&BatchPushRequest {
            producer_id: pid_bytes(7),
            items: vec![item(None)],
        })
        .unwrap();

        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_JSON)
            .body(Full::new(Bytes::from(body)))
            .unwrap();

        let (resp, appended) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert_eq!(appended.len(), 1);
        match &appended[0].header.dest {
            restate_wal_protocol::Destination::Processor { dedup: None, .. } => {}
            other => panic!("expected no dedup, got {other:?}"),
        }
    }

    #[restate_core::test]
    #[traced_test]
    async fn dedup_skipped_per_item_when_seq_missing() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let body = BatchPushRequest {
            producer_id: pid_bytes(99),
            items: vec![item(None), item(Some(2))],
        }
        .encode_to_vec();

        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body)))
            .unwrap();

        let (resp, appended) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert_eq!(appended.len(), 2);
        let dedup0 = matches!(
            &appended[0].header.dest,
            restate_wal_protocol::Destination::Processor { dedup: Some(_), .. }
        );
        let dedup1 = matches!(
            &appended[1].header.dest,
            restate_wal_protocol::Destination::Processor { dedup: Some(_), .. }
        );
        assert!(!dedup0, "item 0 (no seq) must not have DedupInformation");
        assert!(dedup1, "item 1 (with seq) must have DedupInformation");
    }

    #[restate_core::test]
    #[traced_test]
    async fn unsupported_media_type() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Full::new(Bytes::from_static(b"foo")))
            .unwrap();
        let (resp, _) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[restate_core::test]
    #[traced_test]
    async fn bad_protobuf() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from_static(
                b"\xff\xff\xff\xff\xff\xff\xff",
            )))
            .unwrap();
        let (resp, _) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[restate_core::test]
    #[traced_test]
    async fn bad_producer_id_length() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let body = BatchPushRequest {
            producer_id: Bytes::from_static(&[0u8; 8]),
            items: vec![item(Some(1))],
        }
        .encode_to_vec();

        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let (resp, _) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[restate_core::test]
    #[traced_test]
    async fn missing_producer_id_rejected() {
        // producer_id is mandatory; an absent (proto3 default = empty bytes) value
        // must yield 400, since 0-byte length is not a valid 16-byte u128.
        let captured = Arc::new(Mutex::new(Vec::new()));
        let body = BatchPushRequest {
            producer_id: Bytes::new(),
            items: vec![item(Some(1))],
        }
        .encode_to_vec();
        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let (resp, _) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[restate_core::test]
    #[traced_test]
    async fn empty_batch() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let body = BatchPushRequest::default().encode_to_vec();
        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let (resp, _) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[restate_core::test]
    #[traced_test]
    async fn method_not_allowed_on_get() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://localhost/restate/push")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let (resp, _) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[restate_core::test]
    #[traced_test]
    async fn dispatcher_error_returns_500() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let body = BatchPushRequest {
            producer_id: pid_bytes(1),
            items: vec![item(Some(1))],
        }
        .encode_to_vec();
        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let (resp, _) = drive(req, dispatcher_failing(), captured).await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[restate_core::test]
    #[traced_test]
    async fn deterministic_partition_key_for_same_producer_and_seq() {
        // Replays of the same (producer_id, sequence_number) must route to the same
        // partition so the partition processor can dedup via DedupInformation.
        let captured1 = Arc::new(Mutex::new(Vec::new()));
        let captured2 = Arc::new(Mutex::new(Vec::new()));
        let body = BatchPushRequest {
            producer_id: pid_bytes(42),
            items: vec![item(Some(99))],
        }
        .encode_to_vec();

        let req1 = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body.clone())))
            .unwrap();
        let req2 = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let (_, appended1) =
            drive(req1, dispatcher_recording(captured1.clone()), captured1).await;
        let (_, appended2) =
            drive(req2, dispatcher_recording(captured2.clone()), captured2).await;
        assert_eq!(
            appended1[0].partition_key(),
            appended2[0].partition_key(),
            "partition_key must be deterministic"
        );
    }

    #[restate_core::test]
    #[traced_test]
    async fn rejects_key_for_non_keyed_service() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut t = target_service("greeter.Greeter", "greet");
        t.key = Some("oops".into());
        let mut it = item(None);
        it.target = Some(t);

        let body = BatchPushRequest {
            producer_id: pid_bytes(7),
            items: vec![it],
        }
        .encode_to_vec();
        let req = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/restate/push")
            .header(header::CONTENT_TYPE, APPLICATION_PROTO)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let (resp, _) = drive(req, dispatcher_recording(captured.clone()), captured).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
