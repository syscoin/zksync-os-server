alloy::sol!(
    /// Simple contract that can emit events on demand.
    #[sol(rpc)]
    EventEmitter,
    "test-contracts/out/EventEmitter.sol/EventEmitter.json"
);

alloy::sol!(
    /// Simple contract that deploys another contract on demand.
    #[sol(rpc)]
    DummyFactory,
    "test-contracts/out/DummyFactory.sol/DummyFactory.json"
);

alloy::sol!(
    /// Simple contract that calls modexp.
    #[sol(rpc)]
    ModExp,
    "test-contracts/out/Modexp.sol/ModExp.json"
);
