#!/usr/bin/env python3
"""Lays out a build in dist/: unpack.py ZIP.

ZIP is CI's `title` artifact (title zips, their checksums and ELFs) or one title zip. Each title
zip is checked against the .sha256 beside it, then its PPSA folder replaces dist/<TITLE_ID>/.
"""
import hashlib
import os
import re
import shutil
import sys
import zipfile

DIST = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", "dist"))
TITLE = re.compile(r"PPSA\d{5}")


def unpack_title(path: str) -> None:
    name = os.path.basename(path)
    sums = path + ".sha256"
    if os.path.isfile(sums):
        with open(sums, encoding="utf-8") as f:
            want = f.read().split()[0]
        with open(path, "rb") as f:
            got = hashlib.sha256(f.read()).hexdigest()
        if want != got:
            sys.exit(f"{name} is damaged: its checksum doesn't match. Download the build again.")
    with zipfile.ZipFile(path) as z:
        # Folders only: the license text and notices sit beside the title folder.
        tops = {n.split("/")[0] for n in z.namelist() if "/" in n}
        if len(tops) != 1 or not TITLE.fullmatch(next(iter(tops))):
            sys.exit(f"{name} isn't a title zip: it should hold one PPSA folder.")
        title_id = tops.pop()
        shutil.rmtree(os.path.join(DIST, title_id), ignore_errors=True)
        z.extractall(DIST, members=[n for n in z.namelist() if n.startswith(title_id + "/")])
    print(f"dist/{title_id} ready")


def main() -> int:
    if len(sys.argv) != 2:
        sys.exit("usage: unpack.py ZIP")
    os.makedirs(DIST, exist_ok=True)
    try:
        build = zipfile.ZipFile(sys.argv[1])
    except (OSError, zipfile.BadZipFile) as e:
        sys.exit(f"Couldn't open {sys.argv[1]} as a zip — {e}")
    with build as z:
        inner = [n for n in z.namelist() if re.fullmatch(r"PPSA\d{5}\.zip", n)]
        if not inner:
            unpack_title(sys.argv[1])
            return 0
        names = [n for n in z.namelist() if "/" not in n]
        z.extractall(DIST, members=names)
    for n in inner:
        unpack_title(os.path.join(DIST, n))
    return 0


if __name__ == "__main__":
    sys.exit(main())
