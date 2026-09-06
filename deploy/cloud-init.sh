#!/bin/bash
# Runs once on first boot. Installs Docker from Docker's own apt repository
# (Ubuntu's packaged docker.io lags and lacks the compose plugin).
set -eux

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y ca-certificates curl

install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
chmod a+r /etc/apt/keyrings/docker.asc

. /etc/os-release
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] \
https://download.docker.com/linux/ubuntu ${VERSION_CODENAME} stable" \
    > /etc/apt/sources.list.d/docker.list

apt-get update
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
usermod -aG docker ubuntu
systemctl enable --now docker

# 1 GB of swap: a t3.micro has only 1 GB of RAM, and Let's Encrypt renewals
# plus apt upgrades are happier with a little headroom.
if [ ! -f /swapfile ]; then
    fallocate -l 1G /swapfile
    chmod 600 /swapfile
    mkswap /swapfile
    swapon /swapfile
    echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

# Unattended security updates.
apt-get install -y unattended-upgrades
systemctl enable --now unattended-upgrades

touch /var/lib/cloud-init-app-ready
