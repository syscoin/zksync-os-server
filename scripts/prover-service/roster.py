#!/usr/bin/env python3
"""Authenticate complete, indexed wrapper rosters against the registry's padded tree."""

import copy
import functools
import argparse
import os
import sys

import service as s


MAX_CANDIDATES = 4096
TREE_DEPTH = 32


@functools.lru_cache(maxsize=16384)
def pair(left, right):
    return s.raw_hex(s.keccak(min(left, right) + max(left, right)), 32)


@functools.lru_cache(maxsize=MAX_CANDIDATES)
def leaf(encoded):
    return s.raw_hex(s.keccak(s.raw_hex(s.keccak(encoded))), 32)


def construct(period, candidates):
    """Build the complete artifact; candidates must already be in registry account order."""
    s.uint(period, 64)
    s.require(type(candidates) is list and 0 < len(candidates) <= MAX_CANDIDATES, "invalid_roster_count")
    nodes, previous = [], bytes(20)
    for index, candidate in enumerate(candidates):
        encoded = s.encode_fields(s.CANDIDATE, candidate)
        s.require(candidate["index"] == index, "noncontiguous_roster_indices")
        for field in ("account", "operator", "beneficiary"):
            s.nonzero(candidate[field], 20)
        account = s.raw_hex(candidate["account"], 20)
        s.require(account > previous, "roster_accounts_not_sorted_unique")
        previous = account
        nodes.append(leaf(encoded))
    proofs = [[] for _ in candidates]
    zero = bytes(32)
    for level in range(TREE_DEPTH):
        for index, proof in enumerate(proofs):
            sibling = (index >> level) ^ 1
            proof.append("0x" + (nodes[sibling] if sibling < len(nodes) else zero).hex())
        nodes = [pair(nodes[index], nodes[index + 1] if index + 1 < len(nodes) else zero)
                 for index in range(0, len(nodes), 2)]
        zero = pair(zero, zero)
    return {"schema_version": 1, "period": period, "root": "0x" + nodes[0].hex(), "count": len(candidates),
            "candidates": copy.deepcopy(candidates), "proofs": proofs}


def authenticate(artifact, expected):
    """Require the entire published set, including the canonical proof for every index."""
    s.exact(artifact, ("schema_version", "period", "root", "count", "candidates", "proofs"))
    s.exact(expected, ("period", "root", "count"))
    s.require(type(artifact["schema_version"]) is int and artifact["schema_version"] == 1, "unsupported_roster_schema")
    for value in (artifact, expected):
        s.uint(value["period"], 64)
        s.require(0 < s.uint(value["count"], 32) <= MAX_CANDIDATES, "invalid_roster_count")
        s.nonzero(value["root"])
    s.require(all(artifact[field] == expected[field] for field in expected), "roster_context_mismatch")
    s.require(type(artifact["candidates"]) is list and len(artifact["candidates"]) == artifact["count"],
              "incomplete_roster")
    s.require(type(artifact["proofs"]) is list and len(artifact["proofs"]) == artifact["count"],
              "incomplete_roster_proofs")
    for proof in artifact["proofs"]:
        s.require(type(proof) is list and len(proof) == TREE_DEPTH, "invalid_roster_proof")
        for sibling in proof:
            s.raw_hex(sibling, 32)
    reconstructed = construct(artifact["period"], artifact["candidates"])
    s.require(reconstructed["root"] == artifact["root"], "roster_root_mismatch")
    s.require(reconstructed["proofs"] == artifact["proofs"], "roster_proof_mismatch")
    return reconstructed


def selected(artifact, expected, index):
    complete = authenticate(artifact, expected)
    s.require(s.uint(index, 32) < complete["count"], "selected_wrapper_out_of_range")
    return {"candidate": complete["candidates"][index], "candidate_proof": complete["proofs"][index]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    command = parser.add_subparsers(dest="command", required=True).add_parser("build")
    command.add_argument("--period", type=int, required=True)
    command.add_argument("--candidates", required=True)
    command.add_argument("--output", required=True)
    args = parser.parse_args()
    s.write_new(args.output, construct(args.period, s.read_json(args.candidates)))


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (s.Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, s.Error) else "roster_construction_failed", file=sys.stderr)
        sys.exit(1)
