#!/usr/bin/env python3
"""Convert the explicit approved public namespace plan to offline subtree input."""
import argparse
import hashlib
import json
from pathlib import Path
import re

import check_identity as identity


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--era-root", type=Path, required=True)
    parser.add_argument("--artifacts-root", type=Path, required=True)
    parser.add_argument("--era-base-commit", required=True)
    parser.add_argument("--era-source-patched-tree", required=True)
    parser.add_argument("--zksync-os-patched-tree", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    raw, plan = identity.load_json(args.plan)
    identity.require_equal(plan["schema"], "syscoin-v32-planned-namespace-v1", "plan schema")
    identity.require_equal(plan["status"], "offline-plan-not-deployed", "plan status")
    if plan["deployed"] is not False or plan["independent_host_reproduction"] is not False:
        raise ValueError("plan must explicitly remain offline/single-host")
    identity.require_equal(identity.address(plan["factory"]), identity.FACTORY, "factory")
    expected_salt = "0x" + hashlib.sha256(plan["namespace_label"].encode("utf-8")).hexdigest()
    identity.require_equal(plan["create2_salt"], expected_salt, "documented namespace salt")
    governance = identity.artifact(args.era_root, args.artifacts_root / "Governance.sol/Governance.json", "cancun")
    ctor = plan["root_governance_constructor"]
    identity.address(ctor["admin"])
    constructor = identity.address_word(ctor["admin"]) + identity.address_word(ctor["security_council"]) + identity.word(ctor["min_delay_seconds"])
    governance_address = identity.create2(plan["factory"], expected_salt, governance["creation"] + constructor)
    source = (args.era_root / "l1-contracts/contracts/common/SyscoinEdgeDARelayDeployment.sol").read_text()
    match = re.search(r"bytes32 constant SYSCOIN_EDGE_DA_RELAY_SALT = (0x[0-9a-f]{64});", source)
    if match is None:
        raise ValueError("canonical relay salt missing")
    result = {
        "schema": "syscoin-v32-offline-identity-input-v1", "scope": "critical-subtree",
        "deployed": False, "namespace": plan["namespace_label"],
        "factory": plan["factory"], "outer_salt": expected_salt, "relay_salt": match[1],
        "root_governance_constructor": ctor, "l1_governance": governance_address,
        "plan_sha256": hashlib.sha256(raw).hexdigest(),
        "config": {"salt": expected_salt, "aliasedGovernanceAddress": identity.alias(governance_address),
                   "isZKsyncOS": True, "testnetVerifier": False},
        "source_bindings": {"era_base_commit": args.era_base_commit,
                            "era_source_patched_tree": args.era_source_patched_tree,
                            "zksync_os_patched_tree": args.zksync_os_patched_tree},
    }
    identity.validate_input(result)
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(result, output, indent=2, sort_keys=True)
        output.write("\n")
    print(f"Offline planned root Governance: {governance_address}")
    print(f"Aliased Governance: {result['config']['aliasedGovernanceAddress']}")


if __name__ == "__main__":
    main()
