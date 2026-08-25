use crate::PipelineComponent;
use crate::component_id::ComponentId;
use crate::peekable_receiver::PeekableReceiver;
use futures::FutureExt as _;
use reth_tasks::Runtime;
use std::collections::HashSet;
use std::future::Future;
use tokio::sync::{mpsc, watch};
use zksync_os_observability::{ComponentState, ComponentStateReporter};

enum SegmentExit<T, Guard> {
    Shutdown(Guard),
    Component(T),
}

async fn select_segment_exit<Component, Shutdown, Guard>(
    component: Component,
    shutdown: Shutdown,
) -> SegmentExit<anyhow::Result<()>, Guard>
where
    Component: Future<Output = anyhow::Result<()>>,
    Shutdown: Future<Output = Guard>,
{
    tokio::pin!(component);
    tokio::pin!(shutdown);

    tokio::select! {
        biased;
        guard = &mut shutdown => SegmentExit::Shutdown(guard),
        result = &mut component => match result {
            Err(err) => match (&mut shutdown).now_or_never() {
                Some(guard) => SegmentExit::Shutdown(guard),
                None => SegmentExit::Component(Err(err)),
            },
            Ok(()) => SegmentExit::Component(Ok(())),
        },
    }
}

/// Pipeline with an active output stream that can be piped to more components
pub struct Pipeline<Output: Send + 'static> {
    receiver: PeekableReceiver<Output>,
    runtime: Runtime,
    spawned_tasks: HashSet<&'static str>,
    shutdown_sender: mpsc::Sender<&'static str>,
    shutdown_receiver: mpsc::Receiver<&'static str>,
    components: Vec<(ComponentId, watch::Receiver<ComponentState>)>,
}

impl<Output: Send + 'static> Pipeline<Output> {
    pub fn components(&self) -> Vec<(ComponentId, watch::Receiver<ComponentState>)> {
        self.components
            .iter()
            .map(|(id, rx)| (*id, rx.clone()))
            .collect()
    }
}

impl Pipeline<()> {
    pub fn new(runtime: Runtime) -> Self {
        let (_sender, receiver) = mpsc::channel::<()>(1);
        let receiver = PeekableReceiver::new(receiver);
        let (shutdown_sender, shutdown_receiver) = mpsc::channel(16);
        Self {
            receiver,
            runtime,
            spawned_tasks: HashSet::default(),
            shutdown_sender,
            shutdown_receiver,
            components: Vec::new(),
        }
    }

    /// Spawns a final supervisor that waits for all pipeline segments to shut down.
    /// Returns the accumulated component state receivers for backpressure monitoring.
    pub fn spawn(mut self) {
        // No consumer exists after the terminal stage.
        drop(self.receiver);

        self.runtime.spawn_critical_with_graceful_shutdown_signal(
            "pipeline",
            |shutdown| async move {
                // Hold shutdown open until every spawned segment deregisters.
                let _guard = shutdown.await;

                while !self.spawned_tasks.is_empty() {
                    // Each segment sends its name when it exits or handles shutdown.
                    let Some(name) = self.shutdown_receiver.recv().await else {
                        // A segment that panicked (or was torn down hard) drops its
                        // sender without deregistering. The supervisor's job is
                        // bookkeeping — it must not amplify that into a second
                        // panic.
                        tracing::warn!(
                            remaining = ?self.spawned_tasks,
                            "pipeline segment(s) never deregistered"
                        );
                        break;
                    };

                    if !self.spawned_tasks.remove(name) {
                        // Defensive logging for duplicate or unexpected notifications.
                        tracing::warn!(%name, "tried to deregister non-existent segment");
                    } else {
                        tracing::debug!(%name, "pipeline segment deregistered");
                    }

                    if !self.spawned_tasks.is_empty() {
                        tracing::debug!("pipeline segments left: {:?}", self.spawned_tasks);
                    }
                }

                if self.spawned_tasks.is_empty() {
                    tracing::debug!("pipeline finished gracefully");
                }
            },
        );
    }
}

impl<Output: Send + 'static> Pipeline<Output> {
    /// Add a transformer component to the pipeline
    pub fn pipe<C>(mut self, component: C) -> Pipeline<C::Output>
    where
        C: PipelineComponent<Input = Output>,
    {
        let id = C::COMPONENT_ID;
        let name = id.as_str();

        let (reporter, rx) = ComponentStateReporter::new(name);
        self.components.push((id, rx));

        let (output_sender, output_receiver) =
            mpsc::channel::<C::Output>(C::OUTPUT_CHANNEL_CAPACITY);
        let output_receiver = PeekableReceiver::new(output_receiver);
        let input_receiver = self.receiver;

        let shutdown_sender = self.shutdown_sender.clone();
        self.runtime
            .spawn_critical_with_graceful_shutdown_signal(name, |shutdown| async move {
                let _shutdown_guard = match select_segment_exit(
                    component.run(input_receiver, output_sender, reporter),
                    shutdown,
                )
                .await
                {
                    SegmentExit::Component(res) => {
                        res.expect("pipeline segment failed");
                        tracing::debug!(name, "segment finished running");
                        None
                    }
                    SegmentExit::Shutdown(guard) => {
                        tracing::debug!(name, "segment shutting down");
                        Some(guard)
                    }
                };
                // Always deregister after either normal completion or shutdown. The supervisor
                // may already be gone during teardown, so a failed bookkeeping send is harmless.
                shutdown_sender.send(name).await.ok();
            });
        self.spawned_tasks.insert(name);

        Pipeline {
            receiver: output_receiver,
            runtime: self.runtime,
            spawned_tasks: self.spawned_tasks,
            shutdown_sender: self.shutdown_sender,
            shutdown_receiver: self.shutdown_receiver,
            components: self.components,
        }
    }

    /// Conditionally add a component if present. The component must keep the same item type.
    pub fn pipe_opt<C>(self, component: Option<C>) -> Pipeline<Output>
    where
        C: PipelineComponent<Input = Output, Output = Output>,
    {
        match component {
            Some(c) => self.pipe(c),
            None => self,
        }
    }

    /// Conditional add one component or the other. Both components need to have same item types.
    pub fn pipe_if<CTrue, CFalse>(
        self,
        condition: bool,
        c_true: CTrue,
        c_false: CFalse,
    ) -> Pipeline<CTrue::Output>
    where
        CTrue: PipelineComponent<Input = Output>,
        CFalse: PipelineComponent<Input = Output, Output = CTrue::Output>,
    {
        match condition {
            true => self.pipe(c_true),
            false => self.pipe(c_false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use reth_tasks::{Runtime, RuntimeBuilder, RuntimeConfig, TokioConfig};
    use std::future::ready;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use zksync_os_observability::ComponentStateReporter;

    /// A segment that reports in, waits for `go`, then fails after a synchronous pause. The pause
    /// provides the window in which the test can fire the shutdown signal before the error lands.
    struct FailingSegment {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        go: tokio::sync::oneshot::Receiver<()>,
    }

    #[async_trait]
    impl PipelineComponent for FailingSegment {
        type Input = ();
        type Output = ();
        const COMPONENT_ID: ComponentId = ComponentId::NoopSink;
        const OUTPUT_CHANNEL_CAPACITY: usize = 1;

        async fn run(
            mut self,
            _input: PeekableReceiver<()>,
            _output: mpsc::Sender<()>,
            _reporter: ComponentStateReporter,
        ) -> anyhow::Result<()> {
            self.started
                .take()
                .expect("run is called once")
                .send(())
                .ok();
            self.go.await.ok();
            std::thread::sleep(Duration::from_millis(300));
            anyhow::bail!("downstream closed")
        }
    }

    static PANIC_HOOK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>;

    struct PanicCounter {
        _guard: tokio::sync::MutexGuard<'static, ()>,
        counter: Arc<AtomicUsize>,
        previous_hook: Option<PanicHook>,
    }

    impl PanicCounter {
        fn count(&self) -> usize {
            self.counter.load(Ordering::SeqCst)
        }
    }

    impl Drop for PanicCounter {
        fn drop(&mut self) {
            std::panic::set_hook(
                self.previous_hook
                    .take()
                    .expect("panic hook is restored exactly once"),
            );
        }
    }

    /// Counts every panic while serializing access to the process-global panic hook.
    async fn install_panic_counter() -> PanicCounter {
        let guard = PANIC_HOOK_LOCK.lock().await;
        let counter = Arc::new(AtomicUsize::new(0));
        let hook_counter = counter.clone();
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |_| {
            hook_counter.fetch_add(1, Ordering::SeqCst);
        }));
        PanicCounter {
            _guard: guard,
            counter,
            previous_hook: Some(previous_hook),
        }
    }

    fn test_runtime() -> Runtime {
        RuntimeBuilder::new(
            RuntimeConfig::default().with_tokio(TokioConfig::existing_handle(
                tokio::runtime::Handle::current(),
            )),
        )
        .build()
        .expect("failed to build runtime")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_segment_error_during_shutdown_winds_down_without_panicking() {
        let panics = install_panic_counter().await;
        let runtime = test_runtime();
        let (started_sender, started) = tokio::sync::oneshot::channel();
        let (go, go_receiver) = tokio::sync::oneshot::channel();
        Pipeline::new(runtime.clone())
            .pipe(FailingSegment {
                started: Some(started_sender),
                go: go_receiver,
            })
            .spawn();

        started.await.expect("segment started");
        go.send(()).expect("segment waits for go");
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(
            runtime
                .initiate_graceful_shutdown()
                .expect("task manager is alive"),
        );

        assert!(
            runtime.graceful_shutdown_with_timeout(Duration::from_secs(10)),
            "every segment must deregister and wind down"
        );
        assert_eq!(
            panics.count(),
            0,
            "an error during shutdown is wind-down, not a failure"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_segment_error_while_live_still_panics_exactly_once() {
        let panics = install_panic_counter().await;
        let runtime = test_runtime();
        let (started_sender, started) = tokio::sync::oneshot::channel();
        let (go, go_receiver) = tokio::sync::oneshot::channel();
        Pipeline::new(runtime.clone())
            .pipe(FailingSegment {
                started: Some(started_sender),
                go: go_receiver,
            })
            .spawn();

        started.await.expect("segment started");
        go.send(()).expect("segment waits for go");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while panics.count() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "a live segment failure must panic"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        runtime.graceful_shutdown_with_timeout(Duration::from_secs(10));
        assert_eq!(
            panics.count(),
            1,
            "exactly the segment's own panic — the supervisor must not amplify it"
        );
    }

    #[tokio::test]
    async fn shutdown_wins_when_both_futures_are_ready() {
        let component = ready::<anyhow::Result<()>>(Err(anyhow::anyhow!("component failed")));

        let exit = select_segment_exit(component, ready(())).await;

        assert!(matches!(exit, SegmentExit::Shutdown(())));
    }
}
