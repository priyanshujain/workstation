# ws - Declarative Workstation Configuration

A Rust tool for declaratively managing your macOS workstation. Define your packages, dotfiles, system settings, and more in type-safe Rust code.

## Features

- **Declarative Configuration**: Define your entire workstation setup in Rust code with compile-time validation
- **Scopes & Profiles**: Organize tools by purpose (personal, work) and create machine profiles that combine scopes
- **Dependency-Aware**: Resources are applied in the correct order based on dependencies
- **Dry-Run Mode**: Preview changes before applying them
- **Testable**: Full mock infrastructure allows testing without actual system changes
- **CI-Ready**: Unit tests run on Linux, E2E tests on macOS

## Quick Start

```bash
# See available profiles
cargo run -- profiles

# Preview what would change
cargo run -- diff --profile work-macbook

# Apply configuration
cargo run -- apply --profile work-macbook

# Dry run (show plan without executing)
cargo run -- apply --profile work-macbook --dry-run
```

## Configuration

Define your workstation in Rust using the builder API:

```rust
use ws_dsl::prelude::*;

pub fn config() -> Workstation {
    Workstation::builder("my-workstation")
        // Personal tools - used on all my machines
        .scope("personal", |s| s
            // Terminal & Shell
            .brew_cask("ghostty")

            // Productivity
            .brew_cask("raycast")

            // Editors
            .brew_cask("visual-studio-code")
            .brew_formula("neovim")

            // CLI Tools
            .brew_formula("git")
            .brew_formula("ripgrep")
            .brew_formula("fzf")

            // Containers
            .brew_cask("docker")
        )

        // Work-specific tools
        .scope("okcredit", |s| s
            .brew_cask("datagrip")
        )

        // Machine profiles
        .profile("personal-macbook", &["personal"])
        .profile("work-macbook", &["personal", "okcredit"])

        .build()
}
```

## CLI Commands

```
ws apply --profile <name>    Apply the declared configuration
    -n, --dry-run            Show what would change without making changes
    -y, --yes                Don't ask for confirmation

ws diff --profile <name>     Show what would change

ws profiles                  List available profiles and scopes
```

## Project Structure

```
workstation/
├── crates/
│   ├── ws-core/             # Core abstractions (Resource trait, graph, executor)
│   ├── ws-dsl/              # DSL builder API
│   └── ws-cli/              # CLI binary
├── macos/                   # macOS-specific resources (Homebrew, defaults, launchd)
├── linux/                   # (Future) Linux-specific resources
├── common/                  # (Future) Cross-platform resources
└── examples/
    └── my-workstation/      # Example configuration
```

## Core Concepts

### Resources

Everything that can be managed is a `Resource`:

```rust
pub trait Resource {
    fn id(&self) -> ResourceId;
    fn depends_on(&self) -> Vec<ResourceId>;
    fn detect(&self, ctx: &Context) -> Result<ResourceState>;
    fn diff(&self, current: &ResourceState) -> Result<Change>;
    fn apply(&self, change: &Change, ctx: &Context) -> Result<()>;
}
```

### Scopes

Group related resources together:

```rust
.scope("personal", |s| s
    .brew_formula("git")
    .brew_formula("ripgrep"))

.scope("work", |s| s
    .brew_cask("slack")
    .brew_cask("zoom"))
```

### Profiles

Define which scopes are active on each machine:

```rust
.profile("home-laptop", &["personal"])
.profile("work-laptop", &["personal", "work"])
```

## Supported Resources

### Currently Implemented

| Resource | Description |
|----------|-------------|
| `BrewFormula` | Homebrew formulae (CLI tools) |
| `BrewCask` | Homebrew casks (GUI applications) |

### Planned

| Resource | Description |
|----------|-------------|
| `CargoPackage` | Rust crates via cargo |
| `NpmGlobal` | Global npm packages |
| `UvTool` | Python tools via uv |
| `PyenvVersion` | Python versions via pyenv |
| `NvmVersion` | Node.js versions via nvm |
| `Dotfile` | Symlinked dotfiles |
| `Template` | Templated configuration files |
| `MacOSDefaults` | macOS system preferences |
| `LaunchAgent` | macOS background services |

## Testing

The project uses a `CommandRunner` abstraction for testability:

```rust
// In tests, use MockCommandRunner
let mock = Arc::new(MockCommandRunner::new()
    .expect("brew", &["list", "--formula", "git"], CommandOutput::success("git 2.43.0")));

let ctx = Context::with_command_runner("test", mock);
let formula = BrewFormula::new("git");
let state = formula.detect(&ctx).unwrap();
```

Run tests:

```bash
# Run all tests (works on any platform)
cargo test

# Run with verbose output
cargo test -- --nocapture
```

## Development

### Prerequisites

- Rust 1.70+
- macOS (for E2E tests with Homebrew)

### Building

```bash
# Build all crates
cargo build

# Build release
cargo build --release

# Run clippy
cargo clippy --workspace

# Format code
cargo fmt --all
```

### CI

The GitHub Actions workflow runs:

- **Unit tests**: On Ubuntu (fast, cheap) - uses mocks, no Homebrew needed
- **Build check**: On Ubuntu and macOS
- **E2E tests**: On macOS (nightly/manual) - actually runs Homebrew commands
- **Documentation**: Ensures docs compile

## Roadmap

- [x] **Phase 1**: Core framework + Homebrew support
- [x] **Testing**: CommandRunner abstraction + mocks + CI
- [ ] **Phase 2**: More package managers (cargo, npm, uv, pyenv, nvm)
- [ ] **Phase 3**: Dotfile management (symlinks, templates)
- [ ] **Phase 4**: macOS settings (defaults, dock, finder)
- [ ] **Phase 5**: Services & Auth (launchd, gcloud, github, aws)
- [ ] **Phase 6**: Secrets (age encryption) & Sync (drift detection → PR)
- [ ] **Phase 7**: DX polish (watch mode, shell completions)

## Design Philosophy

1. **Type Safety**: Rust DSL provides compile-time validation and IDE autocomplete
2. **Testability**: All system interactions are abstracted for easy mocking
3. **Idempotency**: Running `ws apply` multiple times produces the same result
4. **Transparency**: Always show what will change before doing it
5. **Modularity**: Scopes let you organize and selectively apply configurations

## License

MIT
