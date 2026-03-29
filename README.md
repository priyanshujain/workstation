# wsctl — Workstation Controller

A Rust CLI for declaratively managing macOS workstation setup. Define packages, tools, and applications in type-safe Rust code, then apply them with a single command.

## Usage

```bash
# Install
cargo install --path .

# See available profiles
wsctl profiles

# Preview what would change
wsctl diff work-macbook

# Apply configuration
wsctl apply work-macbook

# Disk audit
wsctl audit

# Interactive disk cleanup TUI
wsctl cleanup
```

### Subcommands

| Command | Description |
|---------|-------------|
| `wsctl apply <PROFILE>` | Install/update packages for a profile |
| `wsctl diff <PROFILE>` | Preview what would change |
| `wsctl profiles` | List available profiles and scopes |
| `wsctl audit` | Show disk usage by category |
| `wsctl cleanup` | Interactive TUI for disk cleanup |

### Flags

- `--json` — Machine-readable output (on `profiles`, `diff`, `audit`)
- `--yes` / `-y` — Skip confirmation prompt (on `apply`)
- `--dry-run` / `-n` — Show plan without executing (on `apply`)
- `--quiet` / `-q` — Errors only
- `-v` / `-vv` / `-vvv` — Increasing verbosity

## Configuration

Edit `src/config.rs` to define your workstation:

```rust
Workstation::builder("my-workstation")
    .scope("personal", |s| {
        s.brew_formula("git")
            .brew_formula("ripgrep")
            .brew_cask("ghostty")
            .brew_cask("raycast")
    })
    .scope("work", |s| {
        s.brew_cask("datagrip")
    })
    .profile("personal-macbook", &["personal"])
    .profile("work-macbook", &["personal", "work"])
    .build()
```

## Project Structure

```
workstation/
├── crates/
│   └── wsctl-core/       # Core abstractions + disk scanning
├── macos/                # macOS resources (Homebrew)
├── src/                  # wsctl binary
│   ├── main.rs           # CLI entry point
│   ├── builder.rs        # Workstation/ScopeBuilder
│   ├── config.rs         # Workstation configuration
│   ├── tui.rs            # Cleanup TUI
│   └── commands/         # Subcommand implementations
├── tests/
│   └── installation.rs   # Brew install lifecycle test
└── justfile
```

## Development

```bash
just ci           # fmt + lint + test + build
just test         # cargo test --workspace
just lint         # cargo clippy --workspace -- -D warnings
just audit        # scan disk usage
just cleanup      # interactive cleanup TUI
just install      # cargo install --path .
```

## License

MIT
