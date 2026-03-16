#[test_log::test(tokio::test)]
async fn restart_from_previous_patch_settles_three_batches() -> anyhow::Result<()> {
    zksync_os_integration_tests::version_restart::restart_from_previous_patch_settles_three_batches(
    )
    .await
}

#[test_log::test(tokio::test)]
async fn restart_from_previous_minor_is_not_operational() -> anyhow::Result<()> {
    zksync_os_integration_tests::version_restart::restart_from_previous_minor_is_not_operational()
        .await
}
