## Prerequisites

This project requires:

* The **Foundry nightly toolchain**
* The **Rust toolchain**

### Install Foundry (v1.6.0-5bcdddc06a)

To install [Foundry](https://getfoundry.sh/):

```bash
# Download the Foundry installer
curl -L https://foundry.paradigm.xyz | bash

# Install forge, cast, anvil, chisel
# Temporarily we require custom version of anvil that supports compressed state.
# Hopefully https://github.com/foundry-rs/foundry/pull/13244 will get accepted, then we will
# be able to switch to nightly.
foundryup -r itegulov/foundry -C 5bcdddc06abe5b0cd8e9bc1de8ddfb7202a95ed1
```

Verify your installation reports correct version:

```bash
$ anvil --version
anvil Version: 1.6.0-dev
Commit SHA: 5bcdddc06abe5b0cd8e9bc1de8ddfb7202a95ed1
...
```

### Install Rust

Install [Rust](https://www.rust-lang.org/tools/install) using `rustup`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

After installation, ensure Rust is available:

```bash
rustc --version
```

### Linux packages

```bash
# essentials
sudo apt-get install -y build-essential pkg-config cmake clang lldb lld libssl-dev apt-transport-https ca-certificates curl software-properties-common git    
```
