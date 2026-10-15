default: check

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

test:
    cargo nextest run --workspace --locked --no-tests=pass

test-hooks:
    scripts/test-hooks.sh

test-harness:
    scripts/test-harness.sh

check: fmt-check lint test test-hooks test-harness

spike *args:
    cargo run -p spike -- {{args}}
