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

import java.net.URI;
import java.net.http.HttpClient;
import java.time.Duration;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Properties;

import org.apache.kafka.clients.consumer.ConsumerConfig;
import org.apache.kafka.clients.consumer.ConsumerRecord;
import org.apache.kafka.clients.consumer.ConsumerRecords;
import org.apache.kafka.clients.consumer.KafkaConsumer;
import org.apache.kafka.clients.consumer.OffsetAndMetadata;
import org.apache.kafka.common.TopicPartition;
import org.apache.kafka.common.serialization.ByteArrayDeserializer;

/**
 * Bring-Your-Own Kafka consumer that drives Restate's POC POST /restate/push endpoint.
 *
 * <p>Architecture:
 * <ul>
 *   <li>One Kafka {@link TopicPartition} = one {@link
 *       restate.ingress.push.v1.Push.BatchPushRequest}, so {@code producer_id} can sit on
 *       the batch (per-partition) and {@code sequence_number = offset} stays unique.</li>
 *   <li>Speaks HTTP/2 to Restate via {@link HttpClient}; restate-server's hyper stack
 *       handles HTTP/1.1↔HTTP/2 negotiation transparently.</li>
 *   <li>Commits offsets only on HTTP 202 from Restate. Failures back off and retry —
 *       safe because the dedup info on the envelope guarantees exactly-once on retries.</li>
 *   <li>Uses the classic (eager) consumer-group rebalance protocol by default for
 *       broad broker compatibility. Set {@code KAFKA_GROUP_PROTOCOL=consumer} to opt into
 *       the KIP-848 server-side rebalance protocol (requires Apache Kafka >= 4.0 /
 *       Confluent Platform >= 8).</li>
 * </ul>
 */
public final class App {

    public static void main(String[] args) throws Exception {
        String bootstrap = required("KAFKA_BOOTSTRAP");
        String topic = required("KAFKA_TOPIC");
        String group = required("KAFKA_GROUP");
        String restateUrl = required("RESTATE_URL");
        String service = required("RESTATE_SERVICE");
        String handler = required("RESTATE_HANDLER");

        Properties props = new Properties();
        props.put(ConsumerConfig.BOOTSTRAP_SERVERS_CONFIG, bootstrap);
        props.put(ConsumerConfig.GROUP_ID_CONFIG, group);
        // Default to "classic" so any reasonably current broker works out of the box.
        // Set KAFKA_GROUP_PROTOCOL=consumer to opt into KIP-848 (Apache 4+ / CP 8+).
        props.put(ConsumerConfig.GROUP_PROTOCOL_CONFIG,
                System.getenv().getOrDefault("KAFKA_GROUP_PROTOCOL", "classic"));
        props.put(ConsumerConfig.ENABLE_AUTO_COMMIT_CONFIG, "false");
        props.put(ConsumerConfig.AUTO_OFFSET_RESET_CONFIG, "earliest");
        props.put(ConsumerConfig.KEY_DESERIALIZER_CLASS_CONFIG,
                ByteArrayDeserializer.class.getName());
        props.put(ConsumerConfig.VALUE_DESERIALIZER_CLASS_CONFIG,
                ByteArrayDeserializer.class.getName());

        HttpClient http = HttpClient.newBuilder()
                .version(HttpClient.Version.HTTP_2)
                .connectTimeout(Duration.ofSeconds(5))
                .build();
        URI pushUri = URI.create(restateUrl.replaceAll("/$", "") + "/restate/push");
        PushClient client = new PushClient(http, pushUri);
        EnvelopeBuilder builder = new EnvelopeBuilder(service, handler, group);

        try (KafkaConsumer<byte[], byte[]> consumer = new KafkaConsumer<>(props)) {
            consumer.subscribe(List.of(topic));

            while (!Thread.interrupted()) {
                ConsumerRecords<byte[], byte[]> records = consumer.poll(Duration.ofMillis(500));
                if (records.isEmpty()) {
                    continue;
                }
                Map<TopicPartition, OffsetAndMetadata> commits = new HashMap<>();
                boolean allOk = true;
                for (TopicPartition tp : records.partitions()) {
                    List<ConsumerRecord<byte[], byte[]>> partRecords = records.records(tp);
                    int status = client.send(builder.buildBatch(tp, partRecords));
                    if (status == 202) {
                        long lastOffset = partRecords.get(partRecords.size() - 1).offset();
                        commits.put(tp, new OffsetAndMetadata(lastOffset + 1));
                    } else {
                        System.err.println(
                                "[push-producer] push failed on " + tp + " (HTTP " + status + ")");
                        allOk = false;
                    }
                }
                if (!commits.isEmpty()) {
                    consumer.commitSync(commits);
                }
                if (!allOk) {
                    Thread.sleep(1_000);
                }
            }
        }
    }

    private static String required(String envName) {
        String v = System.getenv(envName);
        if (v == null || v.isBlank()) {
            throw new IllegalStateException("Missing required env var: " + envName);
        }
        return v;
    }
}
