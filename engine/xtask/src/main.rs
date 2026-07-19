//! Workspace tooling, run via `cargo run -p xtask <task>`: the `slangc` shader fan-out and the
//! protocol/codegen emitters. Not shipped; an explicit build step invoked by `just engine` and
//! the gate.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

mod protocol;
mod shaders;
mod stars;

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
        Some("bake-stars") => run_bake_stars(args.collect()),
        Some(other) => bail!("unknown task '{other}' (known: shaders, gen-protocol, bake-stars)"),
        None => {
            bail!("usage: cargo run -p xtask <task>  (known: shaders, gen-protocol, bake-stars)")
        }
    }
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

/// `xtask gen-protocol` — emit the editor-facing protocol artifacts (`sa-types.ts`, the OpenRPC
/// schema, the command manifest) from the `saffron-protocol` DTO crate.
fn run_gen_protocol() -> Result<()> {
    let written = protocol::run(&workspace_root_repo()?)?;
    for path in &written {
        println!("xtask gen-protocol: wrote {}", path.display());
    }
    Ok(())
}

/// The repository root (`engine/`'s parent): the protocol artifacts live under `editor/` and
/// `schemas/`, outside the Cargo tree, so the emitter writes against the repo root, not the
/// workspace.
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
        "xtask shaders: {} compiled, {} up to date, lighting module {} -> {}/shaders",
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

/// The Cargo workspace root (`engine/`): `xtask`'s manifest dir is `engine/xtask`, so the parent
/// is the workspace. Independent of the process cwd.
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("xtask manifest dir has a parent (the workspace root)")
        .to_path_buf()
}
