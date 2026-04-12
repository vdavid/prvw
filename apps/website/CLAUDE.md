# Website (getprvw.com)

Landing page for Prvw. Astro + Tailwind v4, statically built.

## Stack

- **Astro 5.7** - static site generator
- **Tailwind v4** - CSS-first config in `src/styles/global.css`
- Dev server port: **14829**

## Color scheme

The brand is **sky blue** (`#4da6ff`) with warm yellow sub-accents (`#ffc206`). Not Cmdr's mustard yellow.

### Dark mode (default, landing page)

| Token              | Value                       |
| ------------------ | --------------------------- |
| Background         | `#0f1419`                   |
| Surface            | `#151c24`                   |
| Text primary       | `#f0f4f8`                   |
| Text secondary     | `#8899aa`                   |
| Accent             | `#4da6ff`                   |
| Accent hover       | `#6bb8ff`                   |
| Accent glow        | `rgba(77, 166, 255, 0.35)`  |
| Accent contrast    | `#0f1419`                   |

### Light mode (sub-pages, system preference)

| Token              | Value                       |
| ------------------ | --------------------------- |
| Background         | `#f8fafb`                   |
| Surface            | `#eef2f5`                   |
| Text primary       | `#1a2433`                   |
| Text secondary     | `#5c6b7a`                   |
| Accent             | `#2b8ae6`                   |
| Accent contrast    | `#ffffff`                   |

## Patterns

- **Layouts**: `Layout.astro` (base with meta, OG, theme support)
- **CSS variables**: defined in `src/styles/global.css` under `@theme`. Use them everywhere.
- **Light/dark mode**: Dual-selector pattern (media query + `data-theme` attribute). Same approach as Cmdr's website.
  Landing page defaults to dark. Sub-pages will support both.
- **Font**: Self-hosted Inter variable (`public/fonts/inter-latin-variable.woff2`).

## File structure

| File / directory                   | Purpose                           |
| ---------------------------------- | --------------------------------- |
| `src/pages/index.astro`           | Landing page                      |
| `src/layouts/Layout.astro`        | Base layout (meta, OG, fonts)     |
| `src/components/Header.astro`     | Top nav with mobile menu          |
| `src/components/Hero.astro`       | Hero section with CTA             |
| `src/components/Features.astro`   | Feature cards grid                |
| `src/components/Pricing.astro`    | Personal vs commercial pricing    |
| `src/components/Footer.astro`     | Minimal footer with links         |
| `src/styles/global.css`           | Tailwind v4 theme + global styles |

## Analytics

- **Umami** (`Layout.astro`): Cookieless page analytics (pageviews, referrers, geo). Self-hosted at
  `anal.veszelovszki.com`. Script served at `/u/mami` (proxied through Caddy to avoid adblockers). The desktop app has
  **no telemetry**.

**Decision/Why**: We avoid cookies to not need a cookie consent banner. Umami is configured to work without cookies. If
you add analytics tooling, preserve this property. The tracking script URL and website ID are set via
`PUBLIC_UMAMI_HOST` and `PUBLIC_UMAMI_WEBSITE_ID` env vars (see `.env.example`).

## Gotchas

- The `@ts-expect-error` in `astro.config.mjs` is for a Vite version mismatch between Astro and Tailwind. Doesn't
  affect the build.
- `site` must be set in `astro.config.mjs` for OG image URLs to work.
- Don't hardcode colors. Use CSS variables from `global.css`.
- In light mode, accent buttons use white text (`--color-accent-contrast: #ffffff`), not the dark background color.
