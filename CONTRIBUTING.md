# Contributing to txio-cli

First off, thank you for considering contributing! It's people like you that make txio a single terminal for every chain.

## How Can I Contribute?

### Reporting Bugs

Before creating a bug report, please check the [existing issues](https://github.com/Txio-labs/txio-cli/issues). When you do open one, please include:

* A clear and descriptive title
* The exact command you ran, including `--network`/`--rpc-url` flags
* What you expected to happen vs. what actually happened
* Your OS and how you installed txio (cargo/Homebrew/npm/install script)

### Suggesting Enhancements

Enhancement suggestions are tracked as GitHub issues. Please include:

* A clear and descriptive title
* A step-by-step description of the suggested enhancement
* Why it would be useful to most txio users

### Pull Requests

* Do not include issue numbers in the PR title.
* Before merging, automated checks must pass: build, tests, and lint.
* End all files with a newline.
* Include example command output in your PR description when you change CLI behavior or output formatting.

## Development Setup

```bash
cargo build
cargo run -- --help
cargo test
```

For local iteration without reinstalling: `cargo run -- sui balance <address>` etc.

## Adding a Chain

txio uses a `ChainAdapter` trait — every chain lives in `src/chains/`.

1. Add a new file at `src/chains/<chain>.rs`.
2. Implement `ChainAdapter` — at minimum, `call_rpc` and `get_balance`.
3. Register it in `ChainFactory` (`src/chains/factory.rs`).

See the existing adapters (`sui.rs`, `ethereum.rs`, `solana.rs`, `aptos.rs`, `soroban.rs`) for the expected shape.

## Styleguides

### Git Commit Messages

* Use the present tense ("Add feature" not "Added feature")
* Use the imperative mood ("Move cursor to..." not "Moves cursor to...")
* Limit the first line to 72 characters or less
* Reference issues and pull requests liberally after the first line

### Code Style

* Run `cargo fmt` and `cargo clippy` before opening a PR
* Keep chain-specific logic inside its adapter — shared CLI parsing/formatting lives in `src/cli/`
