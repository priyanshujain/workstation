# ws - Declarative Workstation Configuration

A Rust tool for declaratively managing your macOS workstation. Define your packages, tools, and applications in type-safe Rust code.

## Features

- **Declarative Configuration**: Define your workstation setup in Rust with compile-time validation
- **Scopes & Profiles**: Organize tools by purpose (personal, work) and create machine profiles
- **Dependency-Aware**: Resources are applied in the correct order based on dependencies
- **Dry-Run Mode**: Preview changes before applying them
- **Disk Audit & Cleanup**: Scan disk usage by category and interactively clean caches
- **Testable**: Full mock infrastructure — unit tests run without system changes

## Quick Start

```bash
# See available profiles
cargo run -- profiles

# Preview what would change
cargo run -- diff --profile work-macbook

# Apply configuration (dry run)
cargo run -- apply --profile work-macbook --dry-run

# Audit disk usage
cargo run --bin ws-audit

# Interactive disk cleanup TUI
cargo run --bin ws-cleanup
```

## Configuration

Define your workstation in `examples/my-workstation/src/lib.rs`:

```rust
use ws_dsl::prelude::*;

pub fn config() -> Workstation {
    Workstation::builder("my-workstation")
        .scope("personal", |s| {
            s.brew_formula("git")
                .brew_formula("ripgrep")
                .brew_formula("neovim")
                .brew_formula("go")
                .brew_formula("rustup")
                .brew_cask("ghostty")
                .brew_cask("visual-studio-code")
                .brew_cask("docker")
                .brew_cask("raycast")
        })
        .scope("work", |s| {
            s.brew_cask("datagrip")
                .brew_cask("google-cloud-sdk")
        })
        .profile("personal-macbook", &["personal"])
        .profile("work-macbook", &["personal", "work"])
        .build()
}
```

## Disk Tools

### `ws-audit` — Disk Usage Report

Scans your machine and shows a categorized breakdown of disk usage:

```
  Disk  74%
  ██████████████████████░░░░░░░░  339.9 GB used / 460.4 GB total / 69.8 GB free

    104.1 GB  Docker
     71.5 GB  Downloads
     36.0 GB  Go
      9.5 GB  Node.js
      8.5 GB  Xcode
      ...
```

Categories: Docker, Go, Node.js, Python, Rust, Kotlin/Native, Gradle, Xcode, Homebrew, App Caches, Downloads.

### `ws-cleanup` — Interactive TUI

Select cleanup targets with checkboxes, confirm, and execute:

- Homebrew cache, Go/npm/pnpm caches, Playwright browsers
- Chrome/Slack caches, Xcode DerivedData & stale simulators
- Docker unused data, DMG installers in Downloads

## Project Structure

```
workstation/
├── crates/
│   ├── ws-core/         # Core abstractions (Resource trait, graph, executor)
│   ├── ws-dsl/          # DSL builder API
│   ├── ws-cli/          # CLI binary (apply, diff, profiles)
│   └── ws-cleanup/      # Disk audit and cleanup tools
├── macos/               # macOS resources (Homebrew)
├── examples/
│   └── my-workstation/  # Workstation configuration
└── justfile             # Development recipes
```

## Development

```bash
# Install just (if not already)
brew install just

# Run all CI checks
just ci

# Individual checks
just test         # cargo test --workspace
just lint         # cargo clippy --workspace -- -D warnings
just fmt-check    # cargo fmt --all -- --check
just fmt          # auto-fix formatting

# Disk tools
just audit        # scan disk usage
just cleanup      # interactive cleanup TUI
```

## CI

GitHub Actions runs on every push and nightly:

- **Unit tests** + fmt + clippy on Ubuntu
- **Build check** on Ubuntu and macOS
- **E2E tests** on macOS (nightly) — runs actual Homebrew commands
- **Documentation** build

## License

MIT
