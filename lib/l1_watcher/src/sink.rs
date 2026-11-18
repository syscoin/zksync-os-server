/// A destination a watcher pushes its decoded items into. Implemented by the consumer
/// (e.g. mempool subpools) so that `l1_watcher` need not depend on the consumer crate.
// async-trait adds must_use to a future that already carries the annotation.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait EventSink<T>: Send + Sync + 'static {
    async fn push(&mut self, item: T);
}
