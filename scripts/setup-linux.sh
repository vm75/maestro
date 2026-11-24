#!/usr/bin/env bash
set -euo pipefail

# Prepare a persistent, unprivileged Linux lab; never replace existing files.
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
lab="$repo_root/data/linux"
missing=0
for tool in cargo nginx openssl curl python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'Missing required command: %s\n' "$tool" >&2
        missing=1
    fi
done
if (( missing )); then
    echo 'Install the missing commands with your system package manager, then rerun this script.' >&2
    exit 2
fi
# Check Python requirements before creating any lab files.
python3 -c 'import sys; assert sys.version_info >= (3, 11), "Python 3.11+ required"; import jinja2, yaml' || {
    echo 'Activate a Python 3.11+ environment with requirements.txt installed.' >&2
    exit 2
}
api_port=3030
nginx_port=8080
nginx_tls_port=8443
umask 077
mkdir -p "$lab"/{run,logs/nginx,nginx/conf.d,nginx/domains-available,nginx/domains-enabled,nginx/snippets,init.d,wireguard,certs}
mkdir -p "$lab/tmp" "$lab/nginx/logs"

nginx_bin=$(command -v nginx)
dehydrated_bin=$(command -v dehydrated || true)
if [[ -z "$dehydrated_bin" ]]; then
    dehydrated_bin="$lab/certificates/dehydrated"
    if curl -fsSL "https://raw.githubusercontent.com/dehydrated-io/dehydrated/master/dehydrated" -o "$dehydrated_bin" 2>/dev/null; then
        chmod 0700 "$dehydrated_bin"
        echo "Downloaded latest dehydrated to $dehydrated_bin."
    else
        echo "Dehydrated is not installed. Use Certificates > Update dehydrated after setup."
    fi
fi
if [[ ! -e "$lab/dehydrated-dns01-hook.sh" && -f "$repo_root/scripts/dehydrated-dns01-hook.sh" ]]; then
    install -m 0755 "$repo_root/scripts/dehydrated-dns01-hook.sh" "$lab/dehydrated-dns01-hook.sh"
fi
if [[ ! -e "$lab/firewall-policy.json" && ! -L "$lab/firewall-policy.json" ]]; then
    printf '%s\n' '{"enabled":false,"observe_only":false,"rules":[],"allowlist":[]}' > "$lab/firewall-policy.json"
fi

if [[ ! -e "$lab/nginx/nginx.conf" && ! -L "$lab/nginx/nginx.conf" ]]; then
    python3 "$repo_root/scripts/render-nginx-config.py" \
        "$repo_root/config/nginx" "$lab/nginx" \
        --inputs-dir "$lab/nginx-inputs" \
        --var-dir "$lab/var" --pid-path "$lab/run/nginx.pid" \
        --error-log-path "$lab/var/log/nginx/error.log" \
        --acme-directory "$lab/certificates/acme-challenge" \
        --http-port "$nginx_port" --https-port "$nginx_tls_port"
    openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=localhost \
        -keyout "$lab/nginx/certs/default/privkey.pem" \
        -out "$lab/nginx/certs/default/fullchain.pem" >/dev/null 2>&1
    cat > "$lab/nginx/conf.d/00-linux-default.conf" <<EOF
server {
    server_name 127.0.0.1;
    listen $nginx_port;
    location ^~ /.well-known/acme-challenge/ {
        alias $lab/certificates/acme-challenge/;
        default_type text/plain;
    }
    location / { return 200 'maestro linux nginx'; }
}
EOF
fi

if [[ ! -e "$lab/maestro.yaml" && ! -L "$lab/maestro.yaml" ]]; then
    token=$(openssl rand -hex 24)
    printf '%s\n' "$token" > "$lab/token"
    chmod 0600 "$lab/token"
    cat > "$lab/maestro.yaml" <<EOF
version: 1
server:
  listen: "127.0.0.1:$api_port"
  token_file: "$lab/token"
  state_dir: "$lab"
EOF
    chmod 0600 "$lab/maestro.yaml"

fi

mkdir -p "$repo_root/data/linux/init.d"
for name in S80nginx S99maestro; do
    if [[ ! -e "$repo_root/data/linux/init.d/$name" && ! -e "$repo_root/data/linux/init.d/.disabled.$name" ]]; then
        install -m 700 "$repo_root/scripts/linux-init.py" "$repo_root/data/linux/init.d/$name"
    fi
done

printf 'Linux development environment ready: %s\n' "$lab"
printf 'Unified Linux config: %s/maestro.yaml\n' "$lab"
printf 'API token is stored in the private file %s/token\n' "$lab"
printf 'Start from the repository root:\n  make run-linux\n'
echo 'New configs use http://127.0.0.1:3030 and nginx port 8080. Existing files are preserved.'
echo 'Optional feature tools: ip, ping, wg, wg-quick, dehydrated, ipset, iptables, ip6tables.'
echo 'Configure their paths before testing those features; firewall enforcement requires an isolated VM or network namespace.'
