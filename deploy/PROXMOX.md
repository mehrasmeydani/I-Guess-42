# Running it on your own Proxmox server

The app is one container plus Caddy, which fetches the HTTPS certificate for
`iguesslow.com` by itself. On Proxmox that means one small Linux guest with
Docker, reachable from the internet on ports 80 and 443.

## 1. Create the guest

Either works:

- **VM (simplest)**: Debian 12 or Ubuntu 24.04, **2 vCPU, 2 GB RAM, 16 GB
  disk**. The RAM matters mostly for the first build, which compiles Rust.
- **LXC (lighter)**: a Debian 12 template, same sizes, with
  **Options → Features → nesting** turned on, or Docker will not start
  inside it. Leave it unprivileged.

Give it a **static IP** on your LAN (or a DHCP reservation on the router), for
example `192.168.1.50`.

## 2. Install Docker in the guest

```sh
apt update && apt install -y curl ca-certificates
curl -fsSL https://get.docker.com | sh
docker compose version        # should print a version
```

If you log in as a normal user rather than root, add it to the docker group:
`usermod -aG docker <user>`, then log out and back in.

From this machine, make sure SSH works without a password prompt:

```sh
ssh-copy-id root@192.168.1.50
```

## 3. Let the internet reach it

1. **Router: forward TCP 80 and 443** to the guest's IP. Let's Encrypt needs
   port 80 to issue the certificate, and players use 443.
2. **DNS: point `iguesslow.com` at your home's public IP** with an A record
   (and `www` too if you want it). Your public IP is what
   `curl https://api.ipify.org` prints from inside the guest.
3. If your ISP changes your public IP now and then, use your DNS provider's
   dynamic-DNS updater, or the deploy script's DNS check will start failing.

> **Behind CGNAT** (no public IPv4, port forwarding impossible)? Then a
> Cloudflare Tunnel is the way in instead of steps 1-2. It needs Caddy set to
> plain HTTP behind it; ask and it can be wired up.

## 4. The 42 application

On <https://profile.intra.42.fr/oauth/applications>, open the app and make sure
its redirect URIs include:

```
https://iguesslow.com/auth/callback
http://localhost:3000/auth/callback
```

The first is the live site, the second the local test instance (below).

## 5. Deploy

From this repository, with `.env` holding the live settings
(`SITE_DOMAIN=iguesslow.com`, the 42 credentials, `ADMIN_LOGINS` empty):

```sh
./deploy/proxmox.sh root@192.168.1.50     # first time
./deploy/proxmox.sh                       # every update after that
```

It checks Docker and DNS, copies the source over, builds on the guest, starts
the stack, and waits until `https://iguesslow.com/healthz` answers. The
database lives in a Docker volume, so redeploying never touches it.

## Testing before players see it

Run a test instance on your own machine instead of on the server:

```sh
ENV_FILE=.env.test cargo run
```

`.env.test` has your login in `ADMIN_LOGINS`, its own database
(`data/test.db`) and the localhost redirect URI. Open <http://localhost:3000>,
sign in, and use `/admin` for the clock, demo players and demo history.

## Backups

The whole game is one SQLite database in the `iguesslow_game_data` volume.
The simplest backup is a **Proxmox backup of the guest** (Datacenter → Backup
→ Add, daily).

For a file-level copy, stop the app for a second so the database is quiet,
copy the volume, and start it again. SQLite keeps recent writes in side files
(`game.db-wal`), so copy the whole directory, not only `game.db`:

```sh
ssh root@192.168.1.50 'cd iguesslow && docker compose stop app \
  && docker run --rm -v iguesslow_game_data:/d alpine tar czf - -C /d . ; \
  docker compose start app >/dev/null' > game-$(date +%F).tgz
```

Do it at a quiet moment, not around 12:42.
