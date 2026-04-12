# Contributing

Thanks for your interest. Vox is a small, opinionated tool — contributions are welcome, especially for:

- Cross-platform token storage (Linux/Windows) to replace the current macOS-Keychain-only setup.
- Bug fixes with clear reproduction steps.
- Documentation improvements.

## Development

```sh
git clone https://github.com/simonspoon/vox
cd vox
cargo build
cargo test
```

CI runs `fmt`, `clippy -D warnings`, `build`, and `test` on Ubuntu and macOS. Please run the same checks locally before opening a PR:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Pull requests

- Keep changes focused — one concern per PR.
- Commit messages: short imperative subject, no trailing period. Explain the *why* in the body if it isn't obvious from the diff.
- If you change command behavior or flags, update `README.md`.
- New features should include tests where practical.

## Scope

Vox is intentionally small — it bridges Claude Code agents to Telegram for async prompts. Proposals that significantly expand the surface area (other chat platforms, alternative UIs, daemonization, etc.) are likely to be declined. Open an issue to discuss before writing code.
