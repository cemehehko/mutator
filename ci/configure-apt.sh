#!/usr/bin/env bash
set -euo pipefail

security_snapshot=https://snapshot.debian.org/archive/debian-security/20260901T000000Z

for source in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do
  [ -f "$source" ] || continue
  sed -i -E \
    -e 's@https?://deb.debian.org/debian-security@https://security.debian.org/debian-security@g' \
    -e 's@http://security.debian.org/debian-security@https://security.debian.org/debian-security@g' \
    -e 's@https?://deb.debian.org/debian@https://mirror.yandex.ru/debian@g' \
    "$source"

  case "$source" in
    *.list)
      sed -i -E '/^deb(-src)? .*https:\/\/security\.debian\.org\/debian-security[[:space:]]+bullseye-security[[:space:]]/ {
        /check-valid-until=/! {
          /\[/ s/\[/[check-valid-until=no /
          /\[/! s/^(deb(-src)?)[[:space:]]+/\1 [check-valid-until=no] /
        }
      }' "$source"
      sed -i -E "/^deb(-src)? .* bullseye-security[[:space:]]/ s@https://security.debian.org/debian-security@${security_snapshot}@g" "$source"
      ;;
    *.sources)
      awk -v snapshot="$security_snapshot" 'BEGIN { RS=""; ORS="\n\n" }
        /(^|\n)Suites:[ \t]*bullseye-security[ \t]*(\n|$)/ &&
        /https:\/\/security\.debian\.org\/debian-security/ {
          gsub(/https:\/\/security\.debian\.org\/debian-security/, snapshot)
          if ($0 !~ /(^|\n)Check-Valid-Until:/)
            $0 = $0 "\nCheck-Valid-Until: no"
        }
        { print }
      ' "$source" > "${source}.tmp"
      mv "${source}.tmp" "$source"
      ;;
  esac
done
rm -rf /var/lib/apt/lists/*
cat > /etc/apt/apt.conf.d/80ci-reliability <<'EOF'
Acquire::Retries "3";
APT::Update::Error-Mode "any";
EOF
