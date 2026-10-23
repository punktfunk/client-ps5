//! The render passes and the three pipelines a frame uses: planar CSC (`PyroWave`), two-plane
//! CSC (Videodec2's NV12/P010) and the UI layer's premultiplied blit. The shaders are the
//! desktop presenter's committed SPIR-V (`pf_client_video::video_csc_spv`), so the colour
//! maths lives in one place.

use anyhow::{anyhow, Result};
use ash::vk;
use pf_client_video::video_csc_spv as spv;

use crate::device::GpuDevice;

/// Three CSC rows plus the params vec4.
pub const PUSH_BYTES: u32 = 16 * 4;

pub struct Pipe {
    pub set_layout: vk::DescriptorSetLayout,
    pub layout: vk::PipelineLayout,
    pub pipeline: vk::Pipeline,
}

pub struct Pipelines {
    /// Final layout for scan-out (or the offscreen target's own).
    pub pass_out: vk::RenderPass,
    /// Final layout `TRANSFER_SRC`, for read-back. Compatible with `pass_out`.
    pub pass_read: vk::RenderPass,
    pub sampler: vk::Sampler,
    pub planar: Pipe,
    pub two_plane: Pipe,
    pub overlay: Pipe,
}

impl Pipelines {
    /// On an HDR10 `format` (`hdr`) the UI layer's sRGB is re-encoded as PQ at 203-nit white.
    pub fn new(dev: &GpuDevice, format: vk::Format, hdr: bool, out_layout: vk::ImageLayout) -> Result<Self> {
        let sampler = crate::util::sampler(dev)?;
        let pass_out = render_pass(dev, format, out_layout)?;
        let pass_read = render_pass(dev, format, vk::ImageLayout::TRANSFER_SRC_OPTIMAL)?;
        let overlay = if hdr { spv::OVERLAY_PQ_FRAG } else { spv::OVERLAY_FRAG };
        Ok(Self {
            planar: pipe(dev, pass_out, sampler, 3, true, spv::PLANAR_CSC_FRAG, false)?,
            two_plane: pipe(dev, pass_out, sampler, 2, true, spv::NV12_CSC_FRAG, false)?,
            overlay: pipe(dev, pass_out, sampler, 1, false, overlay, true)?,
            pass_out,
            pass_read,
            sampler,
        })
    }

    /// # Safety
    /// The device is idle and nothing recorded uses these objects.
    pub unsafe fn destroy(&self, dev: &GpuDevice) {
        // SAFETY: the caller guarantees the device is idle; every handle is owned here.
        unsafe {
            for p in [&self.planar, &self.two_plane, &self.overlay] {
                dev.device.destroy_pipeline(p.pipeline, None);
                dev.device.destroy_pipeline_layout(p.layout, None);
                dev.device.destroy_descriptor_set_layout(p.set_layout, None);
            }
            dev.device.destroy_render_pass(self.pass_out, None);
            dev.device.destroy_render_pass(self.pass_read, None);
            dev.device.destroy_sampler(self.sampler, None);
        }
    }
}

/// One subpass: clear (the letterbox bars and the black behind the menu), draw, leave the image
/// in `final_layout`.
fn render_pass(dev: &GpuDevice, format: vk::Format, final_layout: vk::ImageLayout) -> Result<vk::RenderPass> {
    let attachment = [vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(final_layout)];
    let color_ref = [vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let subpass = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_ref)];
    // Pairs with the acquire semaphore's COLOR_ATTACHMENT_OUTPUT wait, and orders the decode
    // lane's and the UI upload's writes before the fragment reads.
    let dependency = [vk::SubpassDependency::default()
        .src_subpass(vk::SUBPASS_EXTERNAL)
        .dst_subpass(0)
        .src_stage_mask(
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                | vk::PipelineStageFlags::COMPUTE_SHADER
                | vk::PipelineStageFlags::TRANSFER,
        )
        .src_access_mask(vk::AccessFlags::SHADER_WRITE | vk::AccessFlags::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT | vk::PipelineStageFlags::FRAGMENT_SHADER)
        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::SHADER_READ)];
    // SAFETY: the builders are locals that outlive the call.
    Ok(unsafe {
        dev.device.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(&attachment)
                .subpasses(&subpass)
                .dependencies(&dependency),
            None,
        )
    }?)
}

/// A bufferless fullscreen-triangle pipeline sampling `planes` images through immutable samplers.
fn pipe(
    dev: &GpuDevice,
    pass: vk::RenderPass,
    sampler: vk::Sampler,
    planes: u32,
    push: bool,
    frag: &[u8],
    premultiplied_blend: bool,
) -> Result<Pipe> {
    let immutable = [sampler];
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..planes)
        .map(|i| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(i)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .immutable_samplers(&immutable)
        })
        .collect();
    // SAFETY: builders are locals outliving the calls; the objects go to the returned `Pipe`.
    let set_layout = unsafe {
        dev.device
            .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)
    }?;
    let ranges = [vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        .size(PUSH_BYTES)];
    let set_layouts = [set_layout];
    let mut layout_ci = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
    if push {
        layout_ci = layout_ci.push_constant_ranges(&ranges);
    }
    // SAFETY: as above.
    let layout = unsafe { dev.device.create_pipeline_layout(&layout_ci, None) }?;

    let vert_words = ash::util::read_spv(&mut std::io::Cursor::new(spv::FULLSCREEN_VERT))?;
    let frag_words = ash::util::read_spv(&mut std::io::Cursor::new(frag))?;
    // SAFETY: the word slices outlive the calls; the modules are destroyed below.
    let (vert, frag) = unsafe {
        (
            dev.device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vert_words), None)?,
            dev.device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&frag_words), None)?,
        )
    };
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert)
            .name(c"main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag)
            .name(c"main"),
    ];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample =
        vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let mut blend_attachment =
        vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA);
    if premultiplied_blend {
        // Skia surfaces are premultiplied: src ONE, dst ONE_MINUS_SRC_ALPHA.
        blend_attachment = blend_attachment
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD);
    }
    let attachments = [blend_attachment];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let info = [vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .render_pass(pass)];
    // SAFETY: every borrowed builder is a local that outlives the call.
    let pipeline = unsafe {
        dev.device
            .create_graphics_pipelines(vk::PipelineCache::null(), &info, None)
    }
    .map_err(|(_, e)| anyhow!("create_graphics_pipelines: {e}"))?[0];
    // SAFETY: pipeline creation consumed the modules.
    unsafe {
        dev.device.destroy_shader_module(vert, None);
        dev.device.destroy_shader_module(frag, None);
    }
    Ok(Pipe {
        set_layout,
        layout,
        pipeline,
    })
}
