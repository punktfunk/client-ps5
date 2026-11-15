#!/usr/bin/env python3
"""Fills packaging/sce_sys/param.json for one title: render-param.py TEMPLATE TITLE_ID NAME VERSION.

attribute3 0x80040 declares 119.88 Hz output (and the newer VRR bit); a console that rejects it
for VRR keeps HFR. downloadDataSize 256 gives the title /download0. The concept id is the title
id's five digits, which is what the console checks it against. contentVersion is what a console
or a homebrew store compares to see an update, so it rises with VERSION.
"""
import json
import re
import sys


def content_version(version: str) -> str:
    """`0.1.2` as `01.001.002`. The major is one up: 0.1.0 shipped as `01.000.000`, and a
    content version never goes down. A version with no three numbers reads as 0.0.0."""
    m = re.match(r"v?(\d+)\.(\d+)\.(\d+)", version)
    major, minor, patch = (int(g) for g in m.groups()) if m else (0, 0, 0)
    if major > 98 or minor > 999 or patch > 999:
        raise ValueError(f"version {version!r} does not fit a content version")
    return f"{major + 1:02d}.{minor:03d}.{patch:03d}"


assert content_version("0.1.2") == "01.001.002" > "01.000.000"
assert content_version("v1.20.3+git.abc") == "02.020.003"
assert content_version("+git.unknown") == "01.000.000"


def main() -> int:
    template, title_id, name, version = sys.argv[1:5]
    if not re.fullmatch(r"PPSA\d{5}", title_id):
        print(f"title id {title_id!r} is not PPSA12345-shaped", file=sys.stderr)
        return 2
    text = open(template, encoding="utf-8").read()
    text = text.replace("@TITLE_ID@", title_id).replace("@CONCEPT_ID@", title_id[4:])
    text = text.replace("@TITLE_NAME@", name)
    doc = json.loads(text)
    doc["contentVersion"] = content_version(version)
    if not re.fullmatch(r"[A-Z]{2}\d{4}-PPSA\d{5}_00-[A-Z0-9]{16}", doc["contentId"]):
        print(f"contentId {doc['contentId']!r} is malformed", file=sys.stderr)
        return 2
    print(json.dumps(doc, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
