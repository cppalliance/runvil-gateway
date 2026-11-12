//! The client error event each session failure becomes.

use super::{AudioError, ClientError, MailboxError, SessionError};

pub(super) fn session_error(error: &SessionError, client_event_id: Option<String>) -> ClientError {
    match error {
        SessionError::Audio(AudioError::InvalidBase64(_)) => ClientError::request(
            "invalid_base64_audio",
            "Audio must be valid Base64",
            Some("audio"),
            client_event_id,
        ),
        SessionError::Audio(AudioError::AppendTooLarge { .. }) => ClientError::request(
            "audio_append_too_large",
            "Decoded audio exceeds the 15 MiB append limit",
            Some("audio"),
            client_event_id,
        ),
        SessionError::Audio(AudioError::IncompletePcm16Sample) => ClientError::request(
            "invalid_pcm_audio",
            "PCM16 audio ends with an incomplete sample",
            Some("audio"),
            client_event_id,
        ),
        SessionError::Audio(AudioError::BufferTooLong {
            maximum_seconds,
            retained_ms,
            requested_ms,
        }) => ClientError::overload(
            "too_much_unfinalized_audio",
            format!(
                "Unfinalized audio exceeds {maximum_seconds} seconds: {retained_ms} ms retained, {requested_ms} ms requested"
            ),
            Some("audio"),
            client_event_id,
        ),
        SessionError::Audio(AudioError::CommitTooShort { .. }) => ClientError::request(
            "audio_too_short",
            "A commit requires at least 100 ms of audio",
            Some("audio"),
            client_event_id,
        ),
        SessionError::CommittedItemsAtCapacity => ClientError::overload(
            "too_many_committed_items",
            "At most four committed items may finalize concurrently",
            None,
            client_event_id,
        ),
        SessionError::PendingPrecommitFailure(_) => ClientError::request(
            "precommit_transcription_failed",
            "Further appends are rejected after accurate precommit failure",
            Some("audio"),
            client_event_id,
        ),
        SessionError::CancelJoinAtCapacity => ClientError::overload(
            "audio_queue_lag",
            "Audio queue lag exceeds two seconds",
            Some("audio"),
            client_event_id,
        ),
        SessionError::NoInput => ClientError::request(
            "input_audio_buffer_empty",
            "The input audio buffer is empty",
            Some("audio"),
            client_event_id,
        ),
        SessionError::EpochExhausted
        | SessionError::CanceledTaskFailed
        | SessionError::GenerationUnavailable
        | SessionError::Inference(_)
        | SessionError::Finalization(_)
        | SessionError::Mailbox(MailboxError::TerminalAlreadySet | MailboxError::UnknownItem) => {
            ClientError::server(
                "internal_error",
                "Transcription failed",
                None,
                client_event_id,
            )
        }
    }
}
