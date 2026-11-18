#!/usr/bin/python3
"""File-backed iptables/ip6tables/ipset/ip commands for HTTP lifecycle tests.

Copy this file into a temporary directory using those executable names.
It never opens sockets or changes the host firewall.
"""
import json
from pathlib import Path
import sys
import time

root = Path(sys.argv[0]).parent
program = Path(sys.argv[0]).name
args = sys.argv[1:]
state_path = root / "netfilter-state.json"
state = json.loads(state_path.read_text()) if state_path.exists() else {
    "iptables": {"filter:INPUT": [["-j", "ACCEPT"]],
                 "filter:FORWARD": [["-j", "ACCEPT"]], "nat:PREROUTING": []},
    "ip6tables": {"filter:INPUT": [["-j", "ACCEPT"]]}, "sets": {},
}
with (root / "trace.jsonl").open("a") as trace:
    trace.write(json.dumps({"program": program, "args": args}) + "\n")


def finish(success=True, output=""):
    state_path.write_text(json.dumps(state))
    if output:
        print(output)
    sys.exit(0 if success else 1)


failure = root / "fail-next"
if failure.exists() and failure.read_text().strip() in " ".join([program, *args]):
    failure.unlink()
    finish(False, "injected command failure")

if program == "ip":
    finish(True, "2: mock0 inet 192.0.2.1/24 scope global mock0")
if program == "nginx":
    finish()
if program == "ipset":
    action, name, *options = args
    sets = state["sets"]
    if action == "list":
        finish(name in sets, json.dumps(sets.get(name, {})))
    if action == "create":
        sets.setdefault(name, {})
    elif name not in sets:
        finish(False)
    elif action == "flush":
        sets[name].clear()
    elif action == "add":
        timeout = int(options[options.index("timeout") + 1])
        sets[name][options[0]] = {"timeout": timeout, "expires": time.time() + timeout}
    elif action == "test":
        entry = sets[name].get(options[0], {})
        finish(entry.get("expires", 0) > time.time())
    else:
        finish(False, "unsupported mock ipset command")
    finish()

if program not in ("iptables", "ip6tables"):
    finish(False, "unsupported mock program")
table = "filter"
if args[:1] == ["-t"]:
    table, args = args[1], args[2:]
action = args[0]
chains = state[program]
chain = args[1] if len(args) > 1 else None
key = f"{table}:{chain}"
if action == "-S":
    keys = [key] if chain else [key for key in chains if key.startswith(table + ":")]
    if any(key not in chains for key in keys):
        finish(False)
    lines = []
    for key in keys:
        name = key.split(":", 1)[1]
        lines.append(f"-N {name}")
        lines.extend(f"-A {name} {' '.join(rule)}" for rule in chains[key])
    finish(True, "\n".join(lines))
if action == "-N":
    if key in chains:
        finish(False)
    chains[key] = []
    finish()
if key not in chains:
    finish(False)
if action == "-F":
    chains[key] = []
elif action == "-A":
    chains[key].append(args[2:])
elif action == "-I":
    index = int(args[2]) - 1 if len(args) > 2 and args[2].isdigit() else 0
    chains[key].insert(index, args[3:] if len(args) > 2 and args[2].isdigit() else args[2:])
elif action == "-C":
    finish(args[2:] in chains[key])
elif action == "-D":
    if len(args) == 3 and args[2].isdigit():
        index = int(args[2]) - 1
        if index >= len(chains[key]):
            finish(False)
        chains[key].pop(index)
    elif args[2:] in chains[key]:
        chains[key].remove(args[2:])
    else:
        finish(False)
else:
    finish(False, "unsupported mock iptables command")
finish()
