//! Loader, instance, the validation layer and debug messenger, and the window surface.

use super::*;

/// Picks the platform instance extensions and creates the instance with
/// validation in debug builds.
///
/// The windowed host enables `VK_KHR_surface` + the platform surface extension; the
/// offscreen host enables **no** surface extension at all. A no-surface instance is
/// what lets the editor host boot under the NVIDIA ICD: that driver implements no
/// headless surface, so requesting one would fail `create_instance` with
/// `ERROR_EXTENSION_NOT_PRESENT`.
/// Loads the Vulkan loader (`libvulkan`) into an [`ash::Entry`].
///
/// Everywhere but macOS this is `Entry::load`, which finds the loader on the system library path.
/// macOS has no native Vulkan and no default search entry for Homebrew's `/opt/homebrew/lib`
/// (Apple Silicon) or `/usr/local/lib` (Intel); worse, macOS strips `DYLD_*` env vars across the
/// editor→host spawn (SIP), so a `DYLD_FALLBACK_LIBRARY_PATH` cannot be relied on to reach here.
/// On macOS an exported app's bundled MoltenVK library is loaded directly. Development runs try the
/// known Homebrew Vulkan loader paths so validation layers remain available, then fall back to the
/// default `Entry::load`.
pub(super) fn load_entry() -> Result<ash::Entry> {
    // SAFETY: the ash seam. `Entry::load*` dynamically loads `libvulkan`; the returned entry owns
    // the loader for the caller's use.
    #[cfg(target_os = "macos")]
    {
        let mut loader_paths = Vec::new();
        if let Some(executable_dir) = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        {
            let bundled_moltenvk = executable_dir
                .join("..")
                .join("Frameworks")
                .join("libMoltenVK.dylib");
            if bundled_moltenvk.is_file()
                && let Ok(entry) = unsafe { ash::Entry::load_from(&bundled_moltenvk) }
            {
                return Ok(entry);
            }
        }
        loader_paths.extend(
            [
                "/opt/homebrew/lib/libvulkan.dylib",
                "/opt/homebrew/lib/libvulkan.1.dylib",
                "/usr/local/lib/libvulkan.dylib",
                "/usr/local/lib/libvulkan.1.dylib",
            ]
            .into_iter()
            .map(std::path::PathBuf::from),
        );
        for path in loader_paths {
            if path.is_file()
                && let Ok(entry) = unsafe { ash::Entry::load_from(&path) }
            {
                return Ok(entry);
            }
        }
    }
    unsafe { ash::Entry::load() }.map_err(|err| Error::Loader(err.to_string()))
}

pub(super) fn create_instance(
    entry: &ash::Entry,
    surface_source: &SurfaceSource<'_>,
    validation: bool,
) -> Result<ash::Instance> {
    let app_name = c"Saffron Anima";
    let app_info = vk::ApplicationInfo::default()
        .application_name(app_name)
        .engine_name(app_name)
        .api_version(API_VERSION);

    let mut extensions: Vec<*const c_char> = Vec::new();
    match surface_source {
        SurfaceSource::Window(window) => {
            extensions.push(surface::NAME.as_ptr());
            let display = window
                .display_handle()
                .map_err(|err| Error::NoSurfaceHandle(err.to_string()))?;
            let required =
                ash_window::enumerate_required_extensions(display.as_raw()).map_err(|result| {
                    Error::Vk {
                        context: "enumerate_required_extensions",
                        result,
                    }
                })?;
            extensions.extend_from_slice(required);
        }
        SurfaceSource::Offscreen => {}
    }

    let mut layers: Vec<*const c_char> = Vec::new();
    if validation {
        extensions.push(debug_utils::NAME.as_ptr());
        layers.push(VALIDATION_LAYER.as_ptr());
    }

    // A portability driver (MoltenVK, the only Vulkan on macOS) is hidden from device
    // enumeration unless the instance opts in with `VK_KHR_portability_enumeration` plus the
    // matching create flag. Enable it whenever the loader advertises the extension; on a native
    // ICD the extension is absent and this is a no-op, so there is one code path for every host.
    let portability = instance_extension_available(entry, ash::khr::portability_enumeration::NAME);
    if portability {
        extensions.push(ash::khr::portability_enumeration::NAME.as_ptr());
    }
    let flags = if portability {
        vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR
    } else {
        vk::InstanceCreateFlags::empty()
    };

    let create_info = vk::InstanceCreateInfo::default()
        .flags(flags)
        .application_info(&app_info)
        .enabled_extension_names(&extensions)
        .enabled_layer_names(&layers);

    // SAFETY: the ash seam. The extension/layer name pointers are valid `CStr`s
    // borrowed for the duration of the call; the create-info struct outlives it.
    let instance =
        unsafe { entry.create_instance(&create_info, None) }.map_err(|result| Error::Vk {
            context: "create_instance",
            result,
        })?;
    Ok(instance)
}

/// The single validation layer the engine enables in debug.
pub(super) const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Whether to enable the Khronos validation layer this run. Debug builds enable it (or any
/// build when `SAFFRON_FORCE_VALIDATION` is set), unless `SAFFRON_DISABLE_VALIDATION` is set;
/// release builds run without it. Validation is a heavy per-command CPU cost, so it must not
/// ship on. A debug build that wants it but lacks the installed layer logs once and continues.
pub(super) fn validation_enabled(entry: &ash::Entry) -> bool {
    let wanted = (cfg!(debug_assertions) || std::env::var_os("SAFFRON_FORCE_VALIDATION").is_some())
        && std::env::var_os("SAFFRON_DISABLE_VALIDATION").is_none();
    if !wanted {
        return false;
    }
    if validation_layer_available(entry) {
        true
    } else {
        tracing::warn!("validation layer unavailable — running without it");
        false
    }
}

/// Reports whether the loader advertises the given instance extension.
pub(super) fn instance_extension_available(entry: &ash::Entry, name: &CStr) -> bool {
    // SAFETY: the ash seam. Enumerates instance extensions; no resource is created.
    let Ok(extensions) = (unsafe { entry.enumerate_instance_extension_properties(None) }) else {
        return false;
    };
    extensions.iter().any(|ext| {
        ext.extension_name_as_c_str()
            .map(|n| n == name)
            .unwrap_or(false)
    })
}

/// Reports whether the Khronos validation layer is installed.
pub(super) fn validation_layer_available(entry: &ash::Entry) -> bool {
    // SAFETY: the ash seam. Enumerates instance layers; no resource is created.
    let Ok(layers) = (unsafe { entry.enumerate_instance_layer_properties() }) else {
        return false;
    };
    layers.iter().any(|layer| {
        layer
            .layer_name_as_c_str()
            .map(|name| name == VALIDATION_LAYER)
            .unwrap_or(false)
    })
}

/// Creates the debug-utils messenger that routes validation messages into the
/// engine log. Returns `(None, None)` when the extension is absent.
pub(super) fn create_debug_messenger(
    entry: &ash::Entry,
    instance: &ash::Instance,
    validation: bool,
) -> Result<(
    Option<debug_utils::Instance>,
    Option<vk::DebugUtilsMessengerEXT>,
)> {
    // The messenger lives behind the validation layer (it provides `debug_utils`'s dispatch).
    // When validation is off, the extension was not enabled, so creating the messenger would
    // call a null entry point — gate on the same decision the instance used.
    if !validation {
        return Ok((None, None));
    }
    // SAFETY: the ash seam. Enumerates instance extensions; no resource created.
    let extensions =
        unsafe { entry.enumerate_instance_extension_properties(None) }.map_err(|result| {
            Error::Vk {
                context: "enumerate_instance_extension_properties",
                result,
            }
        })?;
    let present = extensions.iter().any(|ext| {
        ext.extension_name_as_c_str()
            .map(|name| name == debug_utils::NAME)
            .unwrap_or(false)
    });
    if !present {
        return Ok((None, None));
    }

    let loader = debug_utils::Instance::new(entry, instance);
    let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                | vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
        )
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(debug_callback));

    // SAFETY: the ash seam. The create-info (and its callback pointer) is valid
    // for the call; the returned messenger is owned and destroyed in `Device::drop`.
    let messenger =
        unsafe { loader.create_debug_utils_messenger(&info, None) }.map_err(|result| {
            Error::Vk {
                context: "create_debug_utils_messenger",
                result,
            }
        })?;
    Ok((Some(loader), Some(messenger)))
}

/// The validation-layer message sink. Forwards real validation/performance
/// messages to the engine log under the `vulkan` subsystem so the validation-clean
/// gate parses them, and drops loader chatter (general-type messages below error)
/// unless `SAFFRON_VK_VERBOSE` is set.
/// Always returns `VK_FALSE` (does not abort the triggering call).
unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user_data: *mut c_void,
) -> vk::Bool32 {
    // SAFETY: the validation layer guarantees `data` points at a valid callback
    // struct for the duration of this call; the message / id pointers are valid C
    // strings when non-null.
    let (message, id) = unsafe {
        let data = &*data;
        let message = read_c_str(data.p_message);
        let id = read_c_str(data.p_message_id_name);
        (message, id)
    };

    let verbose = std::env::var_os("SAFFRON_VK_VERBOSE").is_some();
    let loader_chatter = types == vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
        && !severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR);
    if !verbose && (loader_chatter || id.contains("OutputNotConsumed")) {
        return vk::FALSE;
    }

    let level = if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        tracing::Level::ERROR
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        tracing::Level::WARN
    } else {
        tracing::Level::INFO
    };
    let kind = if types.contains(vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION) {
        "validation"
    } else if types.contains(vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE) {
        "performance"
    } else {
        "general"
    };

    // A real validation or performance issue (not filtered loader chatter) at
    // warning-or-error severity fails the validation-clean gate.
    if kind != "general" && level != tracing::Level::INFO {
        VALIDATION_ISSUE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    // The messenger logs on another subsystem's behalf, so it sets the target
    // explicitly rather than inheriting this crate's module path.
    let body = if id.is_empty() {
        format!("[{kind}] {message}")
    } else {
        format!("[{kind}] {id}: {message}")
    };
    match level {
        tracing::Level::ERROR => tracing::error!(target: "vulkan", "{body}"),
        tracing::Level::WARN => tracing::warn!(target: "vulkan", "{body}"),
        _ => tracing::info!(target: "vulkan", "{body}"),
    }
    vk::FALSE
}

/// Reads a possibly-null Vulkan C string into an owned `String` (empty if null).
pub(super) fn read_c_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the validation layer guarantees a non-null pointer is a valid,
    // NUL-terminated C string for the duration of the callback.
    unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
}

/// Creates the windowed surface from a window's raw display+window handle pair, via
/// `ash-window`. The offscreen host creates no surface, so this is the only surface
/// path.
pub(super) fn create_window_surface(
    entry: &ash::Entry,
    instance: &ash::Instance,
    window: &dyn WindowSurface,
) -> Result<vk::SurfaceKHR> {
    let display = window
        .display_handle()
        .map_err(|err| Error::NoSurfaceHandle(err.to_string()))?;
    let handle = window
        .window_handle()
        .map_err(|err| Error::NoSurfaceHandle(err.to_string()))?;
    // SAFETY: the ash seam. The display/window handles are valid for the call (the
    // window outlives the device per the host's Drop order); the returned surface is
    // destroyed in `Device::drop`.
    unsafe { ash_window::create_surface(entry, instance, display.as_raw(), handle.as_raw(), None) }
        .map_err(|result| Error::Vk {
            context: "create_surface",
            result,
        })
}
