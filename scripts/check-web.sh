#!/usr/bin/env bash
# Build the verified frontend before Rust embeds it. Install pinned npm packages
# explicitly with npm ci; this gate performs no implicit dependency downloads.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

npm --prefix crates/assistant-web/frontend run check
npm --prefix crates/assistant-web/frontend test
npm --prefix crates/assistant-web/frontend run build
cargo fmt --all --check
cargo check --workspace
cargo test --workspace
