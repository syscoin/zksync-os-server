#!/usr/bin/env bash
# Run repeatable local checks for the Pali SLH-DSA verifier path.
#
# This is a development harness, not a protocol pricing proof. It exercises the
# pinned valid vector, invalid zero signature, precompile fast path, and fallback
# path through Foundry so reviewers can collect wall-clock and gas-report data
# without carrying benchmark logic in the zksync-os patch itself.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TEST_ROOT="${REPO_ROOT}/integration-tests/test-contracts"
ITERATIONS="${SLH_DSA_BENCH_ITERATIONS:-5}"

tests=(
  testRealVerifierAcceptsPinnedKnownAnswerVector
  testRealVerifierRejectsZeroSignature
  testRealVerifierUsesSuccessfulPrecompileFastPath
  testRealVerifierUsesFailingPrecompileFastPath
  testRealVerifierFallsBackWhenPrecompileReturnsEmpty
)

cd "${TEST_ROOT}"

for test_name in "${tests[@]}"; do
  echo "== ${test_name} (${ITERATIONS} iterations) =="
  for ((i = 1; i <= ITERATIONS; ++i)); do
    echo "-- iteration ${i}"
    /usr/bin/time -p forge test \
      --gas-report \
      --match-contract PaliSLHDSAValidatorModuleTest \
      --match-test "${test_name}"
  done
done
