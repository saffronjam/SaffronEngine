+++
title = 'Ash and the Vulkan seam'
weight = 1
+++

# Ash and the Vulkan seam

[`ash`](https://docs.rs/ash/0.38.0+1.3.281/ash/) provides generated Vulkan types, handles, entry-point
loading, and thin calls into the C API. It does not track object ownership, command-buffer state, or
external synchronization. The rendering crate therefore treats every ash call as a boundary where
Anima must establish the Vulkan preconditions.

`saffron-rendering` enables `#![allow(unsafe_code)]` for this boundary. Other safe engine crates consume
its `Device`, `Renderer`, uploaded-resource, and render-graph APIs without calling Vulkan directly. The
unsafe blocks remain at the operations that need their proof, across resource, pass, upload, device,
swapchain, and frame code.

## What an unsafe block proves

Rust defines an `unsafe` block as an assertion that the caller has discharged the operation's extra
safety obligations in the [Rust Reference](https://doc.rust-lang.org/reference/unsafe-keyword.html).
For an ash call, the nearby `// SAFETY:` comment records the relevant facts rather than merely naming
the FFI boundary.

Typical facts include:

- handles belong to the device and remain alive for the call or recorded command;
- pointer-backed create-info slices remain valid for the duration of the call;
- a command buffer is recording and has the required resource states;
- queue, pool, and descriptor operations satisfy Vulkan's external-synchronization rules;
- destroy calls run once, after submitted work has finished.

Some helpers are themselves `unsafe fn` because the caller owns part of that proof.
`record_present_blit`, for example, requires a recording command buffer, live images, and source stage,
access, and layout values that describe the previous writer. Safe constructors such as
`PresentSync::new` keep the proof inside the rendering crate and clean up partially created handles on
failure.

## Fallible Vulkan calls

Ash represents a fallible Vulkan call as `Result<T, vk::Result>`. The rendering error enum preserves
that raw result together with a static operation label:

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("vulkan call '{context}' failed: {result:?}")]
    Vk { context: &'static str, result: vk::Result },
    // Loader, NoDevice, EmptyMesh, and other rendering failures
}
```

The `checked` helper applies the common conversion. A call site keeps the safety proof, operation name,
and propagation together:

```rust
let command_pool = checked(
    unsafe { raw.create_command_pool(&pool_info, None) },
    "present: create_command_pool",
)?;
```

This preserves the exact [Vulkan result code](https://docs.vulkan.org/spec/latest/chapters/fundamentals.html#fundamentals-errorcodes)
for diagnostics and control flow. Loader failures use `Error::Loader`, because `ash::Entry::load` returns
an ash loading error rather than `vk::Result`. Domain failures such as an empty mesh have their own
variants.

Not every Vulkan command is fallible. Recording calls such as `cmd_pipeline_barrier2`, descriptor
updates, and destroy calls return no result. Their correctness comes from the established invariants,
validation-layer checks, and ownership rules, not from `checked`.

## Results that drive control flow

Some Vulkan statuses describe a recoverable presentation condition. The renderer matches those values
before it converts remaining failures into `Error::Vk`:

```rust
let image_index = match acquire {
    Ok((index, _suboptimal)) => index,
    Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
    Err(result) => {
        return Err(Error::Vk {
            context: "acquire_next_image",
            result,
        });
    }
};
```

`Renderer::render_frame` returns `Ok(false)` when acquire reports an out-of-date swapchain, allowing the
application loop to skip that frame and rebuild on the resize path. Present accepts
`ERROR_OUT_OF_DATE_KHR` and `SUBOPTIMAL_KHR` as nonfatal statuses. Any other acquire or present error
retains its `vk::Result` in `Error::Vk`.

## Handles still need owners

Ash handles are copyable identifiers, not owning Rust values. Resource wrappers pair them with the
device or VMA allocation required for destruction and release them in `Drop`. Frame and presentation
rings that borrow device handles instead expose explicit `destroy` methods called after
`Device::wait_idle`.

The error seam and ownership seam complement each other. Typed errors preserve failed operations;
partial-construction paths release handles acquired before the failure; successful construction hands
the complete handle set to one owner.

## In the code

| What | File | Symbols |
|---|---|---|
| Unsafe opt-in and typed errors | `engine/crates/rendering/src/lib.rs` | `#![allow(unsafe_code)]`, `Error`, `Error::Vk`, `Result`, `checked` |
| Loader and device creation | `engine/crates/rendering/src/device.rs` | `load_entry`, `create_instance`, `create_logical_device` |
| Safe construction over unsafe calls | `engine/crates/rendering/src/present.rs` | `PresentSync::new`, `PresentSync::create_slot`, `record_present_blit` |
| Recoverable swapchain statuses | `engine/crates/rendering/src/renderer.rs` | `Renderer::begin_present_frame`, `Renderer::render_frame`, `Renderer::present_active_view_to_swapchain` |
| Handle ownership | `engine/crates/rendering/src/resources.rs` | `Buffer`, `Image`, `Pipeline`, `AccelerationStructure` |

## Related

- [Error handling](../../core-and-conventions/error-handling/): crate-local error types and propagation
- [Meta-layer resources](../meta-layer-resources/): ownership for ash and VMA handles
- [Frame sync and resize](../frame-sync-and-resize/): acquire, submit, present, and swapchain rebuilds
- [VMA allocator](../vma-allocator/): memory allocation across the same unsafe boundary
