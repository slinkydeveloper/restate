// Copyright (c) 2023 -  Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use super::command_interpreter::CommandInterpreter;
use super::{AckCommand, AckMode, Effects, Error};

use crate::partition::state_machine::commands::DeduplicationSource;
use crate::partition::storage::Transaction;
use restate_storage_api::deduplication_table::SequenceNumberSource;
use restate_types::identifiers::FullInvocationId;
use restate_types::invocation::SpanRelation;
use restate_types::journal::raw::RawEntryCodec;

#[derive(Debug)]
pub(crate) struct DeduplicatingCommandInterpreter<Codec> {
    state_machine: CommandInterpreter<Codec>,
}

impl<Codec> DeduplicatingCommandInterpreter<Codec> {
    pub(crate) fn new(state_machine: CommandInterpreter<Codec>) -> Self {
        DeduplicatingCommandInterpreter { state_machine }
    }
}

impl<Codec> DeduplicatingCommandInterpreter<Codec>
where
    Codec: RawEntryCodec,
{
    pub(crate) async fn on_apply<TransactionType: restate_storage_api::Transaction>(
        &mut self,
        command: AckCommand,
        effects: &mut Effects,
        transaction: &mut Transaction<TransactionType>,
    ) -> Result<(Option<FullInvocationId>, SpanRelation), Error> {
        let (fsm_command, ack_mode) = command.into_inner();

        match ack_mode {
            AckMode::Ack(ack_target) => {
                effects.send_ack_response(ack_target.acknowledge());
            }
            AckMode::Dedup(deduplication_source) => {
                let (source, seq_number) = match deduplication_source {
                    DeduplicationSource::Shuffle {
                        seq_number,
                        producing_partition_id,
                        ..
                    } => (
                        SequenceNumberSource::Partition(producing_partition_id),
                        seq_number,
                    ),
                    DeduplicationSource::Ingress {
                        seq_number,
                        ref source_id,
                        ..
                    } => (
                        SequenceNumberSource::Ingress(source_id.clone().into()),
                        seq_number,
                    ),
                };

                if let Some(last_known_seq_number) =
                    transaction.load_dedup_seq_number(source.clone()).await?
                {
                    if seq_number <= last_known_seq_number {
                        effects.send_ack_response(
                            deduplication_source.duplicate(last_known_seq_number),
                        );
                        return Ok((None, SpanRelation::None));
                    }
                }

                transaction.store_dedup_seq_number(source, seq_number).await;
                effects.send_ack_response(deduplication_source.acknowledge());
            }
            AckMode::None => {}
        }

        self.state_machine
            .on_apply(fsm_command, effects, transaction)
            .await
    }
}
