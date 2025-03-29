# Contributing to ArtiGit

Thank you for your interest in contributing to ArtiGit! We welcome contributions from everyone, regardless of experience level. This document provides guidelines and instructions to help you get started with contributing to ArtiGit.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Environment](#development-environment)
- [Making Contributions](#making-contributions)
  - [Finding Issues](#finding-issues)
  - [Development Workflow](#development-workflow)
  - [Pull Request Guidelines](#pull-request-guidelines)
- [Coding Standards](#coding-standards)
- [Testing Guidelines](#testing-guidelines)
- [Documentation](#documentation)
- [Community](#community)

## Code of Conduct

We are committed to providing a friendly, safe, and welcoming environment for all contributors. Please read and follow our [Code of Conduct](CODE_OF_CONDUCT.md).

## Getting Started

1. **Fork the repository**: Start by forking the [ArtiGit repository](https://github.com/Tejasdodla/arti-git) on GitHub.

2. **Clone your fork**: 
   ```
   git clone https://github.com/YOUR-USERNAME/arti-git.git
   cd arti-git
   ```

3. **Set up upstream remote**:
   ```
   git remote add upstream https://github.com/Tejasdodla/arti-git.git
   ```

4. **Create a new branch**: Always work on a new branch, never directly on `main`.
   ```
   git checkout -b feature/your-feature-name
   ```

## Development Environment

### Prerequisites

- Rust (latest stable version, 1.75.0 or newer recommended)
- Git
- Cargo (included with Rust)
- A code editor (VS Code, IntelliJ, vim, etc.)

### Building the Project

```bash
# Build the project
cargo build

# Run tests
cargo test

# Run the application
cargo run -- --help
```

### Development Tools

We recommend setting up the following tools:

- **Rustfmt**: Format your Rust code automatically
  ```
  rustup component add rustfmt
  cargo fmt
  ```

- **Clippy**: Catch common mistakes and improve your Rust code
  ```
  rustup component add clippy
  cargo clippy
  ```

## Making Contributions

### Finding Issues

- Check the [Issues](https://github.com/Tejasdodla/arti-git/issues) page on GitHub for tasks labeled `good first issue` or `help wanted`.
- If you have an idea for a new feature, create an issue first to discuss it with the maintainers.

### Development Workflow

1. **Keep your fork updated**:
   ```
   git fetch upstream
   git checkout main
   git merge upstream/main
   ```

2. **Create a feature branch**:
   ```
   git checkout -b feature/your-feature
   ```

3. **Make your changes**: Write code, tests, and documentation.

4. **Commit your changes** with clear, descriptive commit messages:
   ```
   git commit -m "feature: implement xyz functionality"
   ```

   Follow the [Conventional Commits](https://www.conventionalcommits.org/) specification.

5. **Push to your fork**:
   ```
   git push origin feature/your-feature
   ```

### Pull Request Guidelines

1. **Create a pull request** from your branch to the `main` branch of the original repository.

2. **Fill in the pull request template** with a comprehensive description of your changes.

3. **Ensure CI passes**: Wait for the continuous integration checks to complete.

4. **Address review comments**: Be responsive to feedback from maintainers.

5. **Update your PR** if necessary by adding new commits. Don't force-push unless requested.

## Coding Standards

- Follow idiomatic Rust practices from [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/).
- Use `cargo fmt` to format your code before committing.
- Run `cargo clippy` to address potential issues and improve code quality.
- Write meaningful documentation comments for public APIs.
- Keep functions small and focused on a single responsibility.
- Use descriptive variable names and follow the established naming conventions.

## Testing Guidelines

- Write tests for all new functionality.
- Ensure existing tests pass with your changes.
- Include both unit tests and integration tests when appropriate.
- Test edge cases and error conditions.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_your_function() {
        // Test setup
        let result = your_function();
        // Assertions
        assert_eq!(result, expected_value);
    }
}
```

## Documentation

Good documentation is crucial for any project. When contributing:

- Add docstrings to all public functions, structs, and modules.
- Update existing documentation if your changes affect behavior.
- Consider adding examples where appropriate.
- Make sure API documentation builds without errors:
  ```
  cargo doc --no-deps
  ```

## Community

- Join our [GitHub Discussions](https://github.com/Tejasdodla/arti-git/discussions) for general questions and discussions.
- For quick questions, join our [Matrix chat](https://matrix.to/#/#artigit:matrix.org) (coming soon).

---

Thank you for contributing to ArtiGit! Your efforts help make this project better for everyone.