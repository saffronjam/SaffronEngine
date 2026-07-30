#!/usr/bin/env python3
"""Style-check docs source against the SaffronEngine docs house rules.

Scans markdown under docs/content (or the given paths) and reports:

ERRORS (exit 1):
  - banned code-state / roadmap / change-journey language (timeless-present rule)
  - references to plans/, phases, or plan-session vocabulary
  - front-matter title != body H1, missing H1, or multiple H1s

WARNINGS (exit 0, must be triaged):
  - suspect status/journey phrasing that needs human judgment
  - paragraphs over 90 words
  - explanation/how-to/tutorial leaf pages with no concrete example
    (no code fence, no display math, no mermaid block)

Code fences are stripped before phrase checks; a small allowlist covers the
technical senses of "deferred" etc. Extend ALLOW when a legitimate phrase trips.

Usage: python3 check_style.py [paths...]   (default: docs/content)
"""

import glob
import os
import re
import sys

# Legitimate technical phrases that would otherwise trip a banned pattern.
ALLOW = re.compile(
    r"deferred[ -](rendering|shading|g-buffer|window|capture|visibility|"
    r"structural|execution|work)"
    r"|deferred to the (end|next)"
    r"|deferred-work seam"
    r"|visibility is deferred"
    r"|(work )?deferred from pass"
    r"|planned cell"
    r"|awaiting a rebuild",
    re.IGNORECASE,
)

# Banned outright: status, version-stage, plan-session, change-journey framing.
BANNED = [
    (r"\bcurrently\b", "status framing"),
    (r"\bnot\s+yet\b", "status framing"),
    (r"\bfor\s+now\b", "status framing"),
    (r"\bat\s+the\s+moment\b", "status framing"),
    (r"\bas\s+of\s+(now|today|writing)\b", "status framing"),
    (r"\bdeferred\b", "roadmap 'deferred' (allowlist technical senses)"),
    (r"\bplanned\b", "roadmap framing"),
    (r"\broadmap\b", "roadmap framing"),
    (r"\bfollow-?ups?\b", "roadmap framing"),
    (r"\bout\s+of\s+scope\b", "roadmap framing"),
    (r"\bnon-goals?\b", "roadmap framing"),
    (r"\bawaiting\b", "status framing"),
    (r"\bwill\s+be\s+(added|built|implemented|extended|supported|applied)\b",
     "future promise"),
    (r"\bremaining\s+(step|work|packaging)\b", "future work"),
    (r"\bnot\s+(yet\s+)?implemented\b", "status framing"),
    (r"\bstill\s+(missing|awaiting|can'?t|cannot)\b", "status framing"),
    (r"\bv[12]\b", "version-stage framing (v1/v2)"),
    (r"\bphase[- ]?\d\b", "plan-phase reference"),
    (r"\bthis\s+phase\b", "plan-phase reference"),
    (r"\blater\s+(step|stage|phase|refinement|extension|upgrade|addition)\b",
     "roadmap framing"),
    (r"\bendgame\b", "trajectory framing"),
    (r"\bon-ramp\b", "trajectory framing"),
    (r"\breserved\s+stages?\b", "reserved-for-future framing"),
    (r"\bseam\s+left\b|\bleft\s+as\s+the\s+seam\b", "future-seam framing"),
    (r"\bwhen\s+it\s+gains\b", "future framing"),
    (r"\bonce\s+\S[^.\n]{0,50}\s+lands\b", "future framing"),
    (r"\bplans/", "reference to plans/ folder"),
    (r"\block(ed)?\s+decision\b", "plan-session vocabulary"),
    (r"\blanded\s+together\b|\ball\s+landed\b", "delivery narration"),
    (r"\bwired\s+end\s+to\s+end\b", "milestone narration"),
    (r"\bpreviously\b", "change-journey"),
    (r"\bused\s+to\s+(be|compute|fire|have|do)\b", "change-journey"),
    (r"\bno\s+longer\b", "change-journey"),
    (r"\banymore\b", "change-journey"),
    (r"\bnow\s+that\b", "change-journey"),
    (r"\bthe\s+old\s+\S+\s+(path|toggle|behaviou?r|way|system|code|prefix)\b",
     "change-journey"),
    (r"\b(it|they)\s+replaced\b|\breplaces\s+the\s+old\b", "change-journey"),
    (r"\bhistorical\s+look\b", "change-journey"),
    (r"\blegacy\b", "legacy reference"),
    (r"\bcompat(ibility)?\s+(alias|shim|path)\b", "compat-shim reference"),
    (r"\bthe\s+whole\s+time\b", "codebase-history narration"),
    (r"\bcoding\s+session\b", "session reference"),
]

# Needs human judgment: often fine, often a smell. Reported as warnings.
SUSPECT = [
    (r"\bis\s+now\b|\bare\s+now\b", "possible change-journey 'now'"),
    (r"\bwaits?\s+on\b", "possible status framing (vs. GPU-sync sense)"),
    (r"\breserved\s+for\b", "possible reserved-for-future framing"),
    (r"\b(is|are)\s+gone\b", "possible change-journey"),
    (r"\balready\s+(had|has|exists?|skins|carries)\b",
     "possible build-history framing"),
    (r"\bcomes?\s+for\s+free\b", "promotional"),
    (r"\bvisually\s+(lossless|free)\b", "unqualified quality claim"),
    (r"\bcrucial(ly)?\b", "editorializing intensifier"),
    (r"—[^—\n]*—", "two em dashes in one line"),
    (r"\b(serves|stands|acts|functions)\s+as\b", "copula avoidance"),
    (r"\bin\s+order\s+to\b", "filler phrase"),
    (r"\bit\s+is\s+(important|worth)\s+(to\s+note|noting)\b", "filler phrase"),
    (r"\b(pivotal|seamless(ly)?|testament|delve|leverag(e|es|ing))\b", "AI-vocabulary filler"),
    (r",\s+(ensuring|highlighting|underscoring|showcasing|emphasizing)\s", "trailing -ing analysis clause"),
    (r"\bcarr(y|ies)\s+(weight|intent)\b", "rhetorical tic"),
    (r"\bload-bearing\b", "rhetorical tic"),
    (r"\bnot\s+a\s+duplicate\b|\bboth\s+stay\b", "design-defense voice"),
    (r"\bthe\s+engine'?s\s+first\b", "milestone framing"),
    (r"\bfar\s+better\s+than\b", "promotional comparison"),
]

FRONT = re.compile(r"\A\+\+\+\n(.*?)\n\+\+\+\n", re.S)
TITLE = re.compile(r"^title\s*=\s*(['\"])(.*?)\1\s*(?:#.*)?$", re.M)
FENCE = re.compile(r"^```.*?^```\s*?$", re.S | re.M)
PARA_WORD_LIMIT = 90

errors: list[str] = []
warnings: list[str] = []


def line_of(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def check_file(path: str, rel: str) -> None:
    raw = open(path, encoding="utf-8").read()

    fm = FRONT.match(raw)
    body = raw[fm.end():] if fm else raw
    body_offset = raw[: fm.end()].count("\n") if fm else 0

    masked = FENCE.sub(lambda m: "\n" * m.group(0).count("\n"), body)
    h1s = re.findall(r"^# (.+)$", masked, re.M)
    if fm:
        tm = TITLE.search(fm.group(1))
        title = tm.group(2) if tm else None
        if not h1s:
            errors.append(f"{rel}: no body H1 (front-matter title is not rendered)")
        elif len(h1s) > 1:
            errors.append(f"{rel}: {len(h1s)} H1 headings (must be exactly one)")
        elif title is not None and h1s[0].strip() != title.strip():
            errors.append(f"{rel}: title {title!r} != H1 {h1s[0].strip()!r}")

    # Phrase checks run on prose only: fenced code is already masked above.
    prose = masked
    for line_no, line in enumerate(prose.splitlines(), start=body_offset + 1):
        if ALLOW.search(line):
            stripped = ALLOW.sub("", line)
        else:
            stripped = line
        for pattern, label in BANNED:
            m = re.search(pattern, stripped, re.IGNORECASE)
            if m:
                errors.append(
                    f"{rel}:{line_no}: BANNED [{label}] …{m.group(0)}… | {line.strip()[:100]}")
        for pattern, label in SUSPECT:
            m = re.search(pattern, stripped, re.IGNORECASE)
            if m:
                warnings.append(
                    f"{rel}:{line_no}: suspect [{label}] …{m.group(0)}… | {line.strip()[:100]}")

    # Paragraph budget: plain prose blocks only.
    for m in re.finditer(r"(?:^|\n\n)([^\n|#>\-*+!`$\s][^\n]*(?:\n(?![\n|#>\-*+])[^\n]*)*)",
                         prose):
        para = m.group(1).strip()
        words = len(para.split())
        if words > PARA_WORD_LIMIT:
            warnings.append(
                f"{rel}:{line_of(prose, m.start(1)) + body_offset}: "
                f"paragraph of {words} words (budget {PARA_WORD_LIMIT})")

    # Example requirement for leaf content pages.
    base = os.path.basename(rel)
    is_leaf = base != "_index.md" and base not in ("overview.md",)
    in_scope = any(s in rel for s in ("explanations/", "how-to/", "tutorials/"))
    if is_leaf and in_scope:
        has_example = ("```" in body) or ("$$" in body) or re.search(r"`sa \w", body)
        if not has_example:
            warnings.append(
                f"{rel}: no concrete example (no code fence, display math, or `sa` command)")


def main() -> int:
    roots = sys.argv[1:] or ["docs/content"]
    files: list[str] = []
    for root in roots:
        if os.path.isfile(root):
            files.append(root)
        else:
            files.extend(glob.glob(os.path.join(root, "**", "*.md"), recursive=True))
    if not files:
        print(f"error: no markdown files under {roots}")
        return 2

    common = os.path.commonpath([os.path.abspath(f) for f in files]) if len(files) > 1 \
        else os.path.dirname(os.path.abspath(files[0]))
    for f in sorted(files):
        check_file(f, os.path.relpath(os.path.abspath(f), common))

    print(f"checked {len(files)} pages")
    print(f"ERRORS: {len(errors)}")
    for e in errors:
        print(f"  {e}")
    print(f"WARNINGS: {len(warnings)}")
    for w in warnings:
        print(f"  {w}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
