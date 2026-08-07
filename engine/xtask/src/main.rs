//! Workspace tooling, run via `cargo run -p xtask <task>`: the `slangc` shader fan-out and the
//! protocol codegen emitters.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

mod protocol;
mod shaders;
mod stars;
mod vegetation_fixture;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let task = args.next();
    match task.as_deref() {
        Some("shaders") => run_shaders(args.collect()),
        Some("gen-protocol") => run_gen_protocol(),
        Some("gen-vegetation-e2e-fixture") => run_gen_vegetation_e2e_fixture(args.collect()),
        Some("bake-stars") => run_bake_stars(args.collect()),
        Some(other) => bail!(
            "unknown task '{other}' (known: shaders, gen-protocol, gen-vegetation-e2e-fixture, bake-stars)"
        ),
        None => {
            bail!(
                "usage: cargo run -p xtask <task>  (known: shaders, gen-protocol, gen-vegetation-e2e-fixture, bake-stars)"
            )
        }
    }
}

/// `xtask gen-vegetation-e2e-fixture` — emit the canonical authored vegetation package
/// and the stress-matrix fixtures.
fn run_gen_vegetation_e2e_fixture(args: Vec<String>) -> Result<()> {
    if !args.is_empty() {
        bail!("usage: cargo run -p xtask -- gen-vegetation-e2e-fixture");
    }
    let dir = workspace_root_repo()?.join("tests/e2e/fixtures");
    for output in vegetation_fixture::write_all(&dir)? {
        println!(
            "xtask gen-vegetation-e2e-fixture: wrote {}",
            output.display()
        );
    }
    Ok(())
}

/// `xtask bake-stars <ybsc5>` — bake the fixed-width Yale BSC5 catalog into the runtime table.
fn run_bake_stars(args: Vec<String>) -> Result<()> {
    let [source] = args.as_slice() else {
        bail!("usage: cargo run -p xtask -- bake-stars <decompressed-ybsc5>");
    };
    let output = workspace_root().join("assets/night/bsc5.bin");
    let count = stars::bake(Path::new(source), &output)?;
    println!(
        "xtask bake-stars: wrote {count} stars to {}",
        output.display()
    );
    Ok(())
}

/// `xtask gen-protocol` — emit the editor-facing TypeScript, Luau, envelope, OpenRPC, and manifest
/// artifacts from the `saffron-protocol` DTO crate.
fn run_gen_protocol() -> Result<()> {
    let written = protocol::run(&workspace_root_repo()?)?;
    for path in &written {
        println!("xtask gen-protocol: wrote {}", path.display());
    }
    Ok(())
}

/// The repository root: the protocol artifacts live under `editor/` and `schemas/`, outside the
/// Cargo tree.
fn workspace_root_repo() -> Result<PathBuf> {
    workspace_root()
        .parent()
        .map(Path::to_path_buf)
        .context("workspace root (engine/) has a parent (the repository root)")
}

/// `xtask shaders [--profile <name>]` — the shader pipeline + asset copy. The profile selects
/// the cargo target dir (`target/<profile>/`) the host binary and its runtime assets live in.
fn run_shaders(args: Vec<String>) -> Result<()> {
    let mut profile = "debug".to_owned();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--profile" => {
                profile = iter
                    .next()
                    .context("--profile requires a value (e.g. debug, release)")?;
            }
            other => bail!("unknown shaders flag '{other}' (known: --profile <name>)"),
        }
    }

    let config = shaders::Config::resolve(&workspace_root(), &profile)?;
    println!("xtask shaders: using slangc {}", config.slangc.display());
    let report = shaders::run(&config)?;
    println!(
        "xtask shaders: {} compiled, {} up to date, shared modules {} -> {}/shaders",
        report.spv_compiled,
        report.spv_skipped,
        if report.module_compiled {
            "rebuilt"
        } else {
            "up to date"
        },
        config.runtime_dir.display()
    );
    Ok(())
}

/// The Cargo workspace root (`engine/`), independent of the process cwd.
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("xtask manifest dir has a parent (the workspace root)")
        .to_path_buf()
}
