# Design principles

- **Speed above all.** The image must appear the moment the user opens the file. No loading screens, no spinners, no
  white flash. Preload adjacent images so navigation feels zero-latency. Every interaction (zoom, pan, navigate) must
  respond within a single frame.
- **Minimal chrome.** The image is 99% of the app. No sidebars, no toolbars, no floating panels. The viewer gets out of
  the way and lets you see your photos. Every UI element must earn its place.
- **Platform-native, not generic.** The app should look and feel as if it was made specifically for macOS. Use native
  menus via Tauri, respect system dark/light mode, follow macOS keyboard conventions (Cmd+Q, Cmd+W, Cmd+F for
  fullscreen). Cross-platform comes later, but never at the cost of native feel. When we go cross-platform, fork by OS
  (same approach as Cmdr).
- **Keyboard-first.** Everything must work with the mouse, too, but all features should be fast and intuitive from the
  keyboard. Display shortcuts in menus.
- **Respect the OS.** Honor the user's system settings: light/dark mode, `prefers-reduced-motion` for animations,
  accessibility features. Don't fight the system, work with it.
- **Respect resources.** Minimize CPU, memory, and GPU use. Use render-on-demand (not a continuous render loop). Don't
  keep the GPU busy when idle. Cap the preloader's memory budget.
- **Accessibility.** Features should be available to people with impaired vision, hearing, and cognitive disabilities.
  Keep contrast ratios high, support VoiceOver where possible, don't rely on color alone.
- **Elegant architecture over quick hacks.** We have time to do outstanding work and we're in this for the long run.
  Prefer clean, well-structured code that's easy to reason about.
