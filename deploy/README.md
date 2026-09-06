# Deploying

The app is one container plus Caddy for TLS, driven by `compose.yml`. That runs
identically on EC2, Lightsail, a VPS, or your laptop — the AWS scripts here
just create a machine to put it on.

## What you need first

1. **A domain.** Caddy gets a Let's Encrypt certificate for it automatically,
   but Let's Encrypt has to reach the box over the public internet, so a real
   name pointed at the instance is required. There is no free first year on
   Route 53 — a `.com` is ~$14/year plus $0.50/month for the hosted zone. The
   [GitHub Student Pack](https://education.github.com/pack) gives 42 students a
   free `.me` for a year through Namecheap, which works just as well.
2. **AWS credentials.** See below.
3. **A 42 application** with `https://<your-domain>/auth/callback` registered
   as a redirect URI.

## AWS credentials

In the AWS console, as the root user or an admin:

1. **IAM → Users → Create user**, name it something like `i-guess-42-deploy`.
   Leave console access off; this identity is only for the CLI.
2. **Next → Attach policies directly → Create policy → JSON**, paste
   [`iam-policy.json`](iam-policy.json), and name it `i-guess-42-deploy`.
   (`AmazonEC2FullAccess` also works and is one click, but it is far broader
   than this project needs.)
3. Create the user, open it, **Security credentials → Create access key →
   Command Line Interface**, and copy both halves.
4. On this machine:

   ```sh
   aws configure
   # AWS Access Key ID:     AKIA...
   # AWS Secret Access Key: ...
   # Default region name:   eu-central-1     # Frankfurt, closest to Vienna
   # Default output format: json
   ```

The secret access key is shown exactly once. If you lose it, delete the key and
make another.

## Provision, then deploy

```sh
./deploy/provision.sh          # ~2 minutes; prints the public IP
```

It creates an SSH key pair (saved to `~/.ssh/i-guess-42.pem`), a security group
allowing 22 from your IP and 80/443 from anywhere, a t3.micro running Ubuntu
24.04 with Docker, and an Elastic IP so the address survives reboots. Re-running
it reuses whatever already exists.

Point your domain's **A record** at the printed IP, wait for DNS, then:

```sh
cp .env.example .env           # SITE_DOMAIN + the 42 credentials
./deploy/deploy.sh
```

`deploy.sh` builds the image locally and pipes it over SSH rather than building
on the server — a t3.micro has 1 GB of RAM and would not enjoy compiling Rust.

## Costs

| | |
|---|---|
| t3.micro | free for 12 months on a new account's free tier, then ~$8/month |
| 20 GB gp3 disk | ~$1.60/month |
| Elastic IP | free while attached to a running instance, $0.005/hour otherwise |
| Data transfer | first 100 GB/month free |

Stopping the instance stops the compute charge but **not** the Elastic IP or
disk charge. To stop paying entirely, run `./deploy/teardown.sh`.

## Operating it

```sh
. deploy/.host                                        # HOST, KEY_FILE
ssh -i $KEY_FILE ubuntu@$HOST

cd i_guess_42
sudo docker compose logs -f app     # application logs
sudo docker compose logs -f caddy   # TLS / certificate problems show up here
sudo docker compose restart
```

### Backing up the database

Everything lives in one SQLite file on the `game_data` volume. SQLite is in WAL
mode, so copy the file only while the app is stopped — a clean shutdown
checkpoints the WAL into the main database:

```sh
. deploy/.host
ssh -i "$KEY_FILE" ubuntu@"$HOST" '
    cd i_guess_42
    sudo docker compose stop app >&2
    sudo docker run --rm -v i_guess_42_game_data:/data alpine tar cz -C /data .
    sudo docker compose start app >&2
' > game-$(date +%F).tar.gz
```

The site is down for the couple of seconds that takes. To restore, stop the app
and untar back into the same volume.

## Running a test instance alongside the live one

Keep them completely separate: different domain, different database, different
compose project name. The only difference in configuration is `ADMIN_LOGINS`,
which is what turns test mode on.

On the same server, put a second copy in its own directory:

```sh
ssh -i "$KEY_FILE" ubuntu@"$HOST"
mkdir -p ~/i_guess_42_test && cd ~/i_guess_42_test
cp ~/i_guess_42/compose.yml ~/i_guess_42/Caddyfile .

cat > .env <<'EOF'
SITE_DOMAIN=test.yourdomain.com
FT_CLIENT_ID=...          # the same 42 app is fine
FT_CLIENT_SECRET=...
ADMIN_LOGINS=your-42-login
EOF

# -p gives it its own volumes and network, so the two never share a database.
sudo docker compose -p ig42test up -d --no-build
```

Two things to get right:

1. Add an **A record** for `test.yourdomain.com` pointing at the same IP, and
   register `https://test.yourdomain.com/auth/callback` as a second redirect
   URI on the intra application. Caddy will get a separate certificate.
2. Both stacks want ports 80 and 443, and only one can have them. Either run
   the test instance behind the *live* Caddy by adding a second site block to
   its `Caddyfile`, or drop Caddy from the test stack and publish the app on a
   high port for your own use only.

The simplest version, if the test instance is only ever for you: skip the
domain entirely, run it locally with `docker compose up`, and reach it at
`http://localhost:3000`.

**Never set `ADMIN_LOGINS` on the live instance.** With it unset the admin
routes return 404, so nothing hints they exist.

## Running it somewhere that is not AWS

Nothing above is AWS-specific past `provision.sh`. On any machine with Docker
and a domain pointed at it:

```sh
git clone <repo> && cd i_guess_42
cp .env.example .env    # fill in
docker compose up -d --build
```

Fly.io, Railway, Hetzner, or a Raspberry Pi on your desk all work the same way.
The only requirements are ports 80 and 443 reachable from the internet (for the
certificate) and a persistent volume for `/app/data`.
