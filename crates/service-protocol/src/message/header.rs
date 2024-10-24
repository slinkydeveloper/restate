// Copyright (c) 2023 -  Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

const CUSTOM_MESSAGE_MASK: u16 = 0xFC00;
const VERSION_MASK: u64 = 0x03FF_0000_0000;
const COMPLETED_MASK: u64 = 0x0001_0000_0000;
const REQUIRES_ACK_MASK: u64 = 0x0001_0000_0000;

type MessageTypeId = u16;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Core,
    IO,
    State,
    Syscall,
    Custom,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MessageType {
    Start,
    Completion,
    Suspension,
    Error,
    PollInputStreamEntry,
    OutputStreamEntry,
    GetStateEntry,
    SetStateEntry,
    ClearStateEntry,
    SleepEntry,
    InvokeEntry,
    BackgroundInvokeEntry,
    AwakeableEntry,
    CompleteAwakeableEntry,
    Custom(u16),
}

impl MessageType {
    fn kind(&self) -> MessageKind {
        match self {
            MessageType::Start => MessageKind::Core,
            MessageType::Completion => MessageKind::Core,
            MessageType::Suspension => MessageKind::Core,
            MessageType::Error => MessageKind::Core,
            MessageType::PollInputStreamEntry => MessageKind::IO,
            MessageType::OutputStreamEntry => MessageKind::IO,
            MessageType::GetStateEntry => MessageKind::State,
            MessageType::SetStateEntry => MessageKind::State,
            MessageType::ClearStateEntry => MessageKind::State,
            MessageType::SleepEntry => MessageKind::Syscall,
            MessageType::InvokeEntry => MessageKind::Syscall,
            MessageType::BackgroundInvokeEntry => MessageKind::Syscall,
            MessageType::AwakeableEntry => MessageKind::Syscall,
            MessageType::CompleteAwakeableEntry => MessageKind::Syscall,
            MessageType::Custom(_) => MessageKind::Custom,
        }
    }

    fn has_completed_flag(&self) -> bool {
        matches!(
            self,
            MessageType::PollInputStreamEntry
                | MessageType::GetStateEntry
                | MessageType::SleepEntry
                | MessageType::InvokeEntry
                | MessageType::AwakeableEntry
        )
    }

    fn has_protocol_version(&self) -> bool {
        *self == MessageType::Start
    }

    fn has_requires_ack_flag(&self) -> bool {
        matches!(self, MessageType::Custom(_))
    }
}

const START_MESSAGE_TYPE: u16 = 0x0000;
const COMPLETION_MESSAGE_TYPE: u16 = 0x0001;
const SUSPENSION_MESSAGE_TYPE: u16 = 0x0002;
const ERROR_MESSAGE_TYPE: u16 = 0x0003;
const POLL_INPUT_STREAM_ENTRY_MESSAGE_TYPE: u16 = 0x0400;
const OUTPUT_STREAM_ENTRY_MESSAGE_TYPE: u16 = 0x0401;
const GET_STATE_ENTRY_MESSAGE_TYPE: u16 = 0x0800;
const SET_STATE_ENTRY_MESSAGE_TYPE: u16 = 0x0801;
const CLEAR_STATE_ENTRY_MESSAGE_TYPE: u16 = 0x0802;
const SLEEP_ENTRY_MESSAGE_TYPE: u16 = 0x0C00;
const INVOKE_ENTRY_MESSAGE_TYPE: u16 = 0x0C01;
const BACKGROUND_INVOKE_ENTRY_MESSAGE_TYPE: u16 = 0x0C02;
const AWAKEABLE_ENTRY_MESSAGE_TYPE: u16 = 0x0C03;
const COMPLETE_AWAKEABLE_ENTRY_MESSAGE_TYPE: u16 = 0x0C04;

impl From<MessageType> for MessageTypeId {
    fn from(mt: MessageType) -> Self {
        match mt {
            MessageType::Start => START_MESSAGE_TYPE,
            MessageType::Completion => COMPLETION_MESSAGE_TYPE,
            MessageType::Suspension => SUSPENSION_MESSAGE_TYPE,
            MessageType::Error => ERROR_MESSAGE_TYPE,
            MessageType::PollInputStreamEntry => POLL_INPUT_STREAM_ENTRY_MESSAGE_TYPE,
            MessageType::OutputStreamEntry => OUTPUT_STREAM_ENTRY_MESSAGE_TYPE,
            MessageType::GetStateEntry => GET_STATE_ENTRY_MESSAGE_TYPE,
            MessageType::SetStateEntry => SET_STATE_ENTRY_MESSAGE_TYPE,
            MessageType::ClearStateEntry => CLEAR_STATE_ENTRY_MESSAGE_TYPE,
            MessageType::SleepEntry => SLEEP_ENTRY_MESSAGE_TYPE,
            MessageType::InvokeEntry => INVOKE_ENTRY_MESSAGE_TYPE,
            MessageType::BackgroundInvokeEntry => BACKGROUND_INVOKE_ENTRY_MESSAGE_TYPE,
            MessageType::AwakeableEntry => AWAKEABLE_ENTRY_MESSAGE_TYPE,
            MessageType::CompleteAwakeableEntry => COMPLETE_AWAKEABLE_ENTRY_MESSAGE_TYPE,
            MessageType::Custom(id) => id,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown message code {0:#x}")]
pub struct UnknownMessageType(u16);

impl TryFrom<MessageTypeId> for MessageType {
    type Error = UnknownMessageType;

    fn try_from(value: MessageTypeId) -> Result<Self, UnknownMessageType> {
        match value {
            START_MESSAGE_TYPE => Ok(MessageType::Start),
            COMPLETION_MESSAGE_TYPE => Ok(MessageType::Completion),
            SUSPENSION_MESSAGE_TYPE => Ok(MessageType::Suspension),
            ERROR_MESSAGE_TYPE => Ok(MessageType::Error),
            POLL_INPUT_STREAM_ENTRY_MESSAGE_TYPE => Ok(MessageType::PollInputStreamEntry),
            OUTPUT_STREAM_ENTRY_MESSAGE_TYPE => Ok(MessageType::OutputStreamEntry),
            GET_STATE_ENTRY_MESSAGE_TYPE => Ok(MessageType::GetStateEntry),
            SET_STATE_ENTRY_MESSAGE_TYPE => Ok(MessageType::SetStateEntry),
            CLEAR_STATE_ENTRY_MESSAGE_TYPE => Ok(MessageType::ClearStateEntry),
            SLEEP_ENTRY_MESSAGE_TYPE => Ok(MessageType::SleepEntry),
            INVOKE_ENTRY_MESSAGE_TYPE => Ok(MessageType::InvokeEntry),
            BACKGROUND_INVOKE_ENTRY_MESSAGE_TYPE => Ok(MessageType::BackgroundInvokeEntry),
            AWAKEABLE_ENTRY_MESSAGE_TYPE => Ok(MessageType::AwakeableEntry),
            COMPLETE_AWAKEABLE_ENTRY_MESSAGE_TYPE => Ok(MessageType::CompleteAwakeableEntry),
            v if ((v & CUSTOM_MESSAGE_MASK) != 0) => Ok(MessageType::Custom(v)),
            v => Err(UnknownMessageType(v)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageHeader {
    ty: MessageType,
    length: u32,

    // --- Flags
    /// Only `StartMessage` has protocol_version.
    protocol_version: Option<u16>,

    /// Only `CompletableEntries` have completed flag. See [`MessageType#allows_completed_flag`].
    completed_flag: Option<bool>,
    /// Only `Custom` entries have requires ack flag.
    requires_ack_flag: Option<bool>,
}

impl MessageHeader {
    #[inline]
    pub fn new(ty: MessageType, length: u32) -> Self {
        Self::_new(ty, None, None, None, length)
    }

    #[inline]
    pub fn new_start(protocol_version: u16, length: u32) -> Self {
        Self::_new(
            MessageType::Start,
            Some(protocol_version),
            None,
            None,
            length,
        )
    }

    #[inline]
    pub(super) fn new_entry_header(
        ty: MessageType,
        completed_flag: Option<bool>,
        requires_ack_flag: Option<bool>,
        length: u32,
    ) -> Self {
        debug_assert!(completed_flag.is_some() == ty.has_completed_flag());
        debug_assert!(requires_ack_flag.is_some() == ty.has_requires_ack_flag());

        MessageHeader {
            ty,
            length,
            protocol_version: None,
            completed_flag,
            requires_ack_flag,
        }
    }

    #[inline]
    fn _new(
        ty: MessageType,
        protocol_version: Option<u16>,
        completed_flag: Option<bool>,
        requires_ack_flag: Option<bool>,
        length: u32,
    ) -> Self {
        MessageHeader {
            ty,
            length,
            protocol_version,
            completed_flag,
            requires_ack_flag,
        }
    }

    #[inline]
    pub fn message_kind(&self) -> MessageKind {
        self.ty.kind()
    }

    #[inline]
    pub fn message_type(&self) -> MessageType {
        self.ty
    }

    #[inline]
    pub fn protocol_version(&self) -> Option<u16> {
        self.protocol_version
    }

    #[inline]
    pub fn completed(&self) -> Option<bool> {
        self.completed_flag
    }

    #[inline]
    pub fn requires_ack(&self) -> Option<bool> {
        self.requires_ack_flag
    }

    #[inline]
    pub fn frame_length(&self) -> u32 {
        self.length
    }
}

macro_rules! read_flag_if {
    ($cond:expr, $value:expr, $mask:expr) => {
        if $cond {
            Some(($value & $mask) != 0)
        } else {
            None
        }
    };
}

impl TryFrom<u64> for MessageHeader {
    type Error = UnknownMessageType;

    /// Deserialize the protocol header.
    /// See https://github.com/restatedev/service-protocol/blob/main/service-invocation-protocol.md#message-header
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let ty_code = (value >> 48) as u16;
        let ty: MessageType = ty_code.try_into()?;
        let protocol_version = if ty.has_protocol_version() {
            Some(((value & VERSION_MASK) >> 32) as u16)
        } else {
            None
        };
        let completed_flag = read_flag_if!(ty.has_completed_flag(), value, COMPLETED_MASK);
        let requires_ack_flag = read_flag_if!(ty.has_requires_ack_flag(), value, REQUIRES_ACK_MASK);
        let length = value as u32;

        Ok(MessageHeader::_new(
            ty,
            protocol_version,
            completed_flag,
            requires_ack_flag,
            length,
        ))
    }
}

macro_rules! write_flag {
    ($flag:expr, $value:expr, $mask:expr) => {
        if let Some(true) = $flag {
            *$value |= $mask;
        }
    };
}

impl From<MessageHeader> for u64 {
    /// Serialize the protocol header.
    /// See https://github.com/restatedev/service-protocol/blob/main/service-invocation-protocol.md#message-header
    fn from(message_header: MessageHeader) -> Self {
        let mut res =
            ((u16::from(message_header.ty) as u64) << 48) | (message_header.length as u64);

        if let Some(protocol_version) = message_header.protocol_version {
            res |= (protocol_version as u64) << 32;
        }
        write_flag!(message_header.completed_flag, &mut res, COMPLETED_MASK);
        write_flag!(
            message_header.requires_ack_flag,
            &mut res,
            REQUIRES_ACK_MASK
        );

        res
    }
}

#[cfg(test)]
mod tests {

    use super::{MessageKind::*, MessageType::*, *};

    impl MessageHeader {
        fn new_completable_entry(ty: MessageType, completed: bool, length: u32) -> Self {
            Self::new_entry_header(ty, Some(completed), None, length)
        }
    }

    macro_rules! roundtrip_test {
        ($test_name:ident, $header:expr, $ty:expr, $kind:expr, $len:expr) => {
            roundtrip_test!($test_name, $header, $ty, $kind, $len, None, None, None);
        };
        ($test_name:ident, $header:expr, $ty:expr, $kind:expr, $len:expr, version: $protocol_version:expr) => {
            roundtrip_test!(
                $test_name,
                $header,
                $ty,
                $kind,
                $len,
                None,
                Some($protocol_version),
                None
            );
        };
        ($test_name:ident, $header:expr, $ty:expr, $kind:expr, $len:expr, completed: $completed:expr) => {
            roundtrip_test!(
                $test_name,
                $header,
                $ty,
                $kind,
                $len,
                Some($completed),
                None,
                None
            );
        };
        ($test_name:ident, $header:expr, $ty:expr, $kind:expr, $len:expr, requires_ack: $requires_ack:expr) => {
            roundtrip_test!(
                $test_name,
                $header,
                $ty,
                $kind,
                $len,
                None,
                None,
                Some($requires_ack)
            );
        };
        ($test_name:ident, $header:expr, $ty:expr, $kind:expr, $len:expr, $completed:expr, $protocol_version:expr, $requires_ack:expr) => {
            #[test]
            fn $test_name() {
                let serialized: u64 = $header.into();
                let header: MessageHeader = serialized.try_into().unwrap();

                assert_eq!(header.message_type(), $ty);
                assert_eq!(header.message_kind(), $kind);
                assert_eq!(header.completed(), $completed);
                assert_eq!(header.protocol_version(), $protocol_version);
                assert_eq!(header.requires_ack(), $requires_ack);
                assert_eq!(header.frame_length(), $len);
            }
        };
    }

    roundtrip_test!(
        start,
        MessageHeader::new_start(1, 25),
        Start,
        Core,
        25,
        version: 1
    );

    roundtrip_test!(
        completion,
        MessageHeader::new(Completion, 22),
        Completion,
        Core,
        22
    );

    roundtrip_test!(
        completed_get_state,
        MessageHeader::new_completable_entry(GetStateEntry, true, 0),
        GetStateEntry,
        State,
        0,
        completed: true
    );

    roundtrip_test!(
        not_completed_get_state,
        MessageHeader::new_completable_entry(GetStateEntry, false, 0),
        GetStateEntry,
        State,
        0,
        completed: false
    );

    roundtrip_test!(
        completed_get_state_with_len,
        MessageHeader::new_completable_entry(GetStateEntry, true, 10341),
        GetStateEntry,
        State,
        10341,
        completed: true
    );

    roundtrip_test!(
        custom_entry,
        MessageHeader::new(MessageType::Custom(0xFC00), 10341),
        MessageType::Custom(0xFC00),
        MessageKind::Custom,
        10341,
        requires_ack: false
    );

    roundtrip_test!(
        custom_entry_with_requires_ack,
        MessageHeader::_new(MessageType::Custom(0xFC00), None, None, Some(true), 10341),
        MessageType::Custom(0xFC00),
        MessageKind::Custom,
        10341,
        requires_ack: true
    );
}
