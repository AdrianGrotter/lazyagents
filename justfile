set positional-arguments

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

smoke-tui:
    scripts/smoke-tui.sh

test-tui-hangup:
    scripts/test-tui-hangup.sh

check: fmt-check lint test test-hooks test-harness smoke-tui test-tui-hangup

[no-cd]
dev *args:
    cargo run --locked --manifest-path {{justfile_directory()}}/Cargo.toml -p lazyagents -- "$@"
