#!/usr/bin/env python3
"""Check internal links in the built docs site.

Hugo does not validate plain markdown links, so this walks every rendered HTML
page under the build output (default: docs/public), resolves each internal href
(relative, or absolute under the /saffron-engine/ baseURL), and reports targets
that do not exist. Run `hugo --gc` in docs/ first.

Usage: python3 check_links.py [public-dir]
Exits non-zero if any broken link is found.
"""

import glob
import os
import re
import sys
from urllib.parse import urldefrag

BASEURL_PATH = "/saffron-anima"
SKIP_SUFFIXES = (".css", ".js", ".png", ".svg", ".woff", ".woff2", ".ico",
                 ".xml", ".json", ".txt", ".map")
HREF = re.compile(r'href="([^"]+)"')


def exists(target: str) -> bool:
    if os.path.isdir(target):
        return os.path.exists(os.path.join(target, "index.html"))
    return os.path.exists(target)


def main() -> int:
    root = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "docs/public")
    if not os.path.isdir(root):
        print(f"error: {root} not found — run `hugo --gc` in docs/ first")
        return 2

    broken: dict[str, set[str]] = {}
    pages = glob.glob(root + "/**/*.html", recursive=True)
    checked = 0

    for html in pages:
        page_dir = os.path.dirname(html)
        text = open(html, encoding="utf-8", errors="ignore").read()
        for raw in HREF.findall(text):
            url = urldefrag(raw)[0].split("?")[0]
            if not url or url.startswith(("http", "//", "mailto:", "data:", "javascript:")):
                continue
            if url.lower().endswith(SKIP_SUFFIXES) or "/katex/" in url:
                continue
            if url.startswith("/"):
                path = url[len(BASEURL_PATH):] if url.startswith(BASEURL_PATH + "/") else url
                target = os.path.normpath(root + path)
            else:
                target = os.path.normpath(os.path.join(page_dir, url))
            checked += 1
            if not exists(target):
                broken.setdefault(os.path.relpath(html, root), set()).add(raw)

    total = sum(len(v) for v in broken.values())
    print(f"checked {checked} internal links across {len(pages)} pages")
    print(f"BROKEN LINKS: {total if total else 'none'}")
    for src in sorted(broken):
        for href in sorted(broken[src]):
            print(f"  {src} -> {href}")
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main())
