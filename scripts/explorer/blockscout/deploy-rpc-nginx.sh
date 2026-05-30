#!/usr/bin/env bash
set -euo pipefail

RPC_NGINX_REMOTE_HOST="${RPC_NGINX_REMOTE_HOST:-}"
SSH_KEY_PATH="${SSH_KEY_PATH:-}"

RPC_ZKSYS_DOMAIN="${RPC_ZKSYS_DOMAIN:-rpc-zk.tanenbaum.io}"
RPC_GATEWAY_DOMAIN="${RPC_GATEWAY_DOMAIN:-rpc-gw.tanenbaum.io}"
ZKSYS_RPC_UPSTREAM="${ZKSYS_RPC_UPSTREAM:-http://127.0.0.1:3050}"
GATEWAY_RPC_UPSTREAM="${GATEWAY_RPC_UPSTREAM:-http://127.0.0.1:3052}"
RPC_NGINX_INCLUDE_ZKSYS="${RPC_NGINX_INCLUDE_ZKSYS:-1}"
RPC_NGINX_INCLUDE_GATEWAY="${RPC_NGINX_INCLUDE_GATEWAY:-1}"
RPC_NGINX_REMOVE_ZKSYS="${RPC_NGINX_REMOVE_ZKSYS:-0}"
RPC_NGINX_REMOVE_GATEWAY="${RPC_NGINX_REMOVE_GATEWAY:-0}"

# Comma-separated IPs/CIDRs allowed to use the private Gateway RPC.
# Set this to the Blockscout host IP/CIDR plus any operator admin IP/CIDRs.
GATEWAY_RPC_ALLOWLIST="${GATEWAY_RPC_ALLOWLIST:-}"

# Set RPC_NGINX_ENABLE_TLS=1 after DNS points at this host. Requires LETSENCRYPT_EMAIL.
RPC_NGINX_ENABLE_TLS="${RPC_NGINX_ENABLE_TLS:-0}"
LETSENCRYPT_EMAIL="${LETSENCRYPT_EMAIL:-}"

if [[ -z "${RPC_NGINX_REMOTE_HOST}" ]]; then
  echo "RPC_NGINX_REMOTE_HOST is required, for example ubuntu@node-host" >&2
  exit 1
fi

if [[ "${RPC_NGINX_INCLUDE_ZKSYS}" != "1" && "${RPC_NGINX_INCLUDE_GATEWAY}" != "1" ]]; then
  echo "at least one of RPC_NGINX_INCLUDE_ZKSYS or RPC_NGINX_INCLUDE_GATEWAY must be 1" >&2
  exit 1
fi

if [[ "${RPC_NGINX_INCLUDE_GATEWAY}" == "1" && -z "${GATEWAY_RPC_ALLOWLIST}" ]]; then
  echo "GATEWAY_RPC_ALLOWLIST is required, for example '<blockscout-ip>,<admin-ip-or-cidr>'" >&2
  exit 1
fi

nginx_gateway_geo_lines() {
  local list="$1"
  local entry

  IFS=',' read -r -a entries <<<"${list}"
  for entry in "${entries[@]}"; do
    entry="$(printf '%s' "${entry}" | xargs)"
    [[ -z "${entry}" ]] && continue
    if [[ ! "${entry}" =~ ^[0-9A-Fa-f:./]+$ ]]; then
      echo "invalid GATEWAY_RPC_ALLOWLIST entry: ${entry}" >&2
      exit 1
    fi
    printf '    %s 1;\n' "${entry}"
  done
}

gateway_geo_lines=""
if [[ "${RPC_NGINX_INCLUDE_GATEWAY}" == "1" ]]; then
  gateway_geo_lines="$(nginx_gateway_geo_lines "${GATEWAY_RPC_ALLOWLIST}")"
fi

nginx_common_config="$(
  cat <<EOF
map \$http_upgrade \$connection_upgrade {
    default upgrade;
    '' close;
}

EOF
)"

nginx_gateway_config=""
if [[ "${RPC_NGINX_INCLUDE_GATEWAY}" == "1" ]]; then
  nginx_gateway_config="$(
    cat <<EOF
geo \$gateway_rpc_allowed {
    default 0;
    127.0.0.1 1;
    ::1 1;
${gateway_geo_lines}
}

EOF
  )"
fi

nginx_zksys_config=""
if [[ "${RPC_NGINX_INCLUDE_ZKSYS}" == "1" ]]; then
  nginx_zksys_config="$(
    cat <<EOF
server {
    listen 80;
    server_name ${RPC_ZKSYS_DOMAIN};
    client_max_body_size 20m;

    location ^~ /.well-known/acme-challenge/ {
        root /var/www/html;
    }

    location / {
        if (\$request_method = OPTIONS) {
            add_header Access-Control-Allow-Origin "*" always;
            add_header Access-Control-Allow-Methods "POST, OPTIONS" always;
            add_header Access-Control-Allow-Headers "content-type" always;
            add_header Access-Control-Max-Age 86400 always;
            return 204;
        }

        limit_except POST OPTIONS {
            deny all;
        }

        # SYSCOIN: OS RPC already emits CORS headers; hide upstream value so
        # browsers do not reject duplicated Access-Control-Allow-Origin values.
        proxy_hide_header Access-Control-Allow-Origin;
        add_header Access-Control-Allow-Origin "*" always;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection \$connection_upgrade;
        proxy_read_timeout 120s;
        proxy_send_timeout 120s;
        proxy_pass ${ZKSYS_RPC_UPSTREAM};
    }
}

EOF
  )"
fi

if [[ "${RPC_NGINX_INCLUDE_GATEWAY}" == "1" ]]; then
  nginx_gateway_config+="$(
    cat <<EOF
server {
    listen 80;
    server_name ${RPC_GATEWAY_DOMAIN};
    client_max_body_size 20m;

    location ^~ /.well-known/acme-challenge/ {
        root /var/www/html;
    }

    location / {
        if (\$gateway_rpc_allowed = 0) {
            return 403;
        }

        if (\$request_method = OPTIONS) {
            return 204;
        }

        limit_except POST OPTIONS {
            deny all;
        }

        # SYSCOIN: keep the Gateway RPC private to the IP allowlist. The OS RPC
        # emits wildcard CORS itself, so hide it instead of making this private
        # vhost readable from arbitrary browser origins.
        proxy_hide_header Access-Control-Allow-Origin;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection \$connection_upgrade;
        proxy_read_timeout 120s;
        proxy_send_timeout 120s;
        proxy_pass ${GATEWAY_RPC_UPSTREAM};
    }
}
EOF
  )"
fi

common_config_b64="$(printf '%s' "${nginx_common_config}" | base64 | tr -d '\n')"
zksys_config_b64="$(printf '%s' "${nginx_zksys_config}" | base64 | tr -d '\n')"
gateway_config_b64="$(printf '%s' "${nginx_gateway_config}" | base64 | tr -d '\n')"
ssh_opts=(-o StrictHostKeyChecking=accept-new)
if [[ -n "${SSH_KEY_PATH}" ]]; then
  ssh_opts+=(-i "${SSH_KEY_PATH}")
fi

ssh "${ssh_opts[@]}" "${RPC_NGINX_REMOTE_HOST}" \
  bash -s -- \
  "${common_config_b64}" \
  "${zksys_config_b64}" \
  "${gateway_config_b64}" \
  "${RPC_NGINX_ENABLE_TLS}" \
  "${LETSENCRYPT_EMAIL}" \
  "${RPC_ZKSYS_DOMAIN}" \
  "${RPC_GATEWAY_DOMAIN}" \
  "${RPC_NGINX_INCLUDE_ZKSYS}" \
  "${RPC_NGINX_INCLUDE_GATEWAY}" \
  "${RPC_NGINX_REMOVE_ZKSYS}" \
  "${RPC_NGINX_REMOVE_GATEWAY}" <<'REMOTE_SCRIPT'
set -euo pipefail

COMMON_CONFIG_B64="$1"
ZKSYS_CONFIG_B64="$2"
GATEWAY_CONFIG_B64="$3"
RPC_NGINX_ENABLE_TLS="$4"
LETSENCRYPT_EMAIL="$5"
RPC_ZKSYS_DOMAIN="$6"
RPC_GATEWAY_DOMAIN="$7"
RPC_NGINX_INCLUDE_ZKSYS="$8"
RPC_NGINX_INCLUDE_GATEWAY="$9"
RPC_NGINX_REMOVE_ZKSYS="${10}"
RPC_NGINX_REMOVE_GATEWAY="${11}"

if ! command -v nginx >/dev/null 2>&1; then
  sudo apt-get update
  sudo apt-get install -y nginx
fi

sudo install -d -m 0755 /etc/nginx/conf.d /var/www/html
legacy_config=/etc/nginx/conf.d/zksync-rpc.conf
common_config=/etc/nginx/conf.d/zksync-rpc-common.conf
zksys_config=/etc/nginx/conf.d/zksync-rpc-zksys.conf
gateway_config=/etc/nginx/conf.d/zksync-rpc-gateway.conf

if [[ -f "${legacy_config}" ]]; then
  if grep -q "server_name[[:space:]]\\+${RPC_ZKSYS_DOMAIN}" "${legacy_config}" \
    && [[ "${RPC_NGINX_INCLUDE_ZKSYS}" != "1" && "${RPC_NGINX_REMOVE_ZKSYS}" != "1" ]]; then
    echo "refusing to drop existing ${RPC_ZKSYS_DOMAIN} vhost from ${legacy_config}; set RPC_NGINX_INCLUDE_ZKSYS=1 or RPC_NGINX_REMOVE_ZKSYS=1" >&2
    exit 1
  fi
  if grep -q "server_name[[:space:]]\\+${RPC_GATEWAY_DOMAIN}" "${legacy_config}" \
    && [[ "${RPC_NGINX_INCLUDE_GATEWAY}" != "1" && "${RPC_NGINX_REMOVE_GATEWAY}" != "1" ]]; then
    echo "refusing to drop existing ${RPC_GATEWAY_DOMAIN} vhost from ${legacy_config}; set RPC_NGINX_INCLUDE_GATEWAY=1 or RPC_NGINX_REMOVE_GATEWAY=1" >&2
    exit 1
  fi
fi

printf '%s' "${COMMON_CONFIG_B64}" | base64 -d | sudo tee "${common_config}" >/dev/null
if [[ "${RPC_NGINX_INCLUDE_ZKSYS}" == "1" ]]; then
  printf '%s' "${ZKSYS_CONFIG_B64}" | base64 -d | sudo tee "${zksys_config}" >/dev/null
elif [[ "${RPC_NGINX_REMOVE_ZKSYS}" == "1" ]]; then
  sudo rm -f "${zksys_config}"
fi

if [[ "${RPC_NGINX_INCLUDE_GATEWAY}" == "1" ]]; then
  printf '%s' "${GATEWAY_CONFIG_B64}" | base64 -d | sudo tee "${gateway_config}" >/dev/null
elif [[ "${RPC_NGINX_REMOVE_GATEWAY}" == "1" ]]; then
  sudo rm -f "${gateway_config}"
fi

if [[ -f "${legacy_config}" ]]; then
  sudo mv "${legacy_config}" "${legacy_config}.bak.$(date +%Y%m%d%H%M%S)"
fi

sudo nginx -t
sudo systemctl reload nginx

if [[ "${RPC_NGINX_ENABLE_TLS}" == "1" ]]; then
  if [[ -z "${LETSENCRYPT_EMAIL}" ]]; then
    echo "LETSENCRYPT_EMAIL is required when RPC_NGINX_ENABLE_TLS=1" >&2
    exit 1
  fi

  sudo apt-get update
  sudo apt-get install -y certbot python3-certbot-nginx
  domains=()
  if [[ "${RPC_NGINX_INCLUDE_ZKSYS}" == "1" ]]; then
    domains+=(-d "${RPC_ZKSYS_DOMAIN}")
  fi
  if [[ "${RPC_NGINX_INCLUDE_GATEWAY}" == "1" ]]; then
    domains+=(-d "${RPC_GATEWAY_DOMAIN}")
  fi
  sudo certbot --nginx \
    --non-interactive \
    --agree-tos \
    --expand \
    --redirect \
    --email "${LETSENCRYPT_EMAIL}" \
    "${domains[@]}"
fi

echo "installed /etc/nginx/conf.d/zksync-rpc-*.conf"
if [[ "${RPC_NGINX_INCLUDE_ZKSYS}" == "1" ]]; then
  echo "public zksys RPC: https://${RPC_ZKSYS_DOMAIN}/"
fi
if [[ "${RPC_NGINX_INCLUDE_GATEWAY}" == "1" ]]; then
  echo "allowlisted gateway RPC: https://${RPC_GATEWAY_DOMAIN}/"
fi
REMOTE_SCRIPT
