# Build stage
FROM node:22-alpine AS builder

WORKDIR /app

RUN corepack enable && corepack prepare pnpm@10.11.0 --activate

# Copy workspace config and lockfile
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY apps/website/package.json ./apps/website/

# Install dependencies for website only
RUN pnpm install --frozen-lockfile --filter "./apps/website"

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
