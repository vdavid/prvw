//! Which GPU the viewer runs on, and which graphics API it talks to it through.
//!
//! Left to itself, wgpu takes every backend it was compiled with, pools the adapters they
//! expose, and sorts that pool by device type alone. Two consequences decide this module:
//!
//! - **The backend is a coin toss on Windows.** `wgpu-core` registers Vulkan before DX12
//!   (`instance.rs`, `Instance::new`) and the sort is stable, so for the same physical GPU the
//!   Vulkan adapter wins the tie. HDR output needs a DXGI swapchain colour space
//!   (`ExtendedSrgbLinear`, which is `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709`), and wgpu's
//!   Vulkan backend has no route to it. So the differentiator would have hung on which driver
//!   a machine happened to have installed.
//! - **"Least power" is not the same question as "the GPU driving the screen".** Prvw
//!   transforms pixels to the display profile on the CPU and then draws one textured quad on
//!   demand, so the adapter it renders on costs almost nothing either way. What does matter is
//!   that the swapchain lives on the adapter wired to the monitor the photographer calibrated:
//!   the HDR queries in M2 enumerate outputs per adapter.
//!
//! Both answers are per platform and neither shows up as a compile error, so they live here as
//! data plus pure functions, and every platform's answer compiles on every host. See
//! `docs/notes/gpu-adapter-selection.md` for the evidence behind each one.

use wgpu::{Backends, PowerPreference};

/// Every backend wgpu can really render with. Deliberately not `Backends::all()`, which also
/// carries `NOOP`, a stub backend that creates resources and draws nothing.
const REAL_BACKENDS: Backends = Backends::PRIMARY.union(Backends::SECONDARY);

/// One attempt at getting a GPU: the backends to build the instance with, and how to choose
/// among the adapters they expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterRequest {
    pub backends: Backends,
    pub power_preference: PowerPreference,
}

/// How one platform picks its GPU: the backend the viewer wants, and what keeps it on screen
/// when that backend isn't there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuPolicy {
    /// Tried first.
    pub preferred: AdapterRequest,
    /// Tried when nothing answers [`Self::preferred`]. Always wider than it, never narrower: a
    /// viewer showing nothing is worse than a viewer missing a feature.
    pub fallback: Option<AdapterRequest>,
    /// What the fallback costs, logged at `warn` when it's the one that answered. A QA report
    /// from a machine that took this path should say so in its own words.
    pub fallback_cost: &'static str,
}

impl GpuPolicy {
    // Each build constructs only its own platform's policy. All three compile everywhere so the
    // tests below can check them from whichever host runs, which is the point of keeping this
    // pure: two of the three platforms can't open a window in CI.
    /// macOS: Metal, and the low-power GPU.
    ///
    /// Metal is the only backend Apple ships, so naming it changes nothing today; it stops a
    /// build that ever turns on `vulkan-portability` from quietly moving to MoltenVK, where the
    /// EDR path doesn't exist. `LowPower` matters on the dual-GPU Intel Macs: macOS drives the
    /// display from the integrated GPU under automatic graphics switching, and waking the
    /// discrete one spins the fans for one quad per redraw.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn macos() -> Self {
        Self {
            preferred: AdapterRequest {
                backends: Backends::METAL,
                power_preference: PowerPreference::LowPower,
            },
            fallback: Some(AdapterRequest {
                backends: REAL_BACKENDS,
                power_preference: PowerPreference::LowPower,
            }),
            fallback_cost: "HDR output needs Metal, so it stays off on this machine.",
        }
    }

    /// Windows: DX12, and whichever adapter DXGI names first.
    ///
    /// DX12 is pinned because the HDR surface colour space is a DXGI feature. The power
    /// preference is deliberately `None`: it's the one value that leaves wgpu's adapter sort
    /// alone, so the order stands as `IDXGIFactory1::EnumAdapters1` returned it, and that call
    /// documents its first entry as the adapter driving the desktop's primary display.
    /// `LowPower` would take the integrated GPU on a desktop whose monitor hangs off a discrete
    /// card; `HighPerformance` would wake a laptop's discrete GPU for a viewer that renders a
    /// handful of frames a minute.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn windows() -> Self {
        Self {
            preferred: AdapterRequest {
                backends: Backends::DX12,
                power_preference: PowerPreference::None,
            },
            fallback: Some(AdapterRequest {
                backends: REAL_BACKENDS,
                power_preference: PowerPreference::None,
            }),
            fallback_cost: "HDR output needs DX12, so it stays off on this machine.",
        }
    }

    /// Linux: Vulkan, falling back to OpenGL where no Vulkan driver is installed.
    ///
    /// Same order wgpu would have taken on its own, said out loud so the fallback gets logged
    /// rather than inferred from a GPU name. There's no HDR path here to protect yet.
    #[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
    pub fn linux() -> Self {
        Self {
            preferred: AdapterRequest {
                backends: Backends::VULKAN,
                power_preference: PowerPreference::LowPower,
            },
            fallback: Some(AdapterRequest {
                backends: REAL_BACKENDS,
                power_preference: PowerPreference::LowPower,
            }),
            fallback_cost: "Rendering through OpenGL, which is slower than Vulkan.",
        }
    }

    /// The policy for the platform this binary runs on.
    pub fn for_host() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::macos()
        }
        #[cfg(target_os = "windows")]
        {
            Self::windows()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::linux()
        }
    }

    /// The requests to try, in order.
    pub fn attempts(&self) -> impl Iterator<Item = &AdapterRequest> {
        std::iter::once(&self.preferred).chain(self.fallback.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_policy() -> [GpuPolicy; 3] {
        [GpuPolicy::macos(), GpuPolicy::windows(), GpuPolicy::linux()]
    }

    #[test]
    fn windows_asks_for_dx12_first() {
        assert_eq!(GpuPolicy::windows().preferred.backends, Backends::DX12);
    }

    #[test]
    fn windows_leaves_the_adapter_order_to_dxgi() {
        // Anything but `None` makes wgpu re-sort by device type and throw away DXGI's
        // display-affinity ordering. See the module docs.
        assert_eq!(
            GpuPolicy::windows().preferred.power_preference,
            PowerPreference::None
        );
    }

    #[test]
    fn macos_asks_for_metal_and_the_low_power_gpu() {
        let policy = GpuPolicy::macos();
        assert_eq!(policy.preferred.backends, Backends::METAL);
        assert_eq!(policy.preferred.power_preference, PowerPreference::LowPower);
    }

    #[test]
    fn linux_asks_for_vulkan_and_can_still_reach_opengl() {
        let policy = GpuPolicy::linux();
        assert_eq!(policy.preferred.backends, Backends::VULKAN);
        assert!(
            policy
                .fallback
                .expect("Linux needs an OpenGL fallback")
                .backends
                .contains(Backends::GL)
        );
    }

    #[test]
    fn a_fallback_only_ever_widens_the_first_choice() {
        for policy in every_policy() {
            let fallback = policy.fallback.expect("every platform needs a fallback");
            assert!(
                fallback.backends.contains(policy.preferred.backends),
                "{:?} doesn't contain {:?}",
                fallback.backends,
                policy.preferred.backends
            );
            assert_ne!(fallback.backends, policy.preferred.backends);
        }
    }

    #[test]
    fn nothing_ever_asks_for_the_backend_that_draws_nothing() {
        for policy in every_policy() {
            for request in policy.attempts() {
                assert!(!request.backends.contains(Backends::NOOP));
            }
        }
    }

    #[test]
    fn every_fallback_says_what_it_costs() {
        for policy in every_policy() {
            assert!(policy.fallback_cost.ends_with('.'));
            assert!(policy.fallback_cost.len() > 20);
        }
    }

    #[test]
    fn the_host_gets_its_own_platforms_policy() {
        let expected = if cfg!(target_os = "macos") {
            GpuPolicy::macos()
        } else if cfg!(target_os = "windows") {
            GpuPolicy::windows()
        } else {
            GpuPolicy::linux()
        };
        assert_eq!(GpuPolicy::for_host(), expected);
    }
}
