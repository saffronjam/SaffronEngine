//! Sets an executable-relative rpath on the player binary so a staged export finds the C++
//! runtime libs (pulled in by the vendored Jolt physics) sitting beside the executable. The
//! default library search path still applies, so a dev run resolves them from the system as
//! before — this only adds the folder-local lookup the shipped app needs. The rpath token is
//! platform-specific: ELF linkers use `$ORIGIN`, the Mach-O linker uses `@executable_path`.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS")
        .expect("CARGO_CFG_TARGET_OS is always set for a build script");
    match target_os.as_str() {
        "macos" => {
            // Mach-O: `@executable_path` is the binary-relative token; there is no `-z origin`
            // (that is a GNU-ld ELF directive `ld64` rejects).
            println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
        }
        _ => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
            // Some ELF linkers require `-z origin` to honor `$ORIGIN` in an rpath.
            println!("cargo:rustc-link-arg=-Wl,-z,origin");
        }
    }
}
