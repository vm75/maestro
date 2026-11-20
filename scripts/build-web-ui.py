#!/usr/bin/env python3
"""Compose the offline ASUS UI without npm or runtime asset fetching."""
from __future__ import annotations

import argparse
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "web" / "src"
DESTINATION = ROOT / "web" / "index.html"
PARTS = [
    'head.html',
    'shell.html',
    'views/dashboard.html',
    'views/services.html',
    'views/adguard.html',
    'views/wireguard.html',
    'views/nginx.html',
    'views/certificates.html',
    'views/wol.html',
    'views/firewall.html',
    'dialogs.html',
    'js/bootstrap.js',
    'js/navigation.js',
    'js/dashboard.js',
    'js/services.js',
    'js/nginx.js',
    'js/certificates.js',
    'js/wol.js',
    'js/firewall.js',
    'js/adguard.js',
    'js/wireguard.js',
    'js/system.js',
    'footer.html'
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify the committed output")
    args = parser.parse_args()

    html = "".join((SOURCE / part).read_text(encoding="utf-8") for part in PARTS)
    if args.check:
        if not DESTINATION.exists() or DESTINATION.read_text(encoding="utf-8") != html:
            print("web/index.html is stale; run python3 scripts/build-web-ui.py")
            return 1
        print("web/index.html matches composed source")
        return 0
    DESTINATION.write_text(html, encoding="utf-8")
    print(f"Wrote {DESTINATION}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
