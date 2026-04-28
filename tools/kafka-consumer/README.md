# kafka-consumer

Polls a Kafka topic and forwards each batch to Restate via the
`POST /restate/push` ingress endpoint — a protobuf API that accepts a
`BatchPushRequest` of invocations and returns 202 once they're all appended.

## Build

```
mvn -B compile jib:dockerBuild
```

Produces `kafka-consumer:latest` in the local Docker daemon.

## Run

The repo includes a `docker-compose.yml` with Kafka + a topic seed + this
consumer. Restate is expected to run on the host (default
`http://host.docker.internal:8080`).

```
docker compose up
```

Override anything via env on the `kafka-consumer` service in the compose file
(`KAFKA_TOPIC`, `RESTATE_URL`, `RESTATE_SERVICE`, `RESTATE_HANDLER`, ...).

## Proto sync

`src/main/protobuf/restate/ingress/push.proto` is a symlink to
`crates/ingress-http/proto/restate/ingress/push.proto` — the runtime is the
single source of truth.
