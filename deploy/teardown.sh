#!/usr/bin/env bash
# Destroys everything provision.sh created, so the account stops being billed.
#
# This deletes the instance and its disk. The SQLite database goes with it -
# back it up first (see deploy/README.md).
#
#   ./deploy/teardown.sh
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f deploy/.host ] || { echo "deploy/.host missing - nothing recorded to tear down" >&2; exit 1; }
# shellcheck disable=SC1091
. deploy/.host

NAME="${NAME:-i-guess-42}"
aws_() { aws --region "$REGION" "$@"; }
say() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }

cat <<EOF

This will permanently delete:

    instance      $INSTANCE_ID  (and its 20 GB disk)
    elastic IP    $HOST
    security group / key pair   $NAME

The game database lives on that disk and is NOT backed up by this script.

EOF
read -r -p "Type the instance id to confirm: " confirm
[ "$confirm" = "$INSTANCE_ID" ] || { echo "Did not match - nothing was deleted."; exit 1; }

say "Releasing the Elastic IP"
ALLOC=$(aws_ ec2 describe-addresses --filters "Name=tag:Name,Values=$NAME" \
          --query 'Addresses[0].AllocationId' --output text 2>/dev/null || echo "None")
if [ "$ALLOC" != "None" ] && [ -n "$ALLOC" ]; then
    aws_ ec2 disassociate-address --allocation-id "$ALLOC" >/dev/null 2>&1 || true
    aws_ ec2 release-address --allocation-id "$ALLOC"
    echo "    released $ALLOC"
fi

say "Terminating the instance"
aws_ ec2 terminate-instances --instance-ids "$INSTANCE_ID" >/dev/null
aws_ ec2 wait instance-terminated --instance-ids "$INSTANCE_ID"
echo "    gone"

say "Deleting the security group"
# The group cannot go until the ENI is fully released; retry for a minute.
for _ in $(seq 1 12); do
    if aws_ ec2 delete-security-group --group-name "$NAME" >/dev/null 2>&1; then
        echo "    deleted"; break
    fi
    printf '.'; sleep 5
done
echo

say "Deleting the key pair"
aws_ ec2 delete-key-pair --key-name "$NAME" >/dev/null && echo "    deleted"
rm -f deploy/.host

cat <<EOF

Done. Nothing from this project is still running in $REGION.
The local private key is still at ~/.ssh/$NAME.pem - delete it if you want.
EOF
