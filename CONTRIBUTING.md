# Contributing to Anamnesis

Thank you for considering contributing to Anamnesis! This document explains how to get started.

## Development Setup

```bash
# Clone the repository
git clone https://github.com/INONONO66/anamnesis.git
cd anamnesis

# Build
cargo build

# Run tests
cargo test

# Lint
cargo clippy --all-targets --all-features -- -D warnings

# Format check
cargo fmt --check
```

**Minimum Rust version:** 1.88 (2024 edition)

## Making Changes

### Before You Start

- Check existing [issues](https://github.com/INONONO66/anamnesis/issues) to avoid duplicate work.
- For non-trivial changes, open an issue first to discuss the approach.

### Pull Request Process

1. Fork the repository and create a branch from `main`.
2. Make your changes.
3. Ensure **all checks pass** before submitting:
   ```bash
   cargo fmt --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all-features
   cargo test --doc --all-features
   cargo test --all-targets --all-features --no-run
   ```
4. Open a pull request against `main`.

### Commit Messages

We follow [Conventional Commits](https://www.conventionalcommits.org/):

```
type(scope): description
```

- **type**: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`, `bench`
- **scope**: `graph`, `mechanics`, `query`, `storage`, `api`, `engine` (comma-separated for multiple)
- **description**: lowercase, imperative mood, no period, under 72 characters

Examples:
```
feat(mechanics): add polynomial decay function
fix(query): prevent infinite loop in cyclic graphs
test(engine): add property tests for ingestion pipeline
```

### Code Style

- **No `unwrap()` in library code** — use `Result<T, E>` everywhere.
- **No `println!`** — this is a library. Use the `log` crate if logging is needed.
- **No global state** — all state belongs in `Engine` instances.
- **No type error suppression** — no `as any` equivalents or unsafe blocks without justification.
- **Zero external dependencies for core** — only `std`. Storage adapters may use external crates.
- **Pure functions for mechanics** — scoring and decay functions must have no side effects.

### Tests

- New features should include tests in `tests/`.
- Pure functions should consider property-based testing (`proptest`).
- Run `cargo test --all-targets --all-features --no-run` to verify tests and benchmark targets compile without executing long-running benchmark binaries.

## Reporting Bugs

Use the [bug report template](https://github.com/INONONO66/anamnesis/issues/new?template=bug_report.yml) and include:
- Rust version (`rustc --version`)
- Minimal reproduction code
- Expected vs. actual behavior

## Requesting Features

Use the [feature request template](https://github.com/INONONO66/anamnesis/issues/new?template=feature_request.yml). Explain the use case and why existing functionality doesn't cover it.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
