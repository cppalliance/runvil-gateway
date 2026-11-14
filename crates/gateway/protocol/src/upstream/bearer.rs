//! Where an upstream gets the credential it presents.

use std::fmt;

use async_trait::async_trait;
use gateway_config::Secret;

use crate::error::ProtocolError;

/// The bearer token an upstream presents, fetched for each request.
///
/// A configured API key never changes, so [`StaticBearer`] hands it back as
/// is. A credential with a lifetime, such as an OAuth access token,
/// implements this to refresh before it hands one out.
#[async_trait]
pub trait BearerSource: Send + Sync + fmt::Debug {
    /// The bearer for the next request. An empty secret means the request
    /// carries no `Authorization` header.
    ///
    /// # Errors
    /// Returns a [`ProtocolError`] when no usable credential exists, for
    /// example a signed-out account. Nothing is sent upstream in that case.
    async fn bearer(&self) -> Result<Secret, ProtocolError>;

    /// Reports that the backend answered `rejected` with a 401, so a source
    /// with a lifetime can stop handing it out. The default does nothing.
    async fn rejected(&self, _rejected: &Secret) {}
}

/// A fixed credential: an endpoint's configured API key.
#[derive(Debug, Clone)]
pub struct StaticBearer(Secret);

impl StaticBearer {
    /// Wraps a configured key. An empty key sends no `Authorization` header.
    #[must_use]
    pub fn new(key: Secret) -> StaticBearer {
        StaticBearer(key)
    }
}

#[async_trait]
impl BearerSource for StaticBearer {
    async fn bearer(&self) -> Result<Secret, ProtocolError> {
        Ok(self.0.clone())
    }
}
