#!/usr/bin/env python3
"""Reproducible router/agent benchmarks against scripts/mock_provider.py.

    python3 scripts/bench.py [path/to/lz]

Measures, on this machine:
  * failover latency — time from the first request (answered 429 / 503) to the
    request on the next model, with no output streamed yet
  * wait-for-soonest — a 429 with retry-after on the only model: the run waits
    exactly that long instead of failing
  * fixed request overhead — system prompt + tool schemas, in tokens (chars/4)
  * a 6-step tool turn: requests, wall time, tokens sent per step

Everything runs locally; no provider key is used or touched (LZ_AUTH_CONTENT={}).
"""
import json, os, re, subprocess, sys, tempfile, time

LZ = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/release/lz")
MOCK = os.path.join(os.path.dirname(__file__), "mock_provider.py")

def cfg(models, extra=None):
    c = {
        "provider": {"mock": {"npm": "@ai-sdk/openai-compatible",
                              "options": {"baseURL": "http://127.0.0.1:18080/v1", "apiKey": "x"},
                              "models": {m: {"name": m} for m in models}}},
        "pool": {"include": [f"mock/{m}" for m in models], "sticky_minutes": 0},
        "permission": {"*": "allow"},
    }
    if extra:
        c.update(extra)
    return json.dumps(c)

def run(model, prompt, models, timeout=120):
    home = tempfile.mkdtemp(prefix="lzbench-")
    env = {k: v for k, v in os.environ.items() if not k.endswith("_API_KEY")}
    env.update(HOME=home, LZ_AUTH_CONTENT="{}", LZ_DISABLE_MODELS_FETCH="1", LZ_LOG_LEVEL="debug",
               LZ_CONFIG_CONTENT=cfg(models), LZ_WEB="0")
    t0 = time.time()
    log = open(os.path.join(home, "lz.log"), "w+")
    p = subprocess.Popen([LZ, "run", "--auto", "--print-logs", "--model", model, prompt],
                         env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT, cwd=home)
    try:
        p.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        p.kill()
        log.seek(0)
        sys.exit("lz timed out after %ss; last log lines:\n%s" % (timeout, "".join(log.readlines()[-15:])))
    wall = time.time() - t0
    log.seek(0)
    text = log.read()
    stamps = []
    for line in text.splitlines():
        m = re.search(r"^(\S+)Z .*llm request url=.* model=\"([^\"]+)\"", line)
        if m:
            ts = m.group(1)
            h, mi, s = ts.split("T")[1].split(":")
            stamps.append((float(h) * 3600 + float(mi) * 60 + float(s), m.group(2)))
    return wall, stamps, text

def main():
    import socket
    for _ in range(100):  # wait for 18080 to be free (a previous mock may still be exiting)
        with socket.socket() as sk:
            if sk.connect_ex(("127.0.0.1", 18080)) != 0:
                break
        time.sleep(0.1)
    mock_log = tempfile.NamedTemporaryFile("w+", prefix="lzbench-mock-", suffix=".log", delete=False)
    mock = subprocess.Popen([sys.executable, MOCK], stderr=mock_log, stdout=subprocess.DEVNULL)
    for _ in range(50):  # …and for the new one to listen
        with socket.socket() as sk:
            if sk.connect_ex(("127.0.0.1", 18080)) == 0:
                break
        time.sleep(0.1)
    if mock.poll() is not None:
        sys.exit("mock provider failed to start (see %s)" % mock_log.name)
    try:
        # 1. failover on 429 (a429 is the highest-quality pool member; m1 is the fallback)
        wall, stamps, _ = run("lunar/auto", "hello", ["a429", "m1"])
        req = [s for s in stamps if s[1] in ("a429", "m1")]
        fo429 = next((b[0] - a[0] for a, b in zip(req, req[1:]) if a[1] == "a429" and b[1] == "m1"), None)
        # 2. failover on 503
        wall2, stamps2, _ = run("lunar/auto", "hello", ["b500", "m1"])
        req2 = [s for s in stamps2 if s[1] in ("b500", "m1")]
        fo503 = next((b[0] - a[0] for a, b in zip(req2, req2[1:]) if a[1] == "b500" and b[1] == "m1"), None)
        # 3. wait-for-soonest: r429 answers 429 (retry-after 3 s) twice, then succeeds
        wall3, stamps3, out3 = run("lunar/auto", "hello", ["r429"])
        # 4. a six-step tool turn
        wall4, stamps4, _ = run("mock/m1", "scenario1 go", ["m1"])
        # request sizes from the mock's own log
        mock.terminate()
        mock_log.flush()
        steps = [(int(m.group(1)), int(m.group(2)), int(m.group(4)))
                 for m in re.finditer(r"system=(\d+) tools=(\d+) conversation=(\d+) total=(\d+)", open(mock_log.name).read())
                 if int(m.group(2)) > 10]
        overhead = [(sy + t) // 4 for sy, t, _ in steps]
        totals = [t // 4 for _, _, t in steps]
    finally:
        mock.kill()
    print("LunarZero benchmark (local mock provider)")
    print(f"  failover after 429, before any output : {fo429*1000:6.0f} ms" if fo429 is not None else "  failover 429: n/a")
    print(f"  failover after 503, before any output : {fo503*1000:6.0f} ms" if fo503 is not None else "  failover 503: n/a")
    print(f"  429 + retry-after 3 s on the only model: run waited {wall3:5.1f} s total, answered: {'yes' if 'Hello from mock' in out3 else 'no'}")
    print(f"  6-step tool turn                       : {len([s for s in stamps4 if s[1]=='m1'])} requests in {wall4:4.1f} s wall")
    if steps:
        print(f"  fixed overhead per step (system+tools) : ~{min(overhead)} tokens")
        print(f"  agent steps sent                        : {min(totals)}–{max(totals)} tokens each ({len(steps)} steps with tools)")

if __name__ == "__main__":
    main()
