#!/usr/bin/env bash
# Source this file to load local DNS/ACME credentials into the current shell:
#
#   . scripts/source-dnsprovider.sh
#   scripts/run-dashboard.sh
#
# The credentials live in .secrets/dnsprovider, which is gitignored and is
# never copied, printed, or uploaded. The file is parsed as simple
# "NAME = value" lines (spaces around "=" are allowed, export is optional), so
# it works whether it is written as a shell file or as a key/value list.
#
# Recognised Tencent Cloud pairs (first match wins in the dashboard):
#   TENCENT_KEY / TENCENT_SECRET
#   TENCENTCLOUD_SECRET_ID / TENCENTCLOUD_SECRET_KEY
#   TENCENT_SECRET_ID / TENCENT_SECRET_KEY
#   TENCENT_DNSPOD_SECRET_ID / TENCENT_DNSPOD_SECRET_KEY
#   DNSPOD_SECRET_ID / DNSPOD_SECRET_KEY
#   Tencent_SecretId / Tencent_SecretKey
#
# Optional:
#   ZEROSSL_EAB_KID / ZEROSSL_EAB_HMAC_KEY   enable ZeroSSL (tried first)
#   ACME_EMAIL                               fallback ACME contact email
#   HTTPS_PROXY / HTTP_PROXY / NO_PROXY      used by reqwest for ACME + DNSPod
#   ACME_DNS_PROPAGATION_SECS                DNS-01 wait, default 15

if [ -n "${BASH_SOURCE[0]:-}" ]; then
    _webgateway_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
else
    _webgateway_root="$(pwd)"
fi

_webgateway_secrets="$_webgateway_root/.secrets/dnsprovider"
if [ -f "$_webgateway_secrets" ]; then
    while IFS= read -r _webgateway_line || [ -n "$_webgateway_line" ]; do
        _webgateway_line="${_webgateway_line%$'\r'}"
        if [[ "$_webgateway_line" =~ ^[[:space:]]*(export[[:space:]]+)?([A-Za-z_][A-Za-z0-9_]*)[[:space:]]*=[[:space:]]*(.*)$ ]]; then
            _webgateway_name="${BASH_REMATCH[2]}"
            _webgateway_value="${BASH_REMATCH[3]}"
            # Strip matching quotes, but keep the value otherwise literal.
            if [[ "$_webgateway_value" =~ ^\"(.*)\"$ ]] || [[ "$_webgateway_value" =~ ^'(.*)'$ ]]; then
                _webgateway_value="${BASH_REMATCH[1]}"
            fi
            export "$_webgateway_name=$_webgateway_value"
        fi
    done < "$_webgateway_secrets"
fi

unset _webgateway_root _webgateway_secrets _webgateway_line _webgateway_name _webgateway_value
