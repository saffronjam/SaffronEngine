//! The one shared multi-thread tokio runtime the async command handlers block on (store connectors
//! over reqwest, native dialogs over the XDG portal). Created on first use so unit tests that never
//! reach an async command don't spin one up. Handlers run on IPC worker threads, so `block_on` here
//! never stalls the CEF pump on the main thread.

use std::sync::OnceLock;
use tokio::runtime::Runtime;

pub fn rt() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().expect("build the shared tokio runtime"))
}
