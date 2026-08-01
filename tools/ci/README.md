# CI / reproducible gate

`tools/ci/check.sh` is the single reproducible gate for the engine + CEF editor. It runs eleven
steps in dependency order, accumulates
failures (a failure in any one turns the whole gate red), and prints a per-step pass/fail
summary ending in a clear `ALL GATES PASSED` / `SOME GATES FAILED` verdict.

1. **workspace build** — `cargo build --workspace` + `cargo run -p xtask -- shaders`
   (the `slangc` fan-out + asset copy next to the host binary).
2. **codegen freshness** — `cargo run -p xtask -- gen-protocol` then `git diff --exit-code`
   over the generated wire + Luau artifacts (`editor/src/protocol/sa-types.ts`, the OpenRPC
   schema, the command manifest, `schemas/control/sa.generated.luau`). A drift means the
   committed artifacts no longer match the DTO source.
3. **unit + crate tests** — `cargo test --workspace`: the inline `#[cfg(test)]` + crate `tests/`
   suites, including every self-test oracle, the golden/snapshot byte-exact tests, and the
   physics cross-arch determinism gate (its x86 half — see below).
4. **self-test-removal assertion** — no `run*SelfTest` / `SAFFRON_SELFTEST` /
   `fn *self_test` appears *outside* a `#[cfg(test)]` module (i.e. no runtime self-test survives).
5. **present-only smoke + validation-clean** — boots the host bounded to 5 frames
   (`SAFFRON_EXIT_AFTER_FRAMES=5`) and greps the log for `ERROR  vulkan  [validation]`
   (the only automated detector for the silent GPU-state-bug class).
6. **control-schema contract** — `tools/check-control-schema/check.ts` diffs the live host's
   `help`/results against the generated manifest + OpenRPC, including the decimal-string-u64
   `assertRawU64` tripwire.
7. **project startup smoke** — `tools/check-projects/check.sh` boots the host on a project,
   imports a model + texture, saves, restarts, and re-reads — asserting the on-disk asset layout.
8. **performance budgets** — `tools/bench-foliage-phase1/check.ts` re-measures the phase-1 fixture
   and grades the fresh p95s and counters against `benchmarks/foliage-veg/phase-1-<device>.json`
   for *this* device. Hardware with no record of its own defers rather than borrowing another
   class's ceiling; a breach must reproduce before it fails.
9. **e2e** — the `tests/e2e` bun suite against the host (`SAFFRON_ANIMA_BIN` repointed at
   `engine/target/debug/saffron-host`).
10. **frontend** — `editor/` `bun run build` (gen `@saffron/protocol` → `tsc` → `vite build`) +
   `bun test`.
11. **lint** — `cargo fmt --check` + `cargo clippy --workspace -- -D warnings`.

The five standing gates are *in* the sequence, not adjacent to it: validation-clean (step 5),
the control-schema contract (step 6), the recorded performance budgets (step 8, with the records'
own derivation held inside step 3's `cargo test`), golden/snapshot + the cross-arch determinism
gate (also inside step 3), and the e2e validation-clean assertions (step 9).

## Prerequisites

This builds **only** on the local Fedora Silverblue host inside the `saffron-build` toolbox.
You need, all at once:

- the toolbox (Rust toolchain via `rust-toolchain.toml`, Vulkan 1.4 headers/loader/validation/
  tools, SDL3/winit display deps, slang) — see `AGENTS.md`;
- the **host bun** on `PATH` (steps 6–10 — the contract test, the budget grading, e2e, and
  frontend);
- a **display** — steps 5–9 open a Vulkan swapchain, so run a headless weston compositor and
  point SDL at it.

## Local one-liner (the everyday gate)

```sh
toolbox run -c saffron-build bash -lc '
  export PATH="/var/home/saffronjam/.bun/bin:$PATH" XDG_RUNTIME_DIR=/run/user/$(id -u)
  weston --backend=headless --width=1280 --height=720 --socket=wl-ci --idle-time=0 &
  sleep 2; export WAYLAND_DISPLAY=wl-ci SDL_VIDEODRIVER=wayland
  tools/ci/check.sh'
```

Or, from inside an already-prepared toolbox shell (display + bun set up), use `just check`,
which invokes this script the same way. The `just` recipes carry the toolbox/bun/display lore;
this script owns *what runs and in what order*.

## Hardware/display-gated items this gate cannot fully run

The toolbox is x86_64 + software GPU (llvmpipe), so two legs are intentionally out of reach here
and run on the self-hosted runner instead. They are documented, not silently skipped:

- **The determinism gate's aarch64 leg.** Step 3 runs the determinism gate's x86 half (the trace
  hash is stable run-to-run and build-to-build, flags confirmed active). The non-negotiable
  `x86 hash == aarch64 hash` assertion is owned by `physics/tests/determinism.rs` and is
  `DEFERRED-NEEDS-HARDWARE` until the self-hosted aarch64 runner re-derives the trace.

## Honest CI story

There is **no GitHub-hosted pipeline**, on purpose. A stock hosted runner (`ubuntu-latest`)
cannot build Saffron Anima: the toolchain lives in an immutable-OS toolbox, it needs the Vulkan
SDK + SDL3 + slang, and the smoke/schema/e2e steps need a headless GPU display. Reproducing that
with `apt install` is not feasible, and faking a green hosted run would be dishonest.

`.github/workflows/ci.yml` is therefore configured `runs-on: [self-hosted, saffron-build]` — it
only runs on a **self-hosted runner** provisioned with the `saffron-build` toolbox (or a container
image that replicates it) plus a headless weston. Until such a runner is registered, the workflow
simply queues; the `just check` / `check.sh` run locally is the gate that actually protects `main`.
