+++
title = 'Rust house style'
weight = 1
+++

# Rust house style

The house style is the set of code conventions the whole workspace follows, held in place by a
lint gate rather than by review habit. The design vocabulary itself (plain structs, traits as
interfaces, fn-pointer itables) has its own page,
[Go-flavored design](../../core-and-conventions/go-flavored-design/); this page covers the written
rules and the machinery that enforces them.

## Clippy is law

The workspace manifest turns the whole [Clippy](https://doc.rust-lang.org/clippy/) `all` group on
as warnings, and the gate promotes every warning to an error: `just lint` runs
`cargo clippy --workspace -- -D warnings` after `cargo fmt --check`. A change that trips a lint is
not finished until the lint is clean.

```toml
[workspace.lints.rust]
unsafe_code = "deny"

[workspace.lints.clippy]
all = "warn"
```

```sh
just lint
# cd engine && cargo fmt --check
# cd engine && cargo clippy --workspace -- -D warnings
# bun run format:check           (oxfmt over every .ts/.tsx in the tree)
# bun run lint                   (oxlint --deny-warnings over the same set)
```

## Unsafe is opt-in, one crate per FFI seam

`unsafe_code = "deny"` applies workspace-wide, and most crate roots repeat the deny. Exactly three
crates opt back in with a crate-root `#![allow(unsafe_code)]`, each owning one foreign boundary;
every `unsafe` block inside them carries a `// SAFETY:` comment naming the invariant it relies on.

| Crate | Seam |
|---|---|
| `saffron-rendering` | `ash` Vulkan calls and the VMA allocator (raw C bindings) |
| `saffron-physics-sys` | the `cxx` bridge into vendored Jolt |
| `saffron-host` | the shared-memory frame-publisher wiring and its raw syscalls |

The unsafety never escapes. Each crate wraps its seam in safe methods (`Device::new`,
`Renderer::render_frame`), so no caller of these crates touches a raw handle.
[Dependencies](../dependencies/) covers how the FFI crates are pinned and built.

## Errors are typed values

Fallible work returns `Result<T>`, never a panic on an expected failure. Each library crate
declares its own error enum with [`thiserror`](https://docs.rs/thiserror) and exports a
`Result<T>` alias over it; callers compose errors with `#[from]` and propagate with `?`.
[Error handling](../../core-and-conventions/error-handling/) walks through the whole model. A
panic marks a broken invariant the type system cannot express, and `#[should_panic]` tests pin
those.

## Sharing is explicit

A read-shared handle is an `Arc<T>`, written through the `Ref<T>` alias `saffron-core` exports for
a value built once and then only read. A shared-mutable site spells `Arc<Mutex<T>>` (or
`Arc<RwLock<T>>`) at its declaration, so the exception is visible where it occurs.
[Ownership](../../core-and-conventions/ownership-and-raii/) covers `Drop`, the
device-outlives-resources guarantee, and teardown order.

## Comments say what, not when

A public item carries a brief `///` saying what it is, plus a why when that is not obvious from
the name. There are no section or banner dividers, and a comment describes the code as it stands,
never by contrast with an earlier shape of it. The written rules themselves live in `AGENTS.md`
at the repo root.

## In the code

| What | File | Symbols |
|---|---|---|
| The lint gate | `engine/Cargo.toml` | `[workspace.lints.rust]` (`unsafe_code = "deny"`), `[workspace.lints.clippy]` (`all = "warn"`) |
| The gate invocation | `justfile` | the `lint` recipe (`cargo clippy --workspace -- -D warnings`) |
| The three unsafe opt-ins | `engine/crates/{rendering,physics-sys,host}/src/lib.rs` | `#![allow(unsafe_code)]` plus each crate-root seam note |
| The written conventions | `AGENTS.md` | the "Conventions (not optional)" section |

## Related

- [Go-flavored design](../../core-and-conventions/go-flavored-design/) — the design vocabulary these rules protect
- [Error handling](../../core-and-conventions/error-handling/) — the `thiserror` / `Result<T>` model in full
- [Ownership](../../core-and-conventions/ownership-and-raii/) — `Drop`, `Arc<T>`, and teardown
- [Dependencies](../dependencies/) — the single pin list and the FFI crates
- [Build environment](../build-environment/) — the toolbox the gate runs in
