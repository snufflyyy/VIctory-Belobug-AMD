#!/usr/bin/env python3
"""Summarize VIctory's periodic world-frame samples, not individual frames."""

import argparse
from pathlib import Path
import re
import statistics

ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
TIMING = re.compile(
    r"(?P<frame>[\d.]+) ms/frame \((?P<prepare>[\d.]+) preparing: "
    r"copies (?P<copy>[\d.]+) ms [\d.]+ MB, textures (?P<texture>[\d.]+) ms "
    r".*?; (?P<gpu>[\d.]+) on the GPU; (?P<back>[\d.]+) copying back .*?\), "
    r"(?P<draws>\d+) draws/frame, dropped (?P<dropped>\d+) without a host pipeline "
    r"\((?P<pending>\d+) being made\) and (?P<indirect>\d+) indirect"
)


def read_log(path, minimum, maximum):
    samples = []
    cache = []
    with path.open(errors="replace") as log:
        for raw in log:
            line = ANSI.sub("", raw)
            if any(marker in line for marker in (
                "shaders from the cache and translated", "host pipeline library loaded",
                "host pipeline library saved", "shader cache keyed by translator",
            )):
                cache.append(line.strip().split(": ", 1)[-1])
            match = TIMING.search(line)
            if match and minimum <= int(match["draws"]) <= maximum:
                samples.append({key: float(value) for key, value in match.groupdict().items()})
    return samples, cache


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", nargs="+", type=Path)
    parser.add_argument("--min-draws", type=int, default=1500)
    parser.add_argument("--max-draws", type=int, default=8000)
    args = parser.parse_args()
    for path in args.logs:
        samples, cache = read_log(path, args.min_draws, args.max_draws)
        print(f"{path}: {len(samples)} samples ({args.min_draws}–{args.max_draws} draws/frame)")
        if samples:
            median = {key: statistics.median(s[key] for s in samples) for key in samples[0]}
            print(f"  {median['frame']:.1f} ms/frame ({1000 / median['frame']:.2f} FPS); "
                  f"prepare {median['prepare']:.1f} ms (copy {median['copy']:.1f}, "
                  f"texture {median['texture']:.1f}); GPU {median['gpu']:.1f} ms; "
                  f"copy-back {median['back']:.1f} ms")
            print(f"  draws {median['draws']:g}; dropped {median['dropped']:g}; "
                  f"compiling {median['pending']:g}; indirect drops {median['indirect']:g}")
        for marker in dict.fromkeys(cache[:3] + cache[-3:]):
            print(f"  {marker}")


if __name__ == "__main__":
    main()
