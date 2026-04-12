# Server setup

The getprvw.com website runs on the Hetzner VPS (same server as getcmdr.com).

## Architecture

```
Cloudflare DNS (getprvw.com)  →  Hetzner VPS (37.27.245.171)
                                    ↓
                                  Caddy (reverse proxy, TLS)
                                    ↓
                              getprvw-static (nginx container, port 80)
```

Auto-deploy: push to `main` → CI passes → webhook → server pulls + rebuilds + restarts.

## Components

| Component | Location on server | Repo |
|---|---|---|
| Website container | `~/prvw/apps/website/` | vdavid/prvw |
| Deploy webhook | `~/prvw/infra/deploy-webhook/` | vdavid/prvw |
| Caddy config | `~/hetzner-server/caddy/Caddyfile` | vdavid/hetzner-server |
| Systemd service | `/etc/systemd/system/deploy-prvw-webhook.service` | (on server) |

## One-time setup (already done)

### 1. DNS (Cloudflare)

- A record: `getprvw.com` → `37.27.245.171` (proxied)
- CNAME: `www.getprvw.com` → `getprvw.com` (proxied)

### 2. Clone repo on server

```bash
ssh hetzner "git clone git@github.com:vdavid/prvw.git ~/prvw"
```

### 3. Build and start the website container

```bash
ssh hetzner "cd ~/prvw/apps/website && docker compose build && docker compose up -d"
```

### 4. Add Caddy routes

In `~/hetzner-server/caddy/Caddyfile`:

```
getprvw.com {
    handle /hooks/* {
        reverse_proxy host.docker.internal:9001
    }
    handle {
        reverse_proxy getprvw-static:80
    }
}

www.getprvw.com {
    redir https://getprvw.com{uri} permanent
}
```

### 5. Deploy webhook

Create the systemd service at `/etc/systemd/system/deploy-prvw-webhook.service`:

```ini
[Unit]
Description=Prvw Deploy Webhook Listener
After=network.target

[Service]
Type=simple
User=david
Group=david
Environment="DEPLOY_WEBHOOK_SECRET=<secret from GitHub>"
ExecStart=/usr/local/bin/webhook -hooks /home/david/prvw/infra/deploy-webhook/hooks.json -port 9001 -verbose -template
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Then enable:

```bash
sudo systemctl daemon-reload
sudo systemctl enable deploy-prvw-webhook
sudo systemctl start deploy-prvw-webhook
```

### 6. GitHub secrets

`DEPLOY_WEBHOOK_SECRET` must be set in GitHub repo secrets (same value as in the systemd service).

## Manual deploy

```bash
ssh hetzner "cd ~/prvw && git fetch origin main && git reset --hard origin/main && cd apps/website && docker compose build --no-cache && docker compose down && docker compose up -d"
```

## Email setup

### Done

(none yet)

### Gaps (manual steps needed)

1. **Cloudflare Email Routing**: enable in the Cloudflare dashboard for getprvw.com zone. Add catch-all rule to forward
   to your Gmail. The API token doesn't have email routing permissions.
2. **Transactional email sending**: if needed, add getprvw.com as a verified sender domain in Brevo (the tooling doc
   has the API key). Or use Cloudflare's built-in email sending.
