# Build stage
FROM node:24-alpine AS builder

WORKDIR /app

# Match the `packageManager` field in the root package.json. Corepack honors
# that field over whatever we prepare here, so keep them aligned to avoid
# surprises.
RUN corepack enable && corepack prepare pnpm@11.22.0 --activate

# Copy workspace config and lockfile
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY apps/website/package.json ./apps/website/

# Install dependencies for website only.
# `--ignore-scripts`: pnpm 11 turns "ignored build scripts" into a hard install
# error (pnpm 10 only warned), which broke the deploy when the repo moved to
# pnpm 11. esbuild and sharp ship prebuilt binaries and don't need their
# postinstall scripts for the Astro build (verified: build output is identical),
# so we skip all install scripts rather than maintain a build-allowlist.
RUN pnpm install --frozen-lockfile --filter "./apps/website" --ignore-scripts

# Copy website source
COPY apps/website/ ./apps/website/

# Build static site
WORKDIR /app/apps/website
RUN pnpm run build

# Production stage
FROM nginx:alpine AS production

COPY --from=builder /app/apps/website/dist /usr/share/nginx/html
COPY apps/website/nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 80

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://127.0.0.1/ || exit 1

CMD ["nginx", "-g", "daemon off;"]
