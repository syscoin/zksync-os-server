use crate::{ReplayArchiveKey, ReplayArchiveSession};
use async_trait::async_trait;

/// One page of listed replay archive keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayArchiveKeyPage {
    pub keys: Vec<ReplayArchiveKey>,
    /// Opaque token to pass to the next `list_keys_page` call; `None` when this is the last page.
    pub next_page_token: Option<String>,
}

/// Read-side access to replay archive objects.
///
/// Implementations should hide backend-specific path parsing and return normalized archive keys.
// async-trait adds must_use to a future that already carries the annotation.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ReplayArchiveStorageReader {
    /// Lists one page of stored replay archive object keys without fetching payloads.
    ///
    /// Pass `None` to start listing and the previous page's `next_page_token` to continue.
    /// Paging lets callers interleave listing with payload downloads instead of waiting for a
    /// full listing of a large archive.
    async fn list_keys_page(
        &self,
        page_token: Option<String>,
    ) -> anyhow::Result<ReplayArchiveKeyPage>;

    /// Lists one page under an exact writer prefix. A page token belongs to the same session.
    /// Readers that only support whole-archive recovery must not silently broaden the request.
    async fn list_keys_page_for_session(
        &self,
        session: Option<&ReplayArchiveSession>,
        page_token: Option<String>,
    ) -> anyhow::Result<ReplayArchiveKeyPage> {
        anyhow::ensure!(
            session.is_none(),
            "archive reader does not support session-scoped listing"
        );
        self.list_keys_page(page_token).await
    }

    /// Fetches the payload of a single archived object.
    async fn fetch_object(&self, key: &ReplayArchiveKey) -> anyhow::Result<Vec<u8>>;
}
