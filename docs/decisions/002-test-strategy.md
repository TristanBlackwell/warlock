# ADR-002: Two-Tier Test Strategy

**Status:** Accepted
**Date:** 2026-02-27
**Context:** Testing a Linux-only control plane across macOS dev, CI, and production hosts

## Context

Warlock is a Firecracker microVM control plane that only runs in production on
Linux with KVM, the Firecracker binary, the jailer, and a specific filesystem
layout (`/opt/firecracker`, `/srv/jailer`). Development happens on macOS, and
CI runs on GitHub Actions (Ubuntu without KVM or Firecracker installed).

This creates a testing gap: the most important code paths (VM creation, jailer
isolation, rootfs copying, the full lifecycle) cannot execute without the real
Firecracker stack. At the same time, a large surface area -- validation logic,
error handling, HTTP response structure, capacity math, version parsing -- has
no dependency on Firecracker at all.

## Decision

Split tests into two tiers with different execution requirements:

### Tier 1: Fast Tests (`cargo test`)

Run everywhere -- macOS, CI, provisioned hosts. The server starts in development
mode (`WARLOCK_DEV=true`), which returns a dummy `JailerConfig` and skips all
Firecracker preflight checks.

**What they cover:**

- Unit tests for domain logic (validation, capacity math, cgroup config, version parsing)
- Unit tests for error types (`ApiError` variants, `IntoResponse`, obfuscation)
- Integration tests against the HTTP API for paths that don't require Firecracker:
  - Validation errors (422) -- invalid vCPUs, memory below minimum
  - Not found (404) -- nonexistent VM IDs
  - Bad path parameters (400) -- non-UUID path segments
  - Error obfuscation (500) -- filesystem operations fail in dev mode, client sees generic error
  - Healthcheck structure (200) -- response shape, capacity fields, copy strategy
  - Empty VM list (200) -- baseline list response

**Location:** `src/**/tests` (inline unit tests) and `tests/vm.rs`, `tests/healthcheck.rs`.

### Tier 2: Live Tests (`make test-live`)

Run only on a fully provisioned host with Firecracker, KVM, jailer, kernel,
rootfs, and the `/srv/jailer/` directory layout. Gated by the `WARLOCK_LIVE=true`
environment variable -- without it, every test returns early.

The server starts in production mode (no `WARLOCK_DEV`), running real preflight
checks and detecting the actual cgroup version and copy strategy.

**What they cover:**

- Full VM lifecycle: create (202) -> get (200, running) -> list (includes VM) -> delete (200) -> verify 404 -> verify rootfs cleanup
- Non-default VM configuration: custom vCPUs and memory are applied end-to-end
- Healthcheck with running VMs: allocated resources are reflected in the response

**Location:** `tests/vm_lifecycle.rs` (self-contained, does not share the dev-mode test server).

### Running

```sh
# Tier 1 -- everywhere (macOS, CI, droplet)
cargo test
make test

# Tier 2 -- provisioned host only
make test-live
# or: WARLOCK_LIVE=true cargo test --test vm_lifecycle -- --nocapture
```

## Alternatives Considered

### 1. Mock/fake the Firecracker SDK

Introduce a trait (e.g. `VmManager`) and provide a fake implementation for
tests. The handler would call the trait instead of the SDK directly.

- **Pro:** Full lifecycle tests run everywhere, no Firecracker needed
- **Con:** Significant refactoring of the handler layer. The mock would need to
  replicate SDK behaviour (state transitions, error conditions, drop semantics).
  Mocks can drift from real behaviour, giving false confidence. The handler is
  a thin orchestration layer -- the value of testing it with a fake is low
  compared to testing it against the real thing.
- **Verdict:** Over-engineered for the current codebase size. The risk of mock
  drift outweighs the convenience.

### 2. `#[ignore]` attribute

Mark live tests with `#[ignore]` and run them with `cargo test -- --ignored`.

- **Pro:** Built-in Rust mechanism, no custom gating code
- **Con:** `#[ignore]` tests are visible in `cargo test` output as "ignored",
  which is noisy and unclear about *why* they're ignored. Easy to accidentally
  run `-- --ignored` on a machine without Firecracker and get confusing failures.
  No way to convey the prerequisite in the test attribute itself.
- **Verdict:** Env var gating is more explicit and self-documenting.

### 3. Docker-based test environment

Run live tests inside a Docker container with nested KVM.

- **Pro:** Reproducible, could run in CI with KVM-enabled runners
- **Con:** Nested virtualisation is slow and not available on all CI providers.
  Adds Docker as a dependency. The droplet already provides the real environment.
- **Verdict:** Unnecessary complexity. The droplet is the canonical test host.

### 4. Single test tier with conditional compilation

Use `#[cfg(target_os = "linux")]` to gate live tests at compile time.

- **Pro:** No runtime check, tests simply don't exist on macOS
- **Con:** Tests still compile on Linux CI (GitHub Actions) where Firecracker
  isn't installed, so they'd fail at runtime. Would need an additional env var
  check anyway, making `cfg` redundant.
- **Verdict:** `cfg` alone is insufficient; the env var is needed regardless.

## Consequences

### Positive

- `cargo test` is fast and safe on every platform -- no risk of accidentally
  spawning Firecracker processes
- Live tests exercise the real Firecracker stack end-to-end, catching integration
  issues that no amount of mocking would surface
- Clear separation: developers know which tests run where and why
- No production code changes required -- the test tiers are purely a test
  infrastructure concern

### Negative

- Live tests can only run on a provisioned host, so they are not part of the
  CI pipeline. Regressions in the Firecracker integration path are caught later
  (on the droplet) rather than at PR time.
- The two-tier split means some paths are only tested in one tier. Specifically,
  the happy path (create -> get -> delete) is only covered by live tests.
- Live tests share a single in-process server, so they are not fully isolated
  from each other. Test ordering could theoretically matter if a test leaks a VM,
  though each test cleans up after itself.
