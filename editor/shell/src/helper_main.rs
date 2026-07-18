//! The macOS CEF helper executable. CEF on macOS runs each subprocess (renderer, GPU, plugin,
//! alerts) from its own helper `.app` inside the main bundle's `Contents/Frameworks`; every
//! helper app's executable is this binary. It loads the CEF framework (helper-relative path:
//! `../../../Chromium Embedded Framework.framework`) and hands the process to CEF with the
//! subprocess-side app: the renderer registers `window.cefQuery` via the message router and the
//! custom schemes, exactly like the re-exec'd shell does for subprocesses on Linux.

#[cfg(target_os = "macos")]
#[path = "ipc_render.rs"]
mod ipc_render;

#[cfg(target_os = "macos")]
#[path = "schemes.rs"]
mod schemes;

#[cfg(target_os = "macos")]
use cef::{App, ImplApp, WrapApp, rc::Rc, wrap_app};

#[cfg(target_os = "macos")]
wrap_app! {
    struct HelperApp;

    impl App {
        fn render_process_handler(&self) -> Option<cef::RenderProcessHandler> {
            // Invoked in the render subprocess: registers `cefQuery` via the renderer-side router.
            Some(ipc_render::render_process_handler())
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut cef::SchemeRegistrar>) {
            // Custom schemes must be registered identically in every process.
            if let Some(registrar) = registrar {
                schemes::register(registrar);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    use cef::{api_hash, args::Args, execute_process, library_loader::LibraryLoader, sys};

    let exe = std::env::current_exe().expect("helper: current_exe");
    let loader = LibraryLoader::new(&exe, true);
    assert!(loader.load(), "helper: cef_load_library failed");

    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let mut app = HelperApp::new();
    let ret = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    // The framework stays loaded until the helper exits; `loader` drops (and unloads) here.
    drop(loader);
    std::process::ExitCode::from(ret.clamp(0, 255) as u8)
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("saffron-editor-shell-helper is a macOS CEF helper; nothing to do on this platform");
    std::process::ExitCode::from(1)
}
