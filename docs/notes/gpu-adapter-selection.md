# Which GPU Prvw renders on, and which API it uses

Background for `src/render/gpu.rs`. Everything here was read out of the resolved `wgpu` 29.0.4 sources in the Cargo
registry, so it's what this build does, not what the docs promise.

## What wgpu does when you don't tell it

`Instance::request_adapter` (`wgpu-core-29.0.4/src/instance.rs:481`) pools the adapters from every enabled backend, then
sorts that pool:

- The backends are registered in a fixed order in `Instance::new` (`:118-127`): Vulkan, Metal, DX12, OpenGL, noop.
- The sort key is device type alone (`get_order`, `:565`): discrete, integrated, other, virtual, CPU, reordered by
  `PowerPreference`.
- `sort_by_key` is stable, so **for one physical GPU the backend registered first wins the tie**.
- `PowerPreference::None` skips the sort entirely (`:558`), leaving the backend's own enumeration order standing.

So a `Backends::all()` instance on Windows hands back the **Vulkan** adapter for the same GPU that DX12 also exposes.
That's the shipping behaviour of every wgpu app that doesn't pin a backend, and it was Prvw's until this change.

## Why the backend has to be DX12 on Windows

HDR output is the differentiator (`docs/specs/cross-platform-plan.md`, M2), and on Windows it is
`IDXGISwapChain3::SetColorSpace1` with `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709` (scRGB, which wgpu 30 names
`SurfaceColorSpace::ExtendedSrgbLinear`). It's a DXGI call. wgpu's Vulkan backend has no route to it, so on the coin
toss above, the flagship feature would have been present or absent depending on whether a machine had a Vulkan ICD
installed.

Pinning `Backends::DX12` makes that ours. The fallback to `PRIMARY | SECONDARY` exists because a viewer that shows
nothing is worse than a viewer without HDR: a machine with no D3D12 device (a VM with a software GL stack, an ancient
driver) still gets a window, and the `warn` line says what it gave up.

`Backends::all()` is deliberately not the fallback. It carries `Backends::NOOP`, a stub backend that creates resources
and renders nothing (`wgpu-types-29.0.4/src/backend.rs:20-40`). It needs a second opt-in through `NoopBackendOptions`
today, but naming the real backends costs nothing and can't be un-opted-in by a future default.

## Why the power preference differs per platform

The honest framing is that this app barely uses the GPU. Prvw does its colour transform on the CPU in `color/`, then
draws one textured quad, render-on-demand. The adapter it picks costs almost nothing in either direction, so "respect
resources" (principle 2) is decided by which GPU gets **woken**, and correctness is decided by which GPU is **wired to
the monitor**.

- **macOS: `LowPower`.** Apple Silicon has one GPU, so it's inert there. It earns its place on the dual-GPU Intel Macs,
  where automatic graphics switching drives the display from the integrated GPU and asking for the discrete one spins
  the fans for a still image.
- **Windows: `None`.** This is the only value that leaves wgpu's sort alone, and the DX12 backend enumerates through
  `IDXGIFactory1::EnumAdapters1` (`wgpu-hal-29.0.4/src/auxil/dxgi/factory.rs:83`), whose documented first entry is the
  adapter with the output the desktop's primary display is on. That's the display-affinity answer, and it's the one that
  matters for M2: `IDXGIOutput6` HDR metadata is enumerated per adapter, so rendering on a GPU that doesn't drive the
  calibrated monitor is how you end up unable to see its headroom.
  - `LowPower` would take the integrated GPU on a desktop whose monitor hangs off a discrete card, which is a wide-gamut
    photographer's normal setup.
  - `HighPerformance` would wake a laptop's discrete GPU at every launch for a viewer that renders a few frames a
    minute, and on a laptop whose panel is wired to the integrated GPU it _adds_ a cross-adapter copy rather than
    removing one.
- **Linux: `LowPower`.** Same reasoning as macOS, and there's no HDR path to protect yet. PRIME offload is opt-in
  through the user's own environment variables, so an app-side preference wouldn't be the deciding vote anyway.

## What still needs a real Windows box

- That a DX12 adapter is found at all on the hardware we care about (near-certain; every Windows 10 22H2 machine with a
  WDDM 2.0 driver has one).
- That `EnumAdapters1`'s first entry really is the display-driving adapter on a hybrid laptop and on a desktop with both
  an iGPU and a dGPU. The `info` log line added alongside this names the adapter, its device type, the backend, and the
  driver, so one QA run answers it.
- Whether the swapchain ends up cross-adapter anywhere, which would show up as a present-time cost rather than a wrong
  picture.
