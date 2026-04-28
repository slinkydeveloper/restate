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

import java.nio.ByteBuffer;
import java.util.List;
import java.util.UUID;

import com.google.protobuf.ByteString;

import org.apache.kafka.clients.consumer.ConsumerRecord;
import org.apache.kafka.common.TopicPartition;

import restate.ingress.push.v1.Push.BatchPushRequest;
import restate.ingress.push.v1.Push.Header;
import restate.ingress.push.v1.Push.InvocationTarget;
import restate.ingress.push.v1.Push.PushItem;

/**
 * Builds one {@link BatchPushRequest} per Kafka {@link TopicPartition}.
 *
 * <p>The {@code producer_id} sits on the batch and is derived deterministically from
 * {@code (group, topic, partition)} — pinning it at the partition level guarantees that
 * each {@code (producer_id, sequence_number=offset)} tuple is globally unique, which is
 * what Restate's partition processor needs to dedup exactly-once.
 *
 * <p>The runtime resolves the service type and retention durations from the schema
 * registry, so we only ship service/handler/key here.
 */
public final class EnvelopeBuilder {

    private final String serviceName;
    private final String handlerName;
    private final String group;

    public EnvelopeBuilder(String serviceName, String handlerName, String group) {
        this.serviceName = serviceName;
        this.handlerName = handlerName;
        this.group = group;
    }

    public BatchPushRequest buildBatch(
            TopicPartition tp, List<ConsumerRecord<byte[], byte[]>> records) {
        BatchPushRequest.Builder b = BatchPushRequest.newBuilder()
                .setProducerId(ByteString.copyFrom(deriveProducerId(group, tp)));
        InvocationTarget target = InvocationTarget.newBuilder()
                .setService(serviceName)
                .setHandler(handlerName)
                .build();
        for (ConsumerRecord<byte[], byte[]> r : records) {
            PushItem.Builder item = PushItem.newBuilder()
                    .setSequenceNumber(r.offset())
                    .setTarget(target);
            if (r.value() != null) {
                item.setBody(ByteString.copyFrom(r.value()));
            }
            item.addHeaders(Header.newBuilder().setName("kafka.topic").setValue(r.topic()));
            item.addHeaders(Header.newBuilder().setName("kafka.partition")
                    .setValue(Integer.toString(r.partition())));
            item.addHeaders(Header.newBuilder().setName("kafka.offset")
                    .setValue(Long.toString(r.offset())));
            b.addItems(item.build());
        }
        return b.build();
    }

    static byte[] deriveProducerId(String group, TopicPartition tp) {
        UUID u = UUID.nameUUIDFromBytes(
                (group + "/" + tp.topic() + "/" + tp.partition()).getBytes());
        ByteBuffer bb = ByteBuffer.allocate(16);
        bb.putLong(u.getMostSignificantBits());
        bb.putLong(u.getLeastSignificantBits());
        return bb.array();
    }
}
