# RPC

* All standard `eth_` methods are supported (except those specific to EIP-2930, EIP-4844 and EIP-7702). Block tags have
  a special meaning:
    * `earliest` - not supported yet (will return genesis or first uncompressed block)
    * `pending` - the latest produced block
    * `latest` - same as `pending` (consider taking consensus into account here)
    * `safe` - the latest block that has been committed to L1
    * `finalized` - not supported yet (will return the latest block that has been executed on L1)
* `zks_` namespace is kept to the minimum right now to avoid legacy from Era. Only following methods are supported:
    * `zks_getBridgehubContract`
* `ots_` namespace is used for Otterscan integration (meant for local development only)

<!-- SYSCOIN: Downstream RPC retention, capture capacities, and trusted-JS opt-in. -->
## Local RPC capture and retention capacities

Installed log, block, and pending-transaction filters share
`rpc.max_active_filters` (default `4096`). Installed log filters also have an
aggregate address/topic-alternative cap, `rpc.max_filter_terms` (default `1024`).
Both values must be nonzero. Uninstalling or expiring a filter releases its slot;
stateless `eth_getLogs` retains its existing query limits.

Native call tracing, including explorer tracing, remains available with the debug
namespace. One request, including replayed transaction prefixes, has cumulative
`rpc.call_tracer_max_capture_bytes` (default `64 MiB`) and
`rpc.call_tracer_max_frames` (default `100000`) capacities. Exhaustion returns an
explicit error rather than a partial trace. These account for logical capture
data, not total process RSS or VM execution memory; zero rejects capture.

Arbitrary JavaScript tracers additionally require
`rpc.enable_custom_js_tracers: true` (default `false`). Enable this only for trusted
tracer authors on an access-controlled endpoint. Initialization is included in
the existing time/allocation accounting, and Boa's loop limit remains in place,
but between-hook checks cannot interrupt every interpreter operation. Allocation
measurement also depends on jemalloc support. This is not a hard sandbox for
untrusted JavaScript; native tracers do not require the opt-in.

<!-- SYSCOIN: Downstream policy provenance and local capture/admission boundaries. -->
## Optional policy service

When `tx_validator.policy_service.url` is configured, supported unsigned `eth_call`
and `eth_estimateGas` executions consult policy. Their caller-supplied `from`
cannot claim the `bypass_from` exemptions reserved for authenticated transaction
and protocol execution. L1-shaped public simulations are rejected while policy
is configured because that VM path does not run validator hooks; actual L1
protocol execution and simulations without policy retain their existing behavior.
Debug and multi-transaction simulations using `NopValidator` likewise fail closed.
The initial highest-gas estimate probe is policy-validated before execution
outcomes can be returned; the final resolved estimate is validated again because
gas-sensitive execution can differ. Intermediate search probes omit policy calls.

RPC-side policy simulation during signed transaction submission shares
`rpc.max_concurrent_blocking_rpcs` with the other heavy RPC work. At capacity it
returns a retriable server-busy error, without queuing another policy worker.
Disconnecting a caller does not free that slot until its blocking worker exits;
the subsequent synchronous receipt wait does not occupy a simulation slot.

Policy trace capture has local operator capacities:
`tx_validator.policy_service.max_trace_bytes` (default `8 MiB`) and
`max_trace_frames` (default `16384`). Capture charges cumulative calldata and
frame storage before copying; the frame cap also bounds tree / deployment-vector
growth. These are capture capacities, not new consensus transaction limits or
exact process-RSS guarantees. Exhaustion rejects validation without sending a
partial tree to `/judge`; zero fails closed rather than disabling the limit.
The validator reports a denial and logs the local capture-capacity reason without
including captured calldata; it does not disguise the outcome as a partial allow.
Authenticated exempt transactions skip both policy calls and unnecessary capture.
Operators using policy should size these capacities for their legitimate workload.
