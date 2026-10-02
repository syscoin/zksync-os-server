//! Generation helper only: no fixture registration, deployment or real proof.
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == "export-replay" || arg == "recover-replay")
    {
        anyhow::ensure!(
            args.len() == 6,
            "usage: export-replay ARCHIVE STOPPED_DB CHAIN_ID PACKAGE | recover-replay PACKAGE MANIFEST SHA FRESH_DB"
        );
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                if args[1] == "export-replay" {
                    let chain_id = args[4]
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("chain ID type"))?
                        .parse()?;
                    let manifest = zksync_os_integration_tests::component_replay::export_replay(
                        std::path::Path::new(&args[2]),
                        std::path::Path::new(&args[3]),
                        chain_id,
                        std::path::Path::new(&args[5]),
                    )
                    .await?;
                    println!("{}", serde_json::to_string(&manifest)?);
                    Ok(())
                } else {
                    zksync_os_integration_tests::component_replay::recover_manifest(
                        std::path::Path::new(&args[2]),
                        std::path::Path::new(&args[3]),
                        args[4]
                            .to_str()
                            .ok_or_else(|| anyhow::anyhow!("manifest digest type"))?,
                        std::path::Path::new(&args[5]),
                    )
                    .await
                }
            });
    }
    anyhow::ensure!(
        args.len() == 3,
        "usage: anvil-component-bootstrap CONFIG DEADLINE_UNIX"
    );
    let deadline = args[2]
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("deadline type"))?
        .parse()?;
    tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(256 * 1024 * 1024)
        .enable_all()
        .build()?
        .block_on(
            zksync_os_integration_tests::test_config::run_component_fixture_bootstrap(
                args[1].clone().into(),
                deadline,
            ),
        )
}
