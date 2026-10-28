//! The one Vulkan device: instance, physical device, logical device and queue, exported as the
//! [`VulkanDecodeDevice`] the shared `PyroWave` decoder builds on.
//!
//! The feature set mirrors pf-presenter's probe because pyrowave reconstructs its create-infos
//! from what this reports and picks kernels from those features. A device missing one is a
//! device the codec refuses.

use std::ffi::{CStr, CString};

use anyhow::{anyhow, Context, Result};
use ash::vk::{self, Handle as _};
use pf_client_video::video_vk::{QueueLock, VulkanDecodeDevice};

/// The Vulkan version the instance declares. Skia validates its function table against the
/// version it is handed: the device's own (1.4 on RADV and lavapipe) fails that check.
pub const INSTANCE_API_VERSION: u32 = vk::API_VERSION_1_3;

/// What the device presents to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// `VK_KHR_display`: the console's `VideoOut` through RADV's WSI.
    Display,
    /// Images this crate owns, read back for tests. A developer box without a window.
    Offscreen { width: u32, height: u32 },
}

/// The device every part of the title draws and decodes on. Dropping idles and destroys it.
pub struct GpuDevice {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub pdev: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub qf: u32,
    pub api_version: u32,
    pub name: String,
    /// The handoff the `PyroWave` decoder is built from; carries the shared queue lock.
    pub vkd: VulkanDecodeDevice,
    /// `VK_EXT_external_memory_host` is on: decoder memory imports without a copy.
    pub host_memory_import: bool,
    pub target: Target,
}

struct Probe {
    ok: bool,
    float16: bool,
    api_version: u32,
    vendor_id: u32,
    name: String,
}

fn probe(instance: &ash::Instance, pdev: vk::PhysicalDevice) -> Probe {
    // SAFETY: read-only query on a live physical device; the result is plain data.
    let props = unsafe { instance.get_physical_device_properties(pdev) };
    let name = props
        .device_name_as_c_str()
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut f2 = vk::PhysicalDeviceFeatures2::default()
        .push_next(&mut f12)
        .push_next(&mut f13);
    // SAFETY: read-only query; the chained locals outlive the call.
    unsafe { instance.get_physical_device_features2(pdev, &mut f2) };
    let int16 = f2.features.shader_int16;
    Probe {
        ok: props.api_version >= vk::API_VERSION_1_3
            && int16 == vk::TRUE
            && f12.storage_buffer8_bit_access == vk::TRUE
            && f12.timeline_semaphore == vk::TRUE
            && f13.subgroup_size_control == vk::TRUE
            && f13.compute_full_subgroups == vk::TRUE
            && f13.synchronization2 == vk::TRUE,
        float16: f12.shader_float16 == vk::TRUE,
        api_version: props.api_version,
        vendor_id: props.vendor_id,
        name,
    }
}

fn has_extension(instance: &ash::Instance, pdev: vk::PhysicalDevice, name: &CStr) -> bool {
    // SAFETY: read-only query on a live physical device.
    unsafe { instance.enumerate_device_extension_properties(pdev) }
        .unwrap_or_default()
        .iter()
        .any(|e| e.extension_name_as_c_str() == Ok(name))
}

impl GpuDevice {
    /// Brings the device up for `target`. `entry` is the statically linked driver on the
    /// console, the system loader elsewhere.
    pub fn new(entry: ash::Entry, target: Target) -> Result<Self> {
        let instance_exts: Vec<&CStr> = match target {
            Target::Display => vec![ash::khr::surface::NAME, ash::khr::display::NAME],
            Target::Offscreen { .. } => Vec::new(),
        };
        let app = vk::ApplicationInfo::default()
            .application_name(c"punktfunk-ps5")
            .api_version(INSTANCE_API_VERSION);
        let ptrs: Vec<*const std::ffi::c_char> = instance_exts.iter().map(|n| n.as_ptr()).collect();
        // SAFETY: the create-info and the names it borrows outlive the call; the instance is
        // owned by the returned value and destroyed in `Drop`.
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default()
                    .application_info(&app)
                    .enabled_extension_names(&ptrs),
                None,
            )
        }
        .map_err(|e| anyhow!("vkCreateInstance: {e}"))?;
        match Self::with_instance(entry, instance.clone(), &instance_exts, target) {
            Ok(dev) => Ok(dev),
            Err(e) => {
                // SAFETY: nothing was created from the instance that outlives this point.
                unsafe { instance.destroy_instance(None) };
                Err(e)
            }
        }
    }

    fn with_instance(
        entry: ash::Entry,
        instance: ash::Instance,
        instance_exts: &[&CStr],
        target: Target,
    ) -> Result<Self> {
        // SAFETY: the instance is live; the enumeration only reads.
        let devices = unsafe { instance.enumerate_physical_devices() }.context("enumerate GPUs")?;
        let (pdev, qf, p) = devices
            .into_iter()
            .find_map(|pdev| {
                let p = probe(&instance, pdev);
                tracing::info!(
                    "gpu: {} Vulkan {}.{} pyrowave-capable={}",
                    p.name,
                    vk::api_version_major(p.api_version),
                    vk::api_version_minor(p.api_version),
                    p.ok
                );
                // SAFETY: read-only query on a live physical device.
                let families = unsafe { instance.get_physical_device_queue_family_properties(pdev) };
                let qf = families.iter().position(|f| {
                    f.queue_flags
                        .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
                })?;
                (p.api_version >= vk::API_VERSION_1_3).then_some((pdev, qf as u32, p))
            })
            .ok_or_else(|| anyhow!("no Vulkan 1.3 device with a graphics+compute queue"))?;

        let host_memory_import = has_extension(&instance, pdev, ash::ext::external_memory_host::NAME);
        let mut device_exts: Vec<&CStr> = Vec::new();
        if target == Target::Display {
            device_exts.push(ash::khr::swapchain::NAME);
        }
        if host_memory_import {
            device_exts.push(ash::ext::external_memory_host::NAME);
        }
        let ext_ptrs: Vec<*const std::ffi::c_char> = device_exts.iter().map(|n| n.as_ptr()).collect();
        let prio = [1.0f32];
        let queue_ci = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(qf)
            .queue_priorities(&prio)];
        // Exactly the probe's conjuncts when it passed; float16 only where the driver has it,
        // since pyrowave picks a half-precision path off this report.
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default()
            .timeline_semaphore(true)
            .storage_buffer8_bit_access(p.ok)
            .shader_float16(p.ok && p.float16);
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
            .synchronization2(true)
            .subgroup_size_control(p.ok)
            .compute_full_subgroups(p.ok);
        let mut f2 = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut f12)
            .push_next(&mut f13);
        f2.features.shader_int16 = if p.ok { vk::TRUE } else { vk::FALSE };
        // SAFETY: every builder is a local outliving the call; the device is owned by the
        // returned value and destroyed in `Drop`.
        let device = unsafe {
            instance.create_device(
                pdev,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queue_ci)
                    .enabled_extension_names(&ext_ptrs)
                    .push_next(&mut f2),
                None,
            )
        }
        .map_err(|e| anyhow!("vkCreateDevice: {e}"))?;
        // SAFETY: queue 0 of `qf` was requested at create.
        let queue = unsafe { device.get_device_queue(qf, 0) };
        tracing::info!(
            "gpu: device up on {} (host-memory import {}, pyrowave {})",
            p.name,
            host_memory_import,
            p.ok
        );
        let vkd = VulkanDecodeDevice {
            get_instance_proc_addr: entry.static_fn().get_instance_proc_addr as usize,
            instance: instance.handle().as_raw() as usize,
            physical_device: pdev.as_raw() as usize,
            device: device.handle().as_raw() as usize,
            vendor_id: p.vendor_id,
            device_name: p.name.clone(),
            graphics_qf: qf,
            // RADV here is built without Vulkan Video; decode mirrors graphics so the struct
            // stays well-formed for the fields pyrowave reads.
            decode_qf: qf,
            decode_video_caps: 0,
            instance_extensions: instance_exts.iter().map(|n| CString::from(*n)).collect(),
            device_extensions: device_exts.iter().map(|n| CString::from(*n)).collect(),
            f_sampler_ycbcr: false,
            f_timeline_semaphore: true,
            f_synchronization2: true,
            video_decode: false,
            pyrowave_decode: p.ok,
            f_shader_int16: p.ok,
            f_storage_buffer8: p.ok,
            f_subgroup_size_control: p.ok,
            f_compute_full_subgroups: p.ok,
            f_shader_float16: p.ok && p.float16,
            api_version: p.api_version,
            queue_families: vec![qf],
            d3d11_import: false,
            dmabuf_import: false,
            vaapi_av1_decode: false,
            vaapi_hevc_decode: false,
            d3d11_hdr10: false,
            d3d11_nv12: false,
            d3d11_p010: false,
            d3d11_present: false,
            d3d11_fence: false,
            adapter_luid: None,
            queue_lock: std::sync::Arc::new(QueueLock::new()),
        };
        Ok(Self {
            entry,
            instance,
            pdev,
            device,
            queue,
            qf,
            api_version: p.api_version,
            name: p.name,
            vkd,
            host_memory_import,
            target,
        })
    }

    /// The `PyroWave` feature line for the probe and the first bundle.
    pub fn feature_line(&self) -> String {
        format!(
            "vulkan {}.{} {} pyrowave={} int16={} sb8={} subgroup_ctl={} full_subgroups={} f16={} host_mem={}",
            vk::api_version_major(self.api_version),
            vk::api_version_minor(self.api_version),
            self.name,
            self.vkd.pyrowave_decode,
            self.vkd.f_shader_int16,
            self.vkd.f_storage_buffer8,
            self.vkd.f_subgroup_size_control,
            self.vkd.f_compute_full_subgroups,
            self.vkd.f_shader_float16,
            self.host_memory_import,
        )
    }

    /// A memory type index with `props` among `bits`.
    pub fn memory_type(&self, bits: u32, props: vk::MemoryPropertyFlags) -> Option<u32> {
        // SAFETY: read-only query on a live physical device.
        let mem = unsafe { self.instance.get_physical_device_memory_properties(self.pdev) };
        (0..mem.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && mem.memory_types[i as usize].property_flags.contains(props))
    }
}

impl Drop for GpuDevice {
    fn drop(&mut self) {
        let _q = self.vkd.queue_lock.guard();
        // SAFETY: every object built on this device belongs to a value that borrows or clones
        // it and is dropped first (the presenter, the decoders); idling retires queued work.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
