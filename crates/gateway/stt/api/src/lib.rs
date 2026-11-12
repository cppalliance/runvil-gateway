//! Gateway-owned speech facade and HTTP endpoints.
//!
//! [`SpeechService`] owns artifact preparation, one-time initial runtime
//! publication, batch transcription, and Realtime transcription.

mod admission;
mod artifacts;
mod audio;
mod batch;
mod generation;
mod guidance;
mod model;
mod realtime;
mod segment;
mod service;
mod status;
mod take;
#[cfg(all(test, not(feature = "test-fixtures")))]
mod test_fixtures;
#[cfg(feature = "test-fixtures")]
pub mod test_fixtures;

pub use artifacts::SpeechError;
pub use batch::TRANSCRIPTIONS_PATH;
pub use model::SpeechModelInfo;
pub use realtime::REALTIME_PATH;
pub use service::SpeechService;
pub use status::SpeechStatus;

/// The OpenAPI description of the two speech routes, which the Gateway
/// merges into its own document.
#[derive(Debug, utoipa::OpenApi)]
#[openapi(paths(batch::handler, realtime::route::upgrade))]
pub struct SpeechApi;

#[cfg(all(test, miri))]
mod miri_tests {
    use super::SpeechService;

    #[test]
    fn miri_facade_target_executes_without_native_route_fixtures() {
        let service = SpeechService::new();
        assert!(!service.status().ready());
    }
}
