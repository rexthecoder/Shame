#!/usr/bin/env python3
"""Black-box startup/memory/quit benchmark for desktop editors (macOS).

Measures an app from the outside, the way a user experiences it, which the
headless `crates/bench` harness cannot: time until CPU settles ("ready"),
CPU time spent getting there, idle CPU, memory, and quit time.

    python3 perf/bench.py --project ~/Desktop/Projects/luna
    python3 perf/bench.py --project ~/p --app zed=/Applications/Zed.app/Contents/MacOS/zed

Apps run one at a time, interleaved; the first run of each is a discarded
warm-up. Close any running instance of an app first (a running instance
takes over the launch). Standard library only.
"""
import argparse, json, os, re, signal, statistics, subprocess, sys, time

POLL = 0.2


def ps_table():
    out = subprocess.run(["ps", "-axo", "pid=,ppid=,rss=,time=,comm="], capture_output=True, text=True).stdout
    rows = {}
    for line in out.splitlines():
        parts = line.split(None, 4)
        if len(parts) == 5:
            pid, ppid, rss, t, comm = parts
            rows[int(pid)] = (int(ppid), int(rss), cpu_seconds(t), comm)
    return rows


def group_of(comm, is_root):
    """Shame's own process, the Dart/Flutter tooling it spawns, or anything else."""
    name = os.path.basename(comm).lower()
    if is_root:
        return "app"
    return "dart" if ("dart" in name or "flutter" in name) else "other"


def cpu_seconds(t):  # [[dd-]hh:]mm:ss.ss
    days = 0
    if "-" in t:
        d, t = t.split("-")
        days = int(d)
    secs = 0.0
    for p in t.split(":"):
        secs = secs * 60 + float(p)
    return days * 86400 + secs


class Tree:
    """Root process plus descendants; CPU of exited children is kept."""

    def __init__(self, root):
        self.root, self.seen_cpu, self.seen_comm = root, {}, {}

    def pids(self, table):
        kids, stack = ({self.root} if self.root in table else set()), [self.root]
        while stack:
            cur = stack.pop()
            for pid, (ppid, *_) in table.items():
                if ppid == cur and pid not in kids:
                    kids.add(pid)
                    stack.append(pid)
        return kids

    def sample(self):
        table = ps_table()
        live = self.pids(table)
        for pid in live:
            self.seen_cpu[pid] = table[pid][2]
            self.seen_comm[pid] = table[pid][3]
        rss_kb = sum(table[p][1] for p in live)
        return sum(self.seen_cpu.values()), rss_kb / 1024, live


def footprint_by_group(pids, comms, root):
    """Physical footprint (macOS's USS-like number) in MB, per group."""
    out = {"app": 0.0, "dart": 0.0, "other": 0.0}
    for pid in pids:
        r = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True)
        m = re.search(r"Footprint:\s+([\d.]+)\s*(KB|MB|GB)", r.stdout)
        if m:
            mb = float(m.group(1)) * {"KB": 1 / 1024, "MB": 1, "GB": 1024}[m.group(2)]
            out[group_of(comms.get(pid, ""), pid == root)] += mb
    return out


def cpu_by_group(tree):
    out = {"app": 0.0, "dart": 0.0, "other": 0.0}
    for pid, cpu in tree.seen_cpu.items():
        out[group_of(tree.seen_comm.get(pid, ""), pid == tree.root)] += cpu
    return out


def settled(hist, now, cpu, quiet_secs, quiet_cores):
    """True once `cpu` has grown by under `quiet_cores` cores over `quiet_secs`."""
    return now - hist[0][0] >= quiet_secs * 0.95 and (cpu - hist[0][1]) / (now - hist[0][0]) < quiet_cores


def run_once(cmd, args, quiet_secs, quiet_cores, idle_secs, timeout):
    start = time.perf_counter()
    proc = subprocess.Popen([cmd, *args], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    tree, hist, app_hist, peak = Tree(proc.pid), [], [], 0.0
    ready = ready_app = None
    while time.perf_counter() - start < timeout:
        time.sleep(POLL)
        now = time.perf_counter() - start
        cpu, rss, _ = tree.sample()
        app_cpu = tree.seen_cpu.get(proc.pid, 0.0)
        peak = max(peak, rss)
        hist.append((now, cpu))
        hist = [h for h in hist if now - h[0] <= quiet_secs]
        app_hist.append((now, app_cpu))
        app_hist = [h for h in app_hist if now - h[0] <= quiet_secs]
        # Shame's own process settling is tracked apart from the whole tree.
        if ready_app is None and settled(app_hist, now, app_cpu, quiet_secs, quiet_cores):
            ready_app = app_hist[0][0]
        if settled(hist, now, cpu, quiet_secs, quiet_cores):
            ready = hist[0][0]
            ready_app = ready_app if ready_app is not None else ready
            break
    if ready is None:
        proc.terminate()
        proc.wait()
        return None
    cpu_ready, _, _ = tree.sample()
    groups_cpu = cpu_by_group(tree)
    t0, c0 = time.perf_counter(), cpu_ready
    time.sleep(idle_secs)
    c1, rss, live = tree.sample()
    idle_pct = 100 * (c1 - c0) / (time.perf_counter() - t0)
    mem = footprint_by_group(live, tree.seen_comm, proc.pid)
    q0 = time.perf_counter()
    for pid in live:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    quit_s = None
    while time.perf_counter() - q0 < 15:
        # poll() reaps the root; an unreaped child stays in `ps` as a zombie.
        root_gone = proc.poll() is not None
        others = {p for p in live if p != proc.pid} & set(ps_table())
        if root_gone and not others:
            quit_s = time.perf_counter() - q0
            break
        time.sleep(0.05)
    killed = quit_s is None
    if killed:  # did not quit on SIGTERM: record that, then make sure it is gone
        for pid in live:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        proc.wait()
        quit_s = 15.0
    return dict(ready_s=ready, ready_app_s=ready_app, cpu_s=cpu_ready,
                cpu_app_s=groups_cpu["app"], cpu_dart_s=groups_cpu["dart"],
                idle_cpu_pct=idle_pct, footprint_mb=sum(mem.values()),
                footprint_app_mb=mem["app"], footprint_dart_mb=mem["dart"],
                peak_rss_mb=peak, quit_s=quit_s, killed_on_quit=float(killed))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--project", required=True)
    ap.add_argument("--app", action="append", default=[], help="label=executable (default: shame)")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--quiet-secs", type=float, default=3.0)
    ap.add_argument("--quiet-cores", type=float, default=0.1)
    ap.add_argument("--idle-secs", type=float, default=10.0)
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--cooldown", type=float, default=3.0)
    ap.add_argument("--out", default="perf/results.json")
    a = ap.parse_args()
    apps = dict(x.split("=", 1) for x in a.app) or {"shame": "/Applications/Shame.app/Contents/MacOS/dart-ide"}
    project = os.path.abspath(os.path.expanduser(a.project))
    for label, cmd in apps.items():  # a running instance would take over the launch
        if subprocess.run(["pgrep", "-f", cmd], capture_output=True).returncode == 0:
            sys.exit(f"{label} is already running; quit it first.")
    results = {k: [] for k in apps}
    order = list(apps)
    for i in range(a.runs + 1):  # run 0 is the warm-up
        for label in order:
            time.sleep(a.cooldown)
            r = run_once(apps[label], [project], a.quiet_secs, a.quiet_cores, a.idle_secs, a.timeout)
            tag = "warm-up" if i == 0 else f"run {i}"
            print(f"{label:>8} {tag}: {r if r else 'timed out'}", flush=True)
            if i and r:
                results[label].append(r)
        order = order[1:] + order[:1]  # rotate so no app always follows the same one
    print(f"\nmedian of {a.runs} runs (min-max) on {project}")
    for label, runs in results.items():
        if not runs:
            continue
        row = []
        for key in runs[0]:
            v = [r[key] for r in runs]
            row.append(f"{key}={statistics.median(v):.1f} ({min(v):.1f}-{max(v):.1f})")
        print(f"{label:>8}  " + "  ".join(row))
    os.makedirs(os.path.dirname(a.out) or ".", exist_ok=True)
    json.dump(results, open(a.out, "w"), indent=2)


if __name__ == "__main__":
    sys.exit(main())
