#!/usr/bin/env python3
"""Compatibility entry point; implementation lives in zksync-airbender-prover."""

from _prover_rental import run

run('worker.py', globals())
