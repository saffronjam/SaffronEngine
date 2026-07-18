//! CEF message-pump scheduling on AppKit. macOS has one `NSRunLoop` shared by winit and CEF, and
//! winit's `pump_app_events` only delivers events while it is running that loop (its handler is
//! uninstalled between pumps — anything AppKit dispatches outside the pump window is dropped). So
//! the shell must never run the loop itself between pumps: `do_message_loop_work` is driven by a
//! repeating `NSTimer` on the main run loop instead, which fires *inside* the pump window where
//! every event it provokes is delivered or queued, never dropped. The timer is scheduled in
//! `NSRunLoopCommonModes` so CEF keeps pumping during live-resize and menu tracking.

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use std::cell::RefCell;

thread_local! {
    /// The installed pump timer, kept so `uninstall` can invalidate it before CEF shuts down
    /// (`cef::shutdown` runs the loop to drain pending work — the timer must not fire into it).
    static TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
}

/// The pump cadence. Fast enough that CEF's browser-process tasks (input forwarding, IPC, paint
/// scheduling) never wait a perceptible interval; cheap when there is no work.
const PUMP_INTERVAL_SECS: f64 = 0.004;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SaffronCefPumpTick"]
    #[ivars = ()]
    pub struct CefPumpTick;

    impl CefPumpTick {
        #[unsafe(method(onTick:))]
        fn on_tick(&self, _timer: &NSTimer) {
            cef::do_message_loop_work();
        }
    }
);

impl CefPumpTick {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Install the repeating CEF pump timer on the main run loop. Called once after `initialize`,
/// on the main thread.
pub fn install() {
    let mtm = MainThreadMarker::new().expect("cef pump installs on the main thread");
    let tick = CefPumpTick::new(mtm);
    let timer = unsafe {
        let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            PUMP_INTERVAL_SECS,
            &tick,
            sel!(onTick:),
            None,
            true,
        );
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        timer
    };
    TIMER.with(|t| *t.borrow_mut() = Some(timer));
    std::mem::forget(tick);
}

/// Invalidate the pump timer. Called on the main thread before `cef::shutdown`, whose internal
/// loop-draining would otherwise fire `do_message_loop_work` re-entrantly mid-shutdown.
pub fn uninstall() {
    TIMER.with(|t| {
        if let Some(timer) = t.borrow_mut().take() {
            timer.invalidate();
        }
    });
}
