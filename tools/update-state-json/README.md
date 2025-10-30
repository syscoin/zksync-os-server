# A TEMPORARY utility to initialize `state.json` and `genesis.json`

This tool can deploy all the required L1 contracts, dump anvil's `state.json`, and
generate `genesis.json` for the server.

This is a TEMPORARY solution until something more robust is created.
This tool was created _only_ because we didn't have a better solution at time than to reuse
the `zkstack` CLI from Era codebase, which was neither convenient or reliable.

THIS TOOL IS NOT MEANT TO BE USED FOR ANY KIND OF ACTUAL DEPLOYMENTS OUTSIDE OF LOCALHOST ENVIRONMENT.
This is by design.
Do not try to change it.

## Known issues

The tool might crash on the first time sometimes with zksolc compilation failure. I don't know why, but it
works on the second try. Just re-run it.

## Maintaining guidelines

The tool intentionally doesn't have any dependencies.
NO EXTERNAL DEPENDENCIES ALLOWED.
We should be able to run the script having `python3` only.

## Running

`python3 src/main.py`

