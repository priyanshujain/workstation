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