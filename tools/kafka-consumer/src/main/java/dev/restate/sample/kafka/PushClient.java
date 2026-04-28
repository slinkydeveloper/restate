// Copyright (c) 2023 - 2026 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

package dev.restate.sample.kafka;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpRequest.BodyPublishers;
import java.net.http.HttpResponse;
import java.net.http.HttpResponse.BodyHandlers;

import restate.ingress.push.v1.Push.BatchPushRequest;

/**
 * Posts a {@link BatchPushRequest} to {@code POST /restate/push} with
 * {@code Content-Type: application/proto}. Returns the HTTP status code.
 *
 * <p>Uses HTTP/2 — the underlying {@link HttpClient} is configured with
 * {@code HttpClient.Version.HTTP_2}; restate-server (hyper-util) negotiates HTTP/1.1
 * ↔ HTTP/2 transparently via prior-knowledge or {@code Upgrade}.
 */
public final class PushClient {

    private static final String CONTENT_TYPE_PROTO = "application/proto";

    private final HttpClient http;
    private final URI uri;

    public PushClient(HttpClient http, URI uri) {
        this.http = http;
        this.uri = uri;
    }

    public int send(BatchPushRequest batch) throws IOException, InterruptedException {
        HttpRequest req = HttpRequest.newBuilder(uri)
                .version(HttpClient.Version.HTTP_2)
                .header("Content-Type", CONTENT_TYPE_PROTO)
                .POST(BodyPublishers.ofByteArray(batch.toByteArray()))
                .build();
        HttpResponse<byte[]> resp = http.send(req, BodyHandlers.ofByteArray());
        return resp.statusCode();
    }
}
