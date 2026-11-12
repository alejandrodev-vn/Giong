## What and why

<!-- What does this change, and what problem does it solve? Link the issue: "Fixes #123". -->

## How it was tested

<!-- Tests added or changed, and anything you ran by hand. Kernel version if relevant. -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `scripts/check.sh test` pass
- [ ] New behaviour has tests, including what happens when an operation is dropped mid-flight
- [ ] Every new `unsafe` block has a `// SAFETY:` comment, and `scripts/valgrind.sh` and `scripts/check.sh tsan` are clean
- [ ] Performance claims come with before/after numbers and the method
- [ ] `CHANGELOG.md` has an entry under "Unreleased" for user-visible changes
- [ ] The pull request title follows the commit convention in `CONTRIBUTING.md`
