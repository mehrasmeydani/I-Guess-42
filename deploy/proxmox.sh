#!/usr/bin/env bash
# Ships the app to a Docker host you run yourself - a Proxmox VM or LXC - and
# (re)starts it. Unlike deploy.sh, the image is built on the server: a VM you
# own has the memory for it, and this machine needs no Docker at all.
#
#   ./deploy/proxmox.sh root@192.168.1.50    # first run; the target is remembered
#   ./deploy/proxmox.sh                      # every run after that
#
# Needs: SSH access to the host (a key, not a password prompt, is nicest),
# Docker with the compose plugin on it, and ports 80/443 reaching it from the
# internet (see deploy/PROXMOX.md).
set -euo pipefail

cd "$(dirname "$0")/.."

say() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
die() { echo "error: $*" >&2; exit 1; }

if [ $# -ge 1 ]; then
    printf 'TARGET=%q\n' "$1" > deploy/.proxmox
fi
# shellcheck disable=SC1091
[ -f deploy/.proxmox ] && . deploy/.proxmox
[ -n "${TARGET:-}" ] || die "no target. Run: ./deploy/proxmox.sh user@host"

REMOTE_DIR=iguesslow

[ -f .env ] || die ".env missing - copy .env.example and fill it in"
for key in SITE_DOMAIN FT_CLIENT_ID FT_CLIENT_SECRET; do
    grep -q "^$key=.\+" .env || die "$key is not set in .env"
done
DOMAIN=$(grep '^SITE_DOMAIN=' .env | cut -d= -f2-)

# The live site must not be a test instance.
if grep -q '^ADMIN_LOGINS=.\+' .env; then
    echo "    !! ADMIN_LOGINS is set in .env: this would deploy a TEST instance,"
    echo "    !! with /admin, the demo tools and a warning banner on every page."
    read -r -p "    Deploy it anyway? [y/N] " ok
    [ "$ok" = "y" ] || exit 1
fi

say "Checking Docker on $TARGET"
if ssh "$TARGET" 'docker info >/dev/null 2>&1'; then
    DOCKER=docker
elif ssh "$TARGET" 'sudo -n docker info >/dev/null 2>&1'; then
    DOCKER="sudo docker"
else
    die "Docker is not usable on $TARGET. See deploy/PROXMOX.md, step 2."
fi
ssh "$TARGET" "$DOCKER compose version" >/dev/null || die "the docker compose plugin is missing on $TARGET"
echo "    ok ($DOCKER)"

say "Checking that $DOMAIN points at the outside of your network"
public=$(ssh "$TARGET" 'curl -s --max-time 5 https://api.ipify.org || true')
resolved=$(getent hosts "$DOMAIN" | awk '{print $1}' | head -1 || true)
if [ -n "$public" ] && [ "$resolved" != "$public" ]; then
    echo "    !! $DOMAIN resolves to '${resolved:-nothing}', but $TARGET reaches the"
    echo "    !! internet from $public. Caddy cannot get a certificate until the"
    echo "    !! A record points at $public and ports 80/443 are forwarded."
    read -r -p "    Continue anyway? [y/N] " ok
    [ "$ok" = "y" ] || exit 1
else
    echo "    ok ($DOMAIN -> ${resolved:-?})"
fi

say "Copying the source"
# Replace the code directories wholesale, so deleted files do not linger. The
# database is in a Docker volume, not in this directory, and is untouched.
ssh "$TARGET" "mkdir -p ~/$REMOTE_DIR && cd ~/$REMOTE_DIR && rm -rf src templates static migrations"
tar czf - --exclude=./target --exclude=./data --exclude=./.git --exclude='./.env*' \
    --exclude=./deploy/.host --exclude=./deploy/.proxmox . \
    | ssh "$TARGET" "tar xzf - -C ~/$REMOTE_DIR"
scp -q .env "$TARGET:$REMOTE_DIR/.env"
ssh "$TARGET" "chmod 600 ~/$REMOTE_DIR/.env"

say "Building and starting (the first build compiles Rust: a few minutes)"
ssh "$TARGET" "cd ~/$REMOTE_DIR && $DOCKER compose up -d --build --remove-orphans"
ssh "$TARGET" "cd ~/$REMOTE_DIR && $DOCKER compose ps"

say "Checking it from the outside"
for _ in $(seq 1 30); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "https://$DOMAIN/healthz" || echo 000)
    [ "$code" = "200" ] && { echo "    https://$DOMAIN/healthz -> 200"; break; }
    printf '.'; sleep 5
done
echo

cat <<EOF

    https://$DOMAIN

    Logs      ssh $TARGET 'cd $REMOTE_DIR && $DOCKER compose logs -f'
    Restart   ssh $TARGET 'cd $REMOTE_DIR && $DOCKER compose restart'
    Stop      ssh $TARGET 'cd $REMOTE_DIR && $DOCKER compose down'
    Backup    see deploy/PROXMOX.md ("Backups")
EOF
