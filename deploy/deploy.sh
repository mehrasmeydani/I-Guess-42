#!/usr/bin/env bash
# Ships the app to the instance created by provision.sh and (re)starts it.
#
# The image is built here and piped over SSH rather than built on the server:
# a t3.micro has 1 GB of RAM and would struggle to compile Rust.
#
#   ./deploy/deploy.sh
set -euo pipefail

cd "$(dirname "$0")/.."

[ -f deploy/.host ] || { echo "deploy/.host missing - run ./deploy/provision.sh first" >&2; exit 1; }
# shellcheck disable=SC1091
. deploy/.host
[ -f .env ] || { echo ".env missing - copy .env.example and fill it in" >&2; exit 1; }

grep -q '^SITE_DOMAIN=.\+' .env || { echo "SITE_DOMAIN is not set in .env" >&2; exit 1; }
grep -q '^FT_CLIENT_ID=.\+' .env || { echo "FT_CLIENT_ID is not set in .env" >&2; exit 1; }
grep -q '^FT_CLIENT_SECRET=.\+' .env || { echo "FT_CLIENT_SECRET is not set in .env" >&2; exit 1; }
DOMAIN=$(grep '^SITE_DOMAIN=' .env | cut -d= -f2-)

SSH="ssh -i $KEY_FILE -o StrictHostKeyChecking=accept-new ubuntu@$HOST"

say() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }

say "Checking that $DOMAIN resolves to $HOST"
resolved=$(getent hosts "$DOMAIN" | awk '{print $1}' | head -1 || true)
if [ "$resolved" != "$HOST" ]; then
    echo "    !! $DOMAIN resolves to '${resolved:-nothing}', not $HOST."
    echo "    !! Caddy will fail to get a certificate until DNS points here."
    read -r -p "    Continue anyway? [y/N] " ok
    [ "$ok" = "y" ] || exit 1
else
    echo "    ok"
fi

say "Waiting for Docker on the instance"
for _ in $(seq 1 60); do
    $SSH 'command -v docker >/dev/null && sudo docker info >/dev/null 2>&1' && break
    printf '.'; sleep 10
done
echo

say "Building the image locally"
docker build -q -t i_guess_42:latest . >/dev/null
docker image inspect i_guess_42:latest --format '    {{.Id}} {{.Size}} bytes'

say "Shipping the image (this is the slow part)"
docker save i_guess_42:latest | gzip -1 | $SSH 'gunzip | sudo docker load'

say "Shipping the compose files"
$SSH 'mkdir -p ~/i_guess_42'
scp -q -i "$KEY_FILE" compose.yml Caddyfile .env "ubuntu@$HOST:~/i_guess_42/"

say "Starting the stack"
# --no-build: the image was just loaded, and there is no source tree up there.
$SSH 'cd ~/i_guess_42 && sudo docker compose up -d --no-build --remove-orphans'
sleep 5
$SSH 'cd ~/i_guess_42 && sudo docker compose ps'

say "Checking it from the outside"
for _ in $(seq 1 30); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "https://$DOMAIN/healthz" || echo 000)
    [ "$code" = "200" ] && { echo "    https://$DOMAIN/healthz -> 200"; break; }
    printf '.'; sleep 5
done
echo

cat <<EOF

    Live at https://$DOMAIN

    Logs      ssh -i $KEY_FILE ubuntu@$HOST 'cd i_guess_42 && sudo docker compose logs -f'
    Restart   ssh -i $KEY_FILE ubuntu@$HOST 'cd i_guess_42 && sudo docker compose restart'
    Backup    see deploy/README.md ("Backing up the database")
    Teardown  ./deploy/teardown.sh
EOF
