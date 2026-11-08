#!/usr/bin/env python3
"""
Benchmark harness for UMAY against other toolchains.

Each workload is written once per language, computes the same value and prints
it, so correctness is checked before any timing is believed: a build time means
nothing if the program is wrong.

Every command is repeated and the median is reported. Wall-clock is what a user
waits for and feels; a single run measures the machine's mood, not the
toolchain. Peak RSS comes from wait4 on the child that was spawned, not from
RUSAGE_CHILDREN, so it belongs to the command being measured.

Usage:
    scripts/bench.py                 # run everything, print tables
    scripts/bench.py --json          # machine-readable
    scripts/bench.py --only compute  # one workload (substring match)
"""

import json
import os
import re
import shutil
import statistics
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UMAY = os.path.join(ROOT, "target", "release", "umay")

# Repetitions. Build times are noisier than run times, so both get several
# samples and a median; the min is reported too, because the min is the least
# polluted by other work on the machine.
BUILD_REPS = 5
RUN_REPS = 7


# ---------------------------------------------------------------------------
# Measuring one command.
# ---------------------------------------------------------------------------

def _run_once(cmd, cwd, env):
    """Spawns `cmd` in `cwd`; returns (wall_s, exit_code, peak_rss_kb, stdout, stderr).

    A fork/exec is used rather than posix_spawn because posix_spawn cannot set
    the working directory (its Python binding has no cwd argument), and `umay`
    writes `dist/` relative to the working directory. wait4 reports the peak RSS
    of *this* child, whereas RUSAGE_CHILDREN is a high-water mark across every
    child the process ever had and would attribute the largest build to all of
    them.
    """
    out_r, out_w = os.pipe()
    err_r, err_w = os.pipe()
    child_env = env if env is not None else os.environ
    start = time.monotonic()
    pid = os.fork()
    if pid == 0:
        # Child: never touch Python's buffers again, exec on every path.
        try:
            os.dup2(out_w, 1)
            os.dup2(err_w, 2)
            os.close(out_r)
            os.close(err_r)
            os.close(out_w)
            os.close(err_w)
            os.chdir(cwd)
            os.execvpe(cmd[0], cmd, child_env)
        except BaseException:
            os._exit(127)
    os.close(out_w)
    os.close(err_w)

    # Read both pipes until closed. The child writes a few lines and exits, so
    # a select loop over the two read ends is enough.
    import select
    chunks = {out_r: bytearray(), err_r: bytearray()}
    open_fds = [out_r, err_r]
    while open_fds:
        ready, _, _ = select.select(open_fds, [], [])
        for fd in ready:
            data = os.read(fd, 65536)
            if not data:
                open_fds.remove(fd)
                os.close(fd)
            else:
                chunks[fd] += data
    _, status, usage = os.wait4(pid, 0)
    elapsed = time.monotonic() - start
    code = os.waitstatus_to_exitcode(status)
    return (elapsed, code, usage.ru_maxrss,
            chunks[out_r].decode(errors="replace"),
            chunks[err_r].decode(errors="replace"))


def measure(cmd, cwd, env=None, reps=RUN_REPS):
    """Runs `cmd` `reps` times; returns a dict of timings and RSS."""
    times = []
    rss = 0
    code = None
    last_stdout = ""
    last_stderr = ""
    for _ in range(reps):
        wall, code, kb, out, err = _run_once(cmd, cwd, env)
        times.append(wall)
        rss = max(rss, kb)  # peak over runs; equal runs should give one value
        last_stdout, last_stderr = out, err
    return {
        "median_s": statistics.median(times),
        "min_s": min(times),
        "max_s": max(times),
        "rss_kb": rss,
        "exit_code": code,
        "stdout": last_stdout,
        "stderr": last_stderr,
    }


def have(tool):
    return shutil.which(tool) is not None


def version_of(tool):
    """First version line a toolchain prints, or None."""
    for flags in (["--version"], ["version"]):
        try:
            out = shutil.which(tool)
            if not out:
                return None
            import subprocess
            p = subprocess.run([out] + flags, capture_output=True, text=True)
            if p.returncode == 0 and p.stdout.strip():
                return p.stdout.strip().splitlines()[0]
        except FileNotFoundError:
            return None
    return None


# ---------------------------------------------------------------------------
# Workload sources. One per (workload, language). Each prints one integer with
# the same value in every language, so a mismatch is obvious.
# ---------------------------------------------------------------------------

# Zig 0.17's std.debug.print costs ~24s to compile and writes to stderr, so
# every Zig workload prints through libc's write instead. That is the cheap,
# honest way to get the same one line out.
ZIG_PRELUDE = r'''const std = @import("std");
extern "c" fn write(fd: c_int, buf: [*]const u8, n: usize) isize;
fn printI64(v: i64) void {
    var buf: [32]u8 = undefined;
    const s = std.fmt.bufPrint(&buf, "{}\n", .{v}) catch return;
    _ = write(1, s.ptr, s.len);
}
'''


# --- call chain: 1000 functions, each calling the previous ------------------
def gen_call_chain(n=1000):
    umay = []
    for i in range(n):
        if i == 0:
            umay.append(f"fn f{i}(x: i64) -> i64 {{ return x + {i} }}")
        else:
            umay.append(f"fn f{i}(x: i64) -> i64 {{ return (f{i-1}(x * 2 + {i})) % 100003 }}")
    umay += [
        "fn main() -> i32 {",
        f"  let r: i64 = f{n-1}(1)",
        "  println_i64(r)",
        "  return 0",
        "}",
    ]

    rust = []
    for i in range(n):
        if i == 0:
            rust.append(f"fn f{i}(x: i64) -> i64 {{ x + {i} }}")
        else:
            rust.append(f"fn f{i}(x: i64) -> i64 {{ (f{i-1}(x * 2 + {i})) % 100003 }}")
    rust += ["fn main() {", f"    let r: i64 = f{n-1}(1);", '    println!("{}", r);', "}"]

    c = ["#include <stdio.h>"]
    for i in range(n):
        if i == 0:
            c.append(f"long f{i}(long x) {{ return x + {i}; }}")
        else:
            c.append(f"long f{i}(long x) {{ return (f{i-1}(x * 2 + {i})) % 100003; }}")
    c += ["int main(void) {", f"    long r = f{n-1}(1);", '    printf("%ld\\n", r);', "    return 0;", "}"]

    zig = [ZIG_PRELUDE]
    for i in range(n):
        if i == 0:
            zig.append(f"fn call{i}(x: i64) i64 {{ return x + {i}; }}")
        else:
            zig.append(f"fn call{i}(x: i64) i64 {{ return @mod(call{i-1}(x * 2 + {i}), 100003); }}")
    zig += ["pub fn main() void {", f"    printI64(call{n-1}(1));", "}"]

    go = ["package main", "", 'import "fmt"', ""]
    for i in range(n):
        if i == 0:
            go.append(f"func f{i}(x int64) int64 {{ return x + {i} }}")
        else:
            go.append(f"func f{i}(x int64) int64 {{ return (f{i-1}(x*2+{i})) % 100003 }}")
    go += ["", "func main() {", f"\tfmt.Println(f{n-1}(1))", "}"]

    return {
        "umay": "\n".join(umay) + "\n",
        "rust": "\n".join(rust) + "\n",
        "c": "\n".join(c) + "\n",
        "zig": "\n".join(zig) + "\n",
        "go": "\n".join(go) + "\n",
    }


WORKLOADS = {"callchain": gen_call_chain(1000)}


# --- compute: integer-heavy loop --------------------------------------------
WORKLOADS["compute"] = {
    "umay": """
fn main() -> i32 {
  let mut i: i64 = 0
  let mut acc: i64 = 0
  while i < 50000000 {
    acc = (acc + i * 3 + 7) % 1000003
    i = i + 1
  }
  println_i64(acc)
  return 0
}
""",
    "rust": """
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 50_000_000 {
        acc = (acc + i * 3 + 7) % 1_000_003;
        i += 1;
    }
    println!("{}", acc);
}
""",
    "c": """
#include <stdio.h>
int main(void) {
    long acc = 0, i = 0;
    while (i < 50000000) {
        acc = (acc + i * 3 + 7) % 1000003;
        i++;
    }
    printf("%ld\\n", acc);
    return 0;
}
""",
    "zig": ZIG_PRELUDE + """
pub fn main() void {
    var acc: i64 = 0;
    var i: i64 = 0;
    while (i < 50_000_000) : (i += 1) {
        acc = @mod(acc + i * 3 + 7, 1_000_003);
    }
    printI64(acc);
}
""",
    "go": """
package main
import "fmt"
func main() {
    var acc int64 = 0
    for i := int64(0); i < 50_000_000; i++ {
        acc = (acc + i*3 + 7) % 1_000_003
    }
    fmt.Println(acc)
}
""",
}


# --- alloc: many small heap allocations -------------------------------------
WORKLOADS["alloc"] = {
    "umay": """
fn main() -> i32 {
  let mut i: i64 = 0
  let mut total: i64 = 0
  while i < 2000000 {
    let p = alloc(8) as *mut i64
    unsafe { *p = i }
    total = (total + unsafe { *p }) % 1000003
    dealloc(p as *mut u8)
    i = i + 1
  }
  println_i64(total)
  return 0
}
""",
    "rust": """
use std::alloc::{alloc, dealloc, Layout};
fn main() {
    let layout = Layout::from_size_align(8, 8).unwrap();
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 2_000_000 {
        unsafe {
            let p = alloc(layout) as *mut i64;
            *p = i;
            total = (total + *p) % 1_000_003;
            dealloc(p as *mut u8, layout);
        }
        i += 1;
    }
    println!("{}", total);
}
""",
    "c": """
#include <stdio.h>
#include <stdlib.h>
int main(void) {
    long total = 0;
    for (long i = 0; i < 2000000; i++) {
        long *p = malloc(8);
        *p = i;
        total = (total + *p) % 1000003;
        free(p);
    }
    printf("%ld\\n", total);
    return 0;
}
""",
    "zig": ZIG_PRELUDE + """
const allocator = std.heap.c_allocator;
pub fn main() void {
    var total: i64 = 0;
    var i: i64 = 0;
    while (i < 2_000_000) : (i += 1) {
        const p = allocator.alloc(i64, 1) catch unreachable;
        p[0] = i;
        total = @mod(total + p[0], 1_000_003);
        allocator.free(p);
    }
    printI64(total);
}
""",
    "go": """
package main
import "fmt"
func main() {
    var total int64 = 0
    for i := int64(0); i < 2_000_000; i++ {
        p := new(int64)
        *p = i
        total = (total + *p) % 1_000_003
    }
    fmt.Println(total)
}
""",
}


# --- fib: recursive call overhead -------------------------------------------
WORKLOADS["fib"] = {
    "umay": """
fn fib(n: i64) -> i64 {
  if n < 2 { return n }
  return fib(n - 1) + fib(n - 2)
}
fn main() -> i32 {
  println_i64(fib(38))
  return 0
}
""",
    "rust": """
fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
fn main() { println!("{}", fib(38)); }
""",
    "c": """
#include <stdio.h>
long fib(long n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
int main(void) { printf("%ld\\n", fib(38)); return 0; }
""",
    "zig": ZIG_PRELUDE + """
fn fib(n: i64) i64 { return if (n < 2) n else fib(n - 1) + fib(n - 2); }
pub fn main() void { printI64(fib(38)); }
""",
    "go": """
package main
import "fmt"
func fib(n int64) int64 { if n < 2 { return n }; return fib(n-1) + fib(n-2) }
func main() { fmt.Println(fib(38)) }
""",
}


# --- str: a string built in a loop (allocation + copies) --------------------
WORKLOADS["str"] = {
    "rust": r'''
fn main() {
    let mut s = String::new();
    let mut i: i64 = 0;
    while i < 1_000_000 {
        s.push_str("abcdefghij");
        i += 1;
    }
    println!("{}", s.len() % 1_000_003);
}
''',
    "c": r'''
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
int main(void) {
    size_t cap = 1000000 * 10 + 1;
    char *s = malloc(cap);
    size_t len = 0;
    for (long i = 0; i < 1000000; i++) {
        memcpy(s + len, "abcdefghij", 10);
        len += 10;
    }
    printf("%ld\n", (long)(len % 1000003));
    free(s);
    return 0;
}
''',
    "go": r'''
package main
import "fmt"
func main() {
    var s []byte
    for i := 0; i < 1_000_000; i++ {
        s = append(s, "abcdefghij"...)
    }
    fmt.Println(len(s) % 1_000_003)
}
''',
    "zig": ZIG_PRELUDE + r'''
pub fn main() void {
    const n: usize = 1_000_000;
    const chunk = "abcdefghij";
    const buf = std.heap.c_allocator.alloc(u8, n * chunk.len) catch unreachable;
    defer std.heap.c_allocator.free(buf);
    var i: usize = 0;
    while (i < n) : (i += 1) {
        @memcpy(buf[i * chunk.len ..][0..chunk.len], chunk);
    }
    printI64(@intCast(@mod(buf.len, 1_000_003)));
}
''',
}


# ---------------------------------------------------------------------------
# Build commands.
# ---------------------------------------------------------------------------

def compile_command(lang, src, out):
    if lang == "rust":
        return ["rustc", "-O", "-o", out, src]
    if lang == "c-gcc":
        return ["gcc", "-O2", "-o", out, src]
    if lang == "c-clang":
        return ["clang", "-O2", "-o", out, src]
    if lang == "zig":
        return ["zig", "build-exe", "-O", "ReleaseFast", "-lc", "-femit-bin=" + out, src]
    if lang == "go":
        return ["go", "build", "-o", out, src]
    raise ValueError(lang)


# One source, several toolchains: C is built by gcc and by clang.
LANG_TOOLS = {
    "umay": ["umay"],
    "rust": ["rust"],
    "c": ["c-gcc", "c-clang"],
    "zig": ["zig"],
    "go": ["go"],
}

TOOL_FOR = {"rust": "rustc", "c-gcc": "gcc", "c-clang": "clang", "zig": "zig", "go": "go"}
EXT_FOR = {"umay": "um", "rust": "rs", "c": "c", "zig": "zig", "go": "go"}


def build_umay(srcdir, src):
    """Runs `umay build` from a clean cache; returns timing dict and exe path."""
    cache = os.path.join(srcdir, "cache")
    times = []
    rss = 0
    last = None
    env = dict(os.environ)
    env["UMAY_CACHE_DIR"] = cache
    for _ in range(BUILD_REPS):
        shutil.rmtree(cache, ignore_errors=True)
        shutil.rmtree(os.path.join(srcdir, "dist"), ignore_errors=True)
        os.makedirs(cache, exist_ok=True)
        last = _run_once([UMAY, "build", src], srcdir, env)
        times.append(last[0])
        rss = max(rss, last[2])
        if last[1] != 0:
            break
    exe = os.path.join(srcdir, "dist", "prog")
    ok = last[1] == 0 and os.path.exists(exe)
    return {
        "build_ok": ok,
        "build_median_s": statistics.median(times) if ok else None,
        "build_min_s": min(times) if ok else None,
        "build_rss_kb": rss,
        "binary_bytes": os.path.getsize(exe) if ok else None,
        "build_error": None if ok else (last[4][:400] if last else "no run"),
    }, (exe if ok else None)


def build_other(lang, srcdir, src):
    exe = os.path.join(srcdir, "prog")
    cmd = compile_command(lang, src, exe)
    times = []
    rss = 0
    last = None
    for _ in range(BUILD_REPS):
        if os.path.exists(exe):
            os.remove(exe)
        last = _run_once(cmd, srcdir, None)
        times.append(last[0])
        rss = max(rss, last[2])
        if last[1] != 0:
            break
    ok = last[1] == 0 and os.path.exists(exe)
    return {
        "build_ok": ok,
        "build_median_s": statistics.median(times) if ok else None,
        "build_min_s": min(times) if ok else None,
        "build_rss_kb": rss,
        "binary_bytes": os.path.getsize(exe) if ok else None,
        "build_error": None if ok else (last[4][:400] if last else "no run"),
    }, (exe if ok else None)


def last_integer(*texts):
    """The last integer across several outputs; Zig may print to stderr."""
    for t in texts:
        nums = re.findall(r"-?\d+", t)
        if nums:
            return nums[-1]
    return None


def main():
    json_out = "--json" in sys.argv
    only = None
    if "--only" in sys.argv:
        only = sys.argv[sys.argv.index("--only") + 1]

    results = {"toolchains": {}, "workloads": {}}
    for name, tool in [("umay", UMAY), ("rustc", "rustc"), ("gcc", "gcc"),
                       ("clang", "clang"), ("zig", "zig"), ("go", "go")]:
        v = None
        if name == "umay":
            v = "umay 0.1.0" if os.path.exists(UMAY) else None
        elif have(tool):
            v = version_of(tool)
        results["toolchains"][name] = v

    workdir = tempfile.mkdtemp(prefix="umay_bench_")
    try:
        for wname, sources in WORKLOADS.items():
            if only and only not in wname:
                continue
            results["workloads"][wname] = {}
            for src_lang, source in sources.items():
                for lang in LANG_TOOLS.get(src_lang, [src_lang]):
                    if lang == "umay":
                        if not os.path.exists(UMAY):
                            continue
                    else:
                        tool = TOOL_FOR.get(lang)
                        if tool and not have(tool):
                            continue

                    srcdir = os.path.join(workdir, f"{wname}_{lang}")
                    os.makedirs(srcdir, exist_ok=True)
                    src = os.path.join(srcdir, f"prog.{EXT_FOR[src_lang]}")
                    with open(src, "w") as f:
                        f.write(source)

                    if lang == "umay":
                        entry, exe = build_umay(srcdir, src)
                    else:
                        entry, exe = build_other(lang, srcdir, src)

                    if entry["build_ok"] and exe and os.path.exists(exe):
                        # warm-up: page in the loader and the text
                        _run_once([exe], srcdir, None)
                        r = measure([exe], srcdir, None, RUN_REPS)
                        entry["run_median_s"] = r["median_s"]
                        entry["run_min_s"] = r["min_s"]
                        entry["run_max_s"] = r["max_s"]
                        entry["run_rss_kb"] = r["rss_kb"]
                        entry["output"] = last_integer(r["stdout"], r["stderr"])
                    else:
                        entry["run_median_s"] = None
                        entry["output"] = None

                    results["workloads"][wname][lang] = entry
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    if json_out:
        print(json.dumps(results, indent=2))
        return

    print("\n=== Toolchains ===")
    for name, v in results["toolchains"].items():
        print(f"  {name:<8} {v or '(not installed)'}")

    for wname, langs in results["workloads"].items():
        print(f"\n=== Workload: {wname} ===")
        hdr = (f"  {'lang':<9} {'build med':>10} {'build min':>10} {'bld RSS':>9} "
               f"{'binary':>10} {'run med':>9} {'run min':>9} {'run RSS':>9}  output")
        print(hdr)
        outputs = set()
        for lang in sorted(langs):
            e = langs[lang]
            def f(x, fmt):
                return fmt.format(x) if x is not None else "-"
            o = e.get("output")
            if o is not None:
                outputs.add(o)
            tail = str(o) if o is not None else ("BUILD FAILED" if not e["build_ok"] else "RUN FAILED")
            print(f"  {lang:<9} "
                  f"{f(e.get('build_median_s'), '{:.3f}s'):>10} "
                  f"{f(e.get('build_min_s'), '{:.3f}s'):>10} "
                  f"{f(e.get('build_rss_kb'), '{:.0f}K'):>9} "
                  f"{f(e.get('binary_bytes'), '{:.0f}B'):>10} "
                  f"{f(e.get('run_median_s'), '{:.4f}s'):>9} "
                  f"{f(e.get('run_min_s'), '{:.4f}s'):>9} "
                  f"{f(e.get('run_rss_kb'), '{:.0f}K'):>9}  {tail}")
            if not e["build_ok"] and e.get("build_error"):
                for line in e["build_error"].strip().splitlines()[:3]:
                    print(f"      {line}")
        if len(outputs) > 1:
            print(f"  !! outputs differ across languages: {outputs}")
        elif outputs:
            print(f"  (all languages printed: {next(iter(outputs))})")


if __name__ == "__main__":
    main()
