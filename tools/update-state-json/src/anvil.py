from __future__ import annotations

from inputs import DeploymentInputs
from utils import derive_address_from_private_key, run_command

def fund_wallets(inputs: DeploymentInputs) -> None:
    wallets = {
        "commit": inputs.validator_sender_operator_commit_eth,
        "prove": inputs.validator_sender_operator_prove,
        "execute": inputs.validator_sender_operator_execute,
        "governor": derive_address_from_private_key(inputs.governor_key),
        "deployer": derive_address_from_private_key(inputs.deployer_key),
    }

    for role, address in wallets.items():
        # We reuse default rich account from anvil
        run_command(
            f"Fund {role} wallet",
            f"cast send {address} --value 1000000000000000000000 --private-key 0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6",
            inputs.l1_contracts_dir,
            inputs.base_env(),
        )

def start_anvil(state_path: str):
    from subprocess import Popen, DEVNULL
    anvil_process = Popen(
        ["anvil", "--dump-state", state_path],
        stdout=DEVNULL,
        stderr=DEVNULL,
    )
    return anvil_process

def stop_anvil(anvil_process):
    anvil_process.terminate()
    anvil_process.wait()
