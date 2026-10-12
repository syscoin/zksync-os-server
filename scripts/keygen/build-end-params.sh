#!/usr/bin/env bash
# Build the unchanged Airbender helper without resolving its lockless CLI workspace.
set -euo pipefail
umask 077

fail() { printf 'end-params build: %s\n' "$*" >&2; exit 1; }
[[ $# -eq 2 ]] || fail 'usage: build-end-params.sh PRISTINE_AIRBENDER_CHECKOUT NEW_BUILD_DIRECTORY'
readonly airbender_commit=e8f380777b7e90388c7323d6fd80a3b58c589f17
readonly airbender_tree=799e018339df40d0e456ba3fa0fc768ce0691853
readonly toolchain=nightly-2026-02-10
readonly source_sha256=95a59eca0f59f7de4d2e256a6edde5eb4248a563e2ff506d9a8fa3dd136ba2c7
readonly manifest_sha256=dc2ca3c5ad7126733eb26dcddaf6cb4e130a5770e6c363f23d3962a8d6634b89
readonly lock_sha256=0b004f8fe8b536edfb841f99f1c5d78d058df598d2c7f093c457b7932f55e2e6
helper_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly helper_dir
readonly assets_dir="${helper_dir}/end-params"
server_root="$(cd -- "${helper_dir}/../.." && pwd -P)"
airbender_dir="$(cd -- "$1" && pwd -P)"
readonly server_root airbender_dir
[[ ! -e "$2" && ! -L "$2" ]] || fail 'build directory already exists; preserve prior evidence'
build_parent="$(cd -- "$(dirname -- "$2")" && pwd -P)"
build_name="$(basename -- "$2")"
readonly build_parent build_name
[[ "${build_name}" != . && "${build_name}" != .. ]] || fail 'invalid build directory'
readonly build_dir="${build_parent}/${build_name}"
case "${build_dir}/" in
  "${server_root}/"*|"${airbender_dir}/"*) fail 'build directory must be outside both source checkouts' ;;
esac
[[ "${RUST_TOOLCHAIN:-${toolchain}}" == "${toolchain}" ]] || fail 'toolchain override is not the pinned toolchain'
[[ -z "${RUSTFLAGS:-}" && -z "${CARGO_ENCODED_RUSTFLAGS:-}" ]] || fail 'custom Rust flags are not part of the reviewed helper build'

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

[[ "$(git -C "${airbender_dir}" rev-parse --show-toplevel)" == "${airbender_dir}" ]] || fail 'expected checkout root'
[[ "$(git -C "${airbender_dir}" rev-parse HEAD)" == "${airbender_commit}" ]] || fail 'Airbender commit mismatch'
[[ "$(git -C "${airbender_dir}" rev-parse 'HEAD^{tree}')" == "${airbender_tree}" ]] || fail 'Airbender tree mismatch'
[[ -z "$(git -C "${airbender_dir}" status --porcelain)" ]] || fail 'Airbender checkout must be pristine'
readonly fork_source="${airbender_dir}/tools/cli/src/bin/end_params.rs"
[[ "$(sha256_file "${fork_source}")" == "${source_sha256}" ]] || fail 'Airbender helper source mismatch'
[[ "$(sha256_file "${assets_dir}/src/end_params.rs")" == "${source_sha256}" ]] || fail 'packaged helper source mismatch'
[[ "$(sha256_file "${assets_dir}/Cargo.toml")" == "${manifest_sha256}" ]] || fail 'harness manifest mismatch'
[[ "$(sha256_file "${assets_dir}/Cargo.lock")" == "${lock_sha256}" ]] || fail 'harness lock mismatch'
cmp "${fork_source}" "${assets_dir}/src/end_params.rs"

mkdir -m 0700 "${build_dir}"
mkdir -m 0700 "${build_dir}/src"
install -m 0600 "${assets_dir}/Cargo.toml" "${build_dir}/Cargo.toml"
install -m 0600 "${assets_dir}/Cargo.lock" "${build_dir}/Cargo.lock"
install -m 0600 "${assets_dir}/src/end_params.rs" "${build_dir}/src/end_params.rs"
rustc "+${toolchain}" --version --verbose > "${build_dir}/rustc-version.txt"
grep -Fqx 'commit-hash: 18d13b5332916ffca8eadb9106d54b5b434e9978' "${build_dir}/rustc-version.txt" || fail 'rustc identity mismatch'
grep -Fqx 'host: x86_64-unknown-linux-gnu' "${build_dir}/rustc-version.txt" || fail 'the reviewed build is Linux x86_64'

# --locked is mandatory; cache/network policy may additionally be tightened with
# CARGO_NET_OFFLINE=true after fetching the exact locked dependency graph.
cargo "+${toolchain}" build --locked --release \
  --manifest-path "${build_dir}/Cargo.toml" --bin end_params \
  --target-dir "${build_dir}/target"
[[ "$(sha256_file "${build_dir}/Cargo.lock")" == "${lock_sha256}" ]] || fail 'Cargo changed the reviewed lock'
[[ "$(sha256_file "${build_dir}/Cargo.toml")" == "${manifest_sha256}" ]] || fail 'build changed the harness manifest'
[[ "$(sha256_file "${build_dir}/src/end_params.rs")" == "${source_sha256}" ]] || fail 'build changed the Airbender source'
test -x "${build_dir}/target/release/end_params"
printf '%s  %s\n' \
  "${manifest_sha256}" Cargo.toml \
  "${lock_sha256}" Cargo.lock \
  "${source_sha256}" src/end_params.rs \
  "$(sha256_file "${build_dir}/target/release/end_params")" target/release/end_params \
  > "${build_dir}/SHA256SUMS"
printf '%s\n' "${build_dir}/target/release/end_params"
