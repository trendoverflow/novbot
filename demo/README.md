# NovBot demo

## Supported path

The MVP demo is the OrbStack 3-VM setup:

| VM | Role |
|---|---|
| `novbot-ce-arm` | Center (MySQL, `novbot-center`, nginx, console) and one node |
| `novbot-node-2` | Node |
| `novbot-node-3` | Node |

From the Mac, the only supported entry is `http://127.0.0.1/`. OrbStack forwards the center VM's nginx port 80 to that loopback address.

Read-only runbook (lives outside this repo): `novbot-enterprise/docs/NOVBOT-DEMO-ACCESS-CARD.md`.

## Network binding (SEC-A)

[`nginx/novbot-console.conf`](nginx/novbot-console.conf) listens on `127.0.0.1:80` only. It serves the static console from `/var/www/novbot-console` and proxies `/v1/` and `/health` to `http://127.0.0.1:18080` on the center VM.

LAN opt-in is off by default. The documentation-only flag is `NOVBOT_DEMO_LAN_BIND=1`. Nginx does not read environment variables. Record that flag only after Product marks "LAN demo allowed" and Fei records a written waiver, then change the `listen` line by hand to `listen 80;` or `listen 0.0.0.0:80;`. Until then leave `listen 127.0.0.1:80;`.

Do not expose center HTTP `:18080` or MySQL `:3306` to the LAN. Those stay on VM loopback. Clients use nginx at `http://127.0.0.1/`.

### Install the packaged site on the center VM

Copy the file onto `novbot-ce-arm`, enable the site, and reload nginx. This repository does not deploy it.

```bash
sudo cp novbot-console.conf /etc/nginx/sites-available/novbot-console
sudo ln -sfn /etc/nginx/sites-available/novbot-console /etc/nginx/sites-enabled/novbot-console
sudo nginx -t && sudo systemctl reload nginx
```

## Seed

[`seed-orbstack.sh`](seed-orbstack.sh) PUTs config for a node that has already Registered. OrbStack systemd nodes do this on start. `PUT /v1/nodes/:id/config` for an id that has never registered returns 404 and does not insert a `nodes` row. Seed file: [`seed-orb-arm-1.json`](seed-orb-arm-1.json).

## Supersedes PR #7

Open PR #7 ([feat: secure local demo pack](https://github.com/trendoverflow/novbot/pull/7)) proposed a local compose+smoke pack.

The current supported demo path for MVP seal is the OrbStack 3-VM demo (`demo/seed-orbstack.sh`, `demo/seed-orb-arm-1.json`, packaged `demo/nginx/novbot-console.conf`, and the enterprise `NOVBOT-DEMO-ACCESS-CARD.md` runbook).

PR #7 may remain open for a future local-compose path. It is not required for MVP seal once this supersede note is on main.
