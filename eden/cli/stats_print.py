#!/usr/bin/env python3
#
# Copyright (c) 2004-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

# Helper function to print the heading of a Stat Call.

from typing import TextIO


def write_heading(heading: str, out: TextIO) -> None:
    border = "*" * len(heading)
    out.write(_center_strip_right(border, 80))
    out.write(_center_strip_right(heading, 80))
    out.write(_center_strip_right(border, 80) + "\n")


def write_mem_status_table(fuse_counters, out: TextIO) -> None:
    format_str = "{:>40} {:^1} {:<20}"
    keys = [
        "memory_free",
        "memory_free_percent",
        "memory_usage",
        "memory_usage_percent",
    ]
    for key in keys:
        if key.endswith("_percent"):
            value = "%d%s" % (fuse_counters[key], "%")
        else:
            value = "%f(GB)" % (fuse_counters[key] / (10 ** 6))
        centered_text = format_str.format(key.replace("_", " "), ":", value)
        out.write(centered_text.rstrip() + "\n")


LATENCY_FORMAT_STR = "{:<12} {:^4} {:^10}  {:>10}  {:>15}  {:>10} {:>10}\n"


# Prints a record of latencies with 50'th,90'th and 99'th percentile.
def write_latency_record(syscall: str, matrix, out: TextIO) -> None:
    border = "-" * 80
    percentile = {0: "p50", 1: "p90", 2: "p99"}

    for i in range(len(percentile)):
        syscall_name = ""
        if i == int(len(percentile) / 2):
            syscall_name = syscall
        out.write(
            LATENCY_FORMAT_STR.format(
                syscall_name,
                "|",
                percentile[i],
                matrix[i][0],
                matrix[i][1],
                matrix[i][2],
                matrix[i][3],
            )
        )
    out.write(border + "\n")


def write_latency_table(table, out: TextIO) -> None:
    out.write(
        LATENCY_FORMAT_STR.format(
            "SystemCall",
            "|",
            "Percentile",
            "Last Minute",
            "Last 10 Minutes",
            "Last Hour",
            "All Time",
        )
    )
    border = "-" * 80
    out.write(border + "\n")
    for key in table:
        write_latency_record(key, table[key], out)


def write_table(table, heading: str, out: TextIO) -> None:
    key_width = max([len(heading)] + list(map(len, table.keys()))) + 2

    format_str = "{:<{}}{:>15}{:>15}{:>15}{:>15}\n"
    out.write(
        format_str.format(
            heading, key_width, "Last Minute", "Last 10m", "Last Hour", "All Time"
        )
    )
    border = "-" * (key_width + 60)
    out.write(border + "\n")
    for key in table:
        value = table[key]
        out.write(
            format_str.format(key, key_width, value[0], value[1], value[2], value[3])
        )


def _center_strip_right(text: str, width: int) -> str:
    """Returns a string with sufficient leading whitespace such that `text`
    would be centered within the specified `width` plus a trailing newline."""
    space = (width - len(text)) // 2
    return space * " " + text + "\n"


def format_size(size: int) -> str:
    if size > 1000000000:
        return "{:.1f} GB".format(size / 1000000000)
    if size > 1000000:
        return "{:.1f} MB".format(size / 1000000)
    if size > 1000:
        return "{:.1f} KB".format(size / 1000)
    if size > 0:
        return "{} B".format(size)
    return "0"
