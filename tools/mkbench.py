#!/usr/bin/env python3
"""Create a folder with N empty dataset-like files: mkbench.py DIR [N]. Run Kolphin on it with KOLPHIN_BENCH=1."""
import os
import sys

root, n = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 1_000_000
os.makedirs(root, exist_ok=True)
exts = [".jpg", ".png", ".txt", ".json"]
for i in range(n):
    os.close(os.open(os.path.join(root, f"img_{i}{exts[i % len(exts)]}"), os.O_CREAT | os.O_WRONLY, 0o644))
for d in ("annotations", "labels", "src"):
    os.makedirs(os.path.join(root, d), exist_ok=True)
print(f"{n} files in {root}")
