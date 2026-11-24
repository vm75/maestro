#!/usr/bin/env bash
set -euo pipefail

# Real, unprivileged Linux integration smoke test. No mocks, host service
# configuration changes, DNS provider calls, or firewall rule modifications.
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
python_bin=${PYTHON:-python3}
if [[ -z "${PYTHON:-}" && -x "$repo_root/.dev-tools/python/bin/python3" ]]; then
    python_bin="$repo_root/.dev-tools/python/bin/python3"
fi
# Keep all Python subprocesses on the same interpreter as the dependency check.
python3() { command "$python_bin" "$@"; }
tools=(nginx openssl curl "$python_bin")
if [[ -z "${MAESTRO_LINUX_SMOKE_BIN:-}" ]]; then
    tools+=(cargo)
fi
for tool in "${tools[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "Missing required command: $tool" >&2
        exit 2
    }
done

python3 -c 'import sys; assert sys.version_info >= (3, 11), "Python 3.11+ required"; import jinja2, yaml' || {
    echo 'Activate a Python 3.11+ environment with requirements.txt installed.' >&2
    exit 2
}
nginx_bin=$(command -v nginx)
lab=$(mktemp -d "${TMPDIR:-/tmp}/maestro-linux.XXXXXX")
maestro_pid=
keep_lab=${MAESTRO_KEEP_LINUX_LAB:-0}

cleanup() {
    local result=$?
    trap - EXIT
    if [[ -n "$maestro_pid" ]]; then
        kill "$maestro_pid" 2>/dev/null || true
        wait "$maestro_pid" 2>/dev/null || true
    fi
    if [[ -s "$lab/run/nginx.pid" ]]; then
        "$nginx_bin" -p "$lab/nginx/" -c "$lab/nginx/nginx.conf" -s quit >/dev/null 2>&1 || true
    fi
    if [[ "$keep_lab" == "1" ]]; then
        echo "Linux integration lab retained: $lab"
    else
        rm -rf -- "$lab"
    fi
    exit "$result"
}
trap cleanup EXIT

port() {
    python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}
api_port=$(port)
nginx_port=$(port)
while [[ "$nginx_port" == "$api_port" ]]; do nginx_port=$(port); done
nginx_tls_port=$(port)
while [[ "$nginx_tls_port" == "$api_port" || "$nginx_tls_port" == "$nginx_port" ]]; do nginx_tls_port=$(port); done
token=$(openssl rand -hex 24)
mkdir -p "$lab"/{data,run,logs/nginx,nginx/conf.d,nginx/domains-available,nginx/domains-enabled,nginx/snippets,init.d,wireguard}
mkdir -p "$lab/tmp" "$lab/nginx/logs"

# No automatic bans or external firewall writes in this unprivileged suite.
printf '%s\n' '{"enabled":false,"observe_only":false,"rules":[],"allowlist":[]}' > "$lab/firewall-policy.json"

python3 "$repo_root/scripts/render-nginx-config.py" \
    "$repo_root/config/nginx" "$lab/nginx" \
    --inputs-dir "$lab/nginx-inputs" \
    --var-dir "$lab/var" --pid-path "$lab/run/nginx.pid" \
    --error-log-path "$lab/logs/nginx/error.log" \
    --acme-directory "$lab/certificates/acme-challenge" \
    --http-port "$nginx_port" --https-port "$nginx_tls_port"
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=localhost \
    -keyout "$lab/nginx/certs/default/privkey.pem" \
    -out "$lab/nginx/certs/default/fullchain.pem" >/dev/null 2>&1
mkdir -p "$lab/tmp"
cat > "$lab/nginx/conf.d/00-linux-default.conf" <<EOF
server {
    server_name 127.0.0.1;
    listen 127.0.0.1:$nginx_port;
    location / { return 200 'maestro linux nginx'; }
}
EOF

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

bin=${MAESTRO_LINUX_SMOKE_BIN:-}
if [[ -z "$bin" ]]; then
    cd "$repo_root"
    cargo build --quiet --locked --no-default-features --features linux
    bin="$repo_root/target/debug/maestro"
fi
if [[ ! -x "$bin" ]]; then
    echo "Maestro binary is not executable: $bin" >&2
    exit 2
fi
"$bin" --config "$lab/maestro.yaml" --linux-mode check-config
"$bin" --config "$lab/maestro.yaml" --linux-mode serve >"$lab/logs/maestro.stdout" 2>&1 &
maestro_pid=$!

api="http://127.0.0.1:$api_port"
ready=false
for _ in $(seq 1 80); do
    if curl --silent --fail "$api/healthz" >/dev/null; then
        ready=true
        break
    fi
    if ! kill -0 "$maestro_pid" 2>/dev/null; then
        cat "$lab/logs/maestro.stdout" >&2
        echo "Maestro exited before becoming ready" >&2
        exit 1
    fi
    sleep 0.25
done
if [[ "$ready" != true ]]; then
    cat "$lab/logs/maestro.stdout" >&2
    echo "Maestro did not become ready" >&2
    exit 1
fi

get() {
    curl --silent --show-error --fail-with-body -H "Authorization: Bearer $token" "$api$1"
}
post() {
    curl --silent --show-error --fail-with-body -X POST -H "Authorization: Bearer $token" "$api$1"
}
real_result() {
    python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["success"] is True and x["simulated"] is False, x'
}

unauth=$(curl --silent --output /dev/null --write-out '%{http_code}' "$api/api/runtime")
[[ "$unauth" == 401 ]] || { echo "Expected HTTP 401, got $unauth" >&2; exit 1; }

get /api/runtime | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["test_mode"] is False and x["platform"] == "linux", x'
# Exercise a route that is only compiled with the Linux Cargo feature.
discovery=$(get /api/tools) || {
    echo "Linux tool discovery failed. Use a binary built with the linux feature." >&2
    exit 1
}
python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["tools"] and x["modules"], x' <<< "$discovery"
get /api/system/usage | python3 -c 'import json,sys; x=json.load(sys.stdin); assert "cpu" in x and "memory" in x, x'
get /api/firewall | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["policy"]["enabled"] is False, x'

# Test session-backed shared routes without sending WOL packets or touching host firewall.
shared_code() {
    curl --silent --output /dev/null --write-out '%{http_code}' "$@"
}
[[ $(shared_code "$api/api/shared/wol") == 401 ]]
curl --silent --show-error --fail-with-body -c "$lab/cookies" \
    -H "Origin: $api" -H 'Content-Type: application/json' \
    -d "{\"token\":\"$token\"}" "$api/api/session" >/dev/null
shared_get() {
    curl --silent --show-error --fail-with-body -b "$lab/cookies" "$api/api/shared/$1"
}
shared_write() {
    curl --silent --show-error --fail-with-body -b "$lab/cookies" \
        -H "Origin: $api" -H 'Content-Type: application/json' "$@"
}
shared_get modules | python3 -c 'import json,sys; x=json.load(sys.stdin); assert {m["id"] for m in x["modules"]} >= {"nginx","certificates","ban_shield","wol","adguard","wireguard"}, x'
machine='{"name":"Test machine","mac":"02:00:00:00:00:01","broadcast":"192.0.2.255","port":9}'
[[ $(shared_code -b "$lab/cookies" -H 'Content-Type: application/json' -d "$machine" "$api/api/shared/wol") == 403 ]]
[[ $(shared_code -b "$lab/cookies" -H 'Origin: https://invalid.example' -H 'Content-Type: application/json' -d "$machine" "$api/api/shared/wol") == 403 ]]
id=$(shared_write -d "$machine" "$api/api/shared/wol" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
shared_get wol | python3 -c 'import json,sys; x=json.load(sys.stdin); assert len(x)==1 and x[0]["name"]=="Test machine", x'
shared_write -X PUT -d '{"name":"Updated","mac":"02:00:00:00:00:01","port":9}' "$api/api/shared/wol/$id" >/dev/null
shared_get wol | python3 -c 'import json,sys; assert json.load(sys.stdin)[0]["name"]=="Updated"'
shared_write -X DELETE "$api/api/shared/wol/$id" >/dev/null
shared_get wol | python3 -c 'import json,sys; assert json.load(sys.stdin)==[]'
shared_get firewall/capability | python3 -c 'import json,sys; x=json.load(sys.stdin); assert not x["enforcement"], x'
[[ $(shared_code -b "$lab/cookies" "$api/api/shared/wireguard/peers") == 400 ]]
shared_get adguard/config | python3 -c 'import json,sys; x=json.load(sys.stdin); assert not x["enabled"] and "password" not in x'
shared_write -X PUT -d '{"enabled":false,"api_endpoint":"http://127.0.0.1:3000","username":"test","password":"not-a-real-password","lan_ip":"127.0.0.1"}' "$api/api/shared/adguard/config" >/dev/null
shared_get adguard/config | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["username"]=="test" and "password" not in x'
[[ $(shared_code -b "$lab/cookies" -H "Origin: $api" -H 'Content-Type: application/json' -X PUT -d '{"enabled":true,"observe_only":false}' "$api/api/shared/firewall") == 400 ]]
[[ $(shared_code -b "$lab/cookies" -H "Origin: $api" -H 'Content-Type: application/json' -d '{"name":"bad","domains":["example.test"],"method":"dns","staging":true,"auto_renew":false,"hook":"/tmp/invalid"}' "$api/api/shared/certificates") == 422 ]]
shared_get nginx/settings > "$lab/shared-nginx-settings.json"
python3 - "$lab/shared-nginx-settings.json" <<'PYSHAREDNGINX'
import json, sys
x=json.load(open(sys.argv[1]))
assert isinstance(x["shared"],dict) and isinstance(x["site_defaults"],dict), x
assert isinstance(x["allowed_ips"],list) and isinstance(x["limit_policies"],dict), x
PYSHAREDNGINX
shared_get nginx/ingress-usage | python3 -c 'import json,sys; assert isinstance(json.load(sys.stdin),dict)'
shared_write -d '{"kind":"domain","domain":"preview.test","settings":{"access_policy":"public"}}' "$api/api/shared/nginx/access-preview" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["access_policy"]=="public" and x["allowed_sources"] is None, x'
[[ $(shared_code -b "$lab/cookies" -H 'Content-Type: application/json' -X PUT --data-binary "@$lab/shared-nginx-settings.json" "$api/api/shared/nginx/settings") == 403 ]]
shared_write -X PUT --data-binary "@$lab/shared-nginx-settings.json" "$api/api/shared/nginx/settings" >/dev/null
shared_write -X POST "$api/api/shared/nginx/test" | real_result

post /api/nginx/test | real_result
post /api/nginx/start | real_result
get /api/nginx/status | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["running"] is True, x'
[[ $(curl --silent --fail "http://127.0.0.1:$nginx_port/") == "maestro linux nginx" ]]

# Regeneration must retain the lab's non-root runtime options and listener ports.
get /api/nginx/settings > "$lab/nginx-settings.json"
python3 - "$lab/nginx-settings.json" "$lab/nginx" <<'PYSETTINGS'
import json, sys
settings = json.load(open(sys.argv[1]))
assert settings['shared']['temp_dir'] == str(__import__('pathlib').Path(sys.argv[2]).parent / 'tmp'), settings
assert settings['shared']['nginx_user'] == '', settings
assert settings['shared']['nginx_daemon'] is True, settings
PYSETTINGS
curl --silent --show-error --fail-with-body -X PUT \
    -H "Authorization: Bearer $token" \
    -H "Content-Type: application/json" \
    --data-binary "@$lab/nginx-settings.json" "$api/api/nginx/settings" >/dev/null
post /api/nginx/test | real_result
[[ $(curl --silent --fail "http://127.0.0.1:$nginx_port/") == "maestro linux nginx" ]]

# Exercise the settings-only managed-site workflow against real nginx.
# Managed hosts use their own certificate folder; keep this entirely self-signed.
mkdir -p "$lab/nginx/certs/linux_test"
cp "$lab/nginx/certs/default/fullchain.pem" "$lab/nginx/certs/default/privkey.pem" "$lab/nginx/certs/linux_test/"
curl --silent --show-error --fail-with-body -X POST \
    -H "Authorization: Bearer $token" \
    -H "Content-Type: application/json" \
    --data '{"domain":"linux.test","name":"linux.test","enabled":true,"settings":{"disable_root":true,"enable_lanonly":false,"client_max_body_size":"2M"}}' \
    "$api/api/nginx/objects/domain" >/dev/null
[[ -f "$lab/nginx/domains-available/linux.test/root.conf" ]]
get /api/nginx/objects/domain/linux.test/linux.test | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["settings"]["client_max_body_size"] == "2M", x'
python3 - "$lab/nginx-inputs/sites.yaml" <<'PYSITES'
import sys, yaml
with open(sys.argv[1], 'rb') as stream:
    sites = yaml.safe_load(stream)['sites']
site, = sites.values()
assert site['domain'] == 'linux.test' and site['enabled'] is True, site
assert site['client_max_body_size'] == '2M', site
PYSITES
curl --silent --show-error --fail-with-body -X PUT \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
    --data '{"settings":{"disable_root":true,"enable_lanonly":false,"client_max_body_size":"3M"}}' \
    "$api/api/nginx/objects/domain/linux.test/linux.test" >/dev/null
# Shared-settings regeneration must preserve the saved site override.
curl --silent --show-error --fail-with-body -X PUT \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
    --data-binary "@$lab/nginx-settings.json" "$api/api/nginx/settings" >/dev/null
get /api/nginx/objects/domain/linux.test/linux.test | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["settings"]["client_max_body_size"] == "3M" and x["settings"]["enable_lanonly"] is False and "include snippets/restrict.conf;" not in x["content"], x'
# The bundle intentionally drops scanner user agents, including curl's default.
code=$(curl --silent --show-error --insecure --noproxy '*' \
    --user-agent 'Maestro-Linux-Smoke/1.0' \
    --resolve "linux.test:$nginx_tls_port:127.0.0.1" \
    --output /dev/null --write-out '%{http_code}' "https://linux.test:$nginx_tls_port/")
[[ "$code" == 404 ]] || { echo "Expected managed site HTTP 404, got $code" >&2; exit 1; }
curl --silent --show-error --fail-with-body -X DELETE \
    -H "Authorization: Bearer $token" "$api/api/nginx/objects/domain/linux.test/linux.test" >/dev/null
[[ ! -e "$lab/nginx/domains-available/linux.test/root.conf" ]]
post /api/nginx/test | real_result
post /api/nginx/reload | real_result
post /api/nginx/stop | real_result

echo "PASS: native Linux maestro uses real nginx, filesystem, auth, metrics and API"
