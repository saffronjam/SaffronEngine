//! GPU device-loss diagnostics: `VK_NV_device_diagnostic_checkpoints` + `VK_EXT_device_fault`.
//!
//! Every render-graph pass and one-off upload submission drops a named checkpoint into its
//! command stream. When the device is lost, the queue reports the last checkpoint each pipeline
//! stage reached — naming the submission that wedged the GPU — and the fault query adds the
//! driver's fault kind and faulting GPU addresses, instead of a bare `ERROR_DEVICE_LOST`.

use std::ffi::c_void;
use std::sync::Mutex;

use ash::vk;

/// The `VK_NV_device_diagnostic_checkpoints` dispatch plus the marker-name registry.
///
/// A checkpoint marker is an opaque pointer-sized value the driver hands back verbatim. The
/// registry interns each name once and encodes its 1-based index as the marker, so the post-loss
/// query maps markers back to names without keeping raw string pointers alive across frames.
pub struct Checkpoints {
    dispatch: ash::nv::device_diagnostic_checkpoints::Device,
    names: Mutex<Vec<String>>,
}

impl Checkpoints {
    /// Resolves the dispatch. Call only when the extension was enabled on `device`.
    pub(crate) fn new(instance: &ash::Instance, device: &ash::Device) -> Self {
        Self {
            dispatch: ash::nv::device_diagnostic_checkpoints::Device::new(instance, device),
            names: Mutex::new(Vec::new()),
        }
    }

    /// Records a named checkpoint on `cmd`.
    pub(crate) fn mark(&self, cmd: vk::CommandBuffer, name: &str) {
        let index = {
            let mut names = self.names.lock().expect("checkpoint name registry");
            match names.iter().position(|known| known == name) {
                Some(index) => index,
                None => {
                    names.push(name.to_owned());
                    names.len() - 1
                }
            }
        };
        let marker = (index + 1) as *const c_void;
        // SAFETY: the ash seam. `cmd` is recording; the marker is an opaque value the driver
        // returns verbatim from the post-loss query.
        unsafe { (self.dispatch.fp().cmd_set_checkpoint_nv)(cmd, marker) };
    }

    /// Queries `queue`'s last-reached checkpoints and resolves them to `name reached stage`
    /// lines. Valid only once the device is lost. The caller holds the queue's external-sync
    /// lock.
    pub(crate) fn last_reached(&self, queue: vk::Queue) -> Vec<String> {
        let fp = self.dispatch.fp().get_queue_checkpoint_data_nv;
        let mut count = 0_u32;
        // SAFETY: the ash seam. Count-query form; the device is lost, so the data is final.
        unsafe { fp(queue, &mut count, std::ptr::null_mut()) };
        let mut data = vec![vk::CheckpointDataNV::default(); count as usize];
        // SAFETY: the ash seam. `data` holds `count` records with their `sType` set.
        unsafe { fp(queue, &mut count, data.as_mut_ptr()) };
        data.truncate(count as usize);
        let names = self.names.lock().expect("checkpoint name registry");
        data.iter()
            .map(|entry| {
                let name = (entry.p_checkpoint_marker as usize)
                    .checked_sub(1)
                    .and_then(|index| names.get(index))
                    .map_or("<unknown marker>", String::as_str);
                format!("'{name}' reached {:?}", entry.stage)
            })
            .collect()
    }
}

/// The `VK_EXT_device_fault` dispatch: after a device loss the driver reports what faulted —
/// the fault kind and, when it knows them, the faulting GPU virtual addresses.
pub struct DeviceFault {
    dispatch: ash::ext::device_fault::Device,
}

impl DeviceFault {
    /// Resolves the dispatch. Call only when the extension + feature were enabled on `device`.
    pub(crate) fn new(instance: &ash::Instance, device: &ash::Device) -> Self {
        Self {
            dispatch: ash::ext::device_fault::Device::new(instance, device),
        }
    }

    /// Queries the driver's fault report and resolves it to log lines. Valid only once the
    /// device is lost.
    pub(crate) fn report(&self) -> Vec<String> {
        let fp = self.dispatch.fp().get_device_fault_info_ext;
        let device = self.dispatch.device();
        let mut counts = vk::DeviceFaultCountsEXT::default();
        // SAFETY: the ash seam. Count-query form; the device is lost, so the report is final.
        let counted = unsafe { fp(device, &mut counts, std::ptr::null_mut()) };
        if counted != vk::Result::SUCCESS {
            return vec![format!("device fault query failed: {counted:?}")];
        }
        let mut address_infos =
            vec![vk::DeviceFaultAddressInfoEXT::default(); counts.address_info_count as usize];
        let mut vendor_infos =
            vec![vk::DeviceFaultVendorInfoEXT::default(); counts.vendor_info_count as usize];
        // The vendor binary is a driver dump for vendor tooling, not for the log.
        counts.vendor_binary_size = 0;
        let mut info = vk::DeviceFaultInfoEXT::default();
        if let Some(first) = address_infos.first_mut() {
            info = info.address_infos(first);
        }
        if let Some(first) = vendor_infos.first_mut() {
            info = info.vendor_infos(first);
        }
        // SAFETY: the ash seam. The arrays hold the counts the first call reported.
        let filled = unsafe { fp(device, &mut counts, &mut info) };
        if filled != vk::Result::SUCCESS {
            return vec![format!("device fault query failed: {filled:?}")];
        }
        let description = info
            .description_as_c_str()
            .ok()
            .and_then(|text| text.to_str().ok())
            .unwrap_or("<no description>")
            .to_owned();
        address_infos.truncate(counts.address_info_count as usize);
        vendor_infos.truncate(counts.vendor_info_count as usize);
        let mut lines = vec![format!("device fault: {description}")];
        lines.extend(address_infos.iter().map(|entry| {
            // The reported address is precise only to `address_precision` bytes.
            format!(
                "device fault address: {:?} at {:#x} (precision {:#x})",
                entry.address_type, entry.reported_address, entry.address_precision
            )
        }));
        lines.extend(vendor_infos.iter().map(|entry| {
            let description = entry
                .description_as_c_str()
                .ok()
                .and_then(|text| text.to_str().ok())
                .unwrap_or("<no description>");
            format!(
                "device fault vendor info: {description} (code {:#x}, data {:#x})",
                entry.vendor_fault_code, entry.vendor_fault_data
            )
        }));
        lines
    }
}
