//! Image barriers (synchronization2): what the GPU must finish with an
//! image before the next use, and in which layout it is then.

use ash::vk;

use crate::image::COLOR;

/// A pipeline stage with the accesses in it.
pub type Access = (vk::PipelineStageFlags2, vk::AccessFlags2);

/// An image barrier, with an ownership transfer where the families differ.
pub fn barrier(
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src: Access,
    dst: Access,
    families: (u32, u32),
) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .image(image)
        .old_layout(old)
        .new_layout(new)
        .src_stage_mask(src.0)
        .src_access_mask(src.1)
        .dst_stage_mask(dst.0)
        .dst_access_mask(dst.1)
        .src_queue_family_index(families.0)
        .dst_queue_family_index(families.1)
        .subresource_range(COLOR)
}

pub fn barriers(
    d: &ash::Device,
    commands: vk::CommandBuffer,
    list: &[vk::ImageMemoryBarrier2<'_>],
) {
    if list.is_empty() {
        return;
    }
    // SAFETY: recording into a command buffer of this device.
    unsafe {
        d.cmd_pipeline_barrier2(
            commands,
            &vk::DependencyInfo::default().image_memory_barriers(list),
        )
    };
}

/// No ownership transfer.
pub const SAME: (u32, u32) = (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED);

pub const NONE: Access = (vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE);
pub const TRANSFER_WRITE: Access = (
    vk::PipelineStageFlags2::COPY,
    vk::AccessFlags2::TRANSFER_WRITE,
);
pub const TRANSFER_READ: Access = (
    vk::PipelineStageFlags2::COPY,
    vk::AccessFlags2::TRANSFER_READ,
);
pub const SHADER_READ: Access = (
    vk::PipelineStageFlags2::FRAGMENT_SHADER,
    vk::AccessFlags2::SHADER_SAMPLED_READ,
);
pub const COLOR_WRITE: Access = (
    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
    vk::AccessFlags2::from_raw(
        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE.as_raw()
            | vk::AccessFlags2::COLOR_ATTACHMENT_READ.as_raw(),
    ),
);
