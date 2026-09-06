#!/usr/bin/env bash
# Creates the AWS infrastructure for i_guess_42: one EC2 instance with Docker,
# a static Elastic IP, and a security group that only exposes 22/80/443.
#
# Idempotent: re-running reuses anything it already made.
#
#   AWS_REGION=eu-central-1 ./deploy/provision.sh
set -euo pipefail

REGION="${AWS_REGION:-eu-central-1}"
NAME="${NAME:-i-guess-42}"
INSTANCE_TYPE="${INSTANCE_TYPE:-t3.micro}"   # free-tier eligible; t4g.small for ARM
ARCH="${ARCH:-amd64}"                        # arm64 if you switch to a t4g type
KEY_FILE="${KEY_FILE:-$HOME/.ssh/${NAME}.pem}"
SSH_CIDR="${SSH_CIDR:-}"                     # defaults to this machine's IP /32

say() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
aws_() { aws --region "$REGION" "$@"; }

command -v aws >/dev/null || { echo "aws CLI not found on PATH" >&2; exit 1; }

say "Checking credentials"
ident=$(aws_ sts get-caller-identity --output text --query '[Account,Arn]') || {
    echo "No working AWS credentials. Run: aws configure" >&2; exit 1; }
echo "    account/arn: $ident"

say "Resolving latest Ubuntu 24.04 AMI ($ARCH)"
AMI=$(aws_ ssm get-parameter \
    --name "/aws/service/canonical/ubuntu/server/24.04/stable/current/${ARCH}/hvm/ebs-gp3/ami-id" \
    --query 'Parameter.Value' --output text)
echo "    $AMI"

say "Key pair: $NAME"
if aws_ ec2 describe-key-pairs --key-names "$NAME" >/dev/null 2>&1; then
    echo "    already exists (reusing $KEY_FILE)"
    [ -f "$KEY_FILE" ] || { echo "    !! $KEY_FILE is missing and AWS will not re-issue it." >&2
                            echo "    !! Delete the key pair in EC2 and re-run to get a fresh one." >&2; exit 1; }
else
    mkdir -p "$(dirname "$KEY_FILE")"
    aws_ ec2 create-key-pair --key-name "$NAME" \
        --query 'KeyMaterial' --output text > "$KEY_FILE"
    chmod 600 "$KEY_FILE"
    echo "    created, private key saved to $KEY_FILE"
fi

say "Security group: $NAME"
SG=$(aws_ ec2 describe-security-groups --filters "Name=group-name,Values=$NAME" \
        --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || echo "None")
if [ "$SG" = "None" ] || [ -z "$SG" ]; then
    SG=$(aws_ ec2 create-security-group --group-name "$NAME" \
            --description "i_guess_42 web server" --query 'GroupId' --output text)
    echo "    created $SG"
else
    echo "    reusing $SG"
fi

if [ -z "$SSH_CIDR" ]; then
    myip=$(curl -fsS https://checkip.amazonaws.com || true)
    SSH_CIDR="${myip:+${myip}/32}"
    SSH_CIDR="${SSH_CIDR:-0.0.0.0/0}"
fi
echo "    SSH allowed from $SSH_CIDR"

# Adding a rule that already exists is an error, so swallow just that case.
authorize() {
    aws_ ec2 authorize-security-group-ingress --group-id "$SG" \
        --protocol tcp --port "$1" --cidr "$2" >/dev/null 2>&1 \
        || echo "    port $1 from $2 already allowed"
}
authorize 22 "$SSH_CIDR"
authorize 80 0.0.0.0/0
authorize 443 0.0.0.0/0

say "Instance: $NAME"
IID=$(aws_ ec2 describe-instances \
        --filters "Name=tag:Name,Values=$NAME" "Name=instance-state-name,Values=pending,running,stopping,stopped" \
        --query 'Reservations[0].Instances[0].InstanceId' --output text 2>/dev/null || echo "None")
if [ "$IID" = "None" ] || [ -z "$IID" ]; then
    IID=$(aws_ ec2 run-instances \
        --image-id "$AMI" \
        --instance-type "$INSTANCE_TYPE" \
        --key-name "$NAME" \
        --security-group-ids "$SG" \
        --user-data "file://$(dirname "$0")/cloud-init.sh" \
        --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=20,VolumeType=gp3,DeleteOnTermination=true}' \
        --metadata-options 'HttpTokens=required,HttpEndpoint=enabled' \
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$NAME}]" \
        --query 'Instances[0].InstanceId' --output text)
    echo "    launched $IID"
else
    echo "    reusing $IID"
    state=$(aws_ ec2 describe-instances --instance-ids "$IID" \
            --query 'Reservations[0].Instances[0].State.Name' --output text)
    [ "$state" = "stopped" ] && aws_ ec2 start-instances --instance-ids "$IID" >/dev/null
fi

say "Waiting for the instance to run"
aws_ ec2 wait instance-running --instance-ids "$IID"

say "Elastic IP"
ALLOC=$(aws_ ec2 describe-addresses --filters "Name=tag:Name,Values=$NAME" \
          --query 'Addresses[0].AllocationId' --output text 2>/dev/null || echo "None")
if [ "$ALLOC" = "None" ] || [ -z "$ALLOC" ]; then
    ALLOC=$(aws_ ec2 allocate-address --domain vpc \
              --tag-specifications "ResourceType=elastic-ip,Tags=[{Key=Name,Value=$NAME}]" \
              --query 'AllocationId' --output text)
    echo "    allocated $ALLOC"
fi
aws_ ec2 associate-address --instance-id "$IID" --allocation-id "$ALLOC" >/dev/null
IP=$(aws_ ec2 describe-addresses --allocation-ids "$ALLOC" --query 'Addresses[0].PublicIp' --output text)

cat > deploy/.host <<EOF
HOST=$IP
KEY_FILE=$KEY_FILE
INSTANCE_ID=$IID
REGION=$REGION
EOF

say "Done"
cat <<EOF

    Public IP   $IP
    Instance    $IID  ($INSTANCE_TYPE, $REGION)
    SSH         ssh -i $KEY_FILE ubuntu@$IP

    Next:
      1. Point your domain's A record at $IP and wait for DNS to propagate.
      2. Put that domain in .env as SITE_DOMAIN, with the 42 credentials.
      3. Register https://<domain>/auth/callback on the intra application.
      4. ./deploy/deploy.sh

    Docker is still installing on first boot; deploy.sh waits for it.
EOF
