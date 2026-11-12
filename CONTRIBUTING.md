# Contributing

- Issues and pull requests are welcome, in English or Japanese.
- Before a pull request, run what CI runs: `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`.
- The code is not formatted with rustfmt; keep the style of the surrounding code.
- After changing dependencies, run `python3 packaging/licenses.py` and commit `THIRD_PARTY_LICENSES`.
- MornDeck is released into the public domain under the [Unlicense](LICENSE). By contributing, you dedicate your contribution to the public domain under the same terms.
