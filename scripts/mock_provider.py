"""OpenAI-compatible mock provider used by the test harness and scripts/bench.py.
Model ids drive behaviour: a429 → 429 with retry-after 5; r429 → 429 twice then ok;
b500 → 503; loopy → repeats itself; anything else → scripted tool calls by prompt keyword.
Listens on 127.0.0.1:18080.
"""
import json, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer

# Script: a sequence of tool calls driven by the user's prompt keywords.
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        n = int(self.headers.get('content-length', 0))
        body = json.loads(self.rfile.read(n))
        msgs = body.get("messages", [])
        tools = [t['function']['name'] for t in body.get('tools',[])]
        last = msgs[-1]
        sys.stderr.write(f"REQ model={body.get('model')} msgs={len(msgs)} tools={tools} last_role={last['role']}\n")
        sysc = sum(len(m['content'] if isinstance(m['content'], str) else json.dumps(m['content'])) for m in msgs if m['role']=='system')
        toolc = len(json.dumps(body.get('tools', [])))
        convc = sum(len(json.dumps(m)) for m in msgs if m['role']!='system')
        per_tool = sorted(((len(json.dumps(t)), t['function']['name']) for t in body.get('tools', [])), reverse=True)
        sys.stderr.write(f"SIZES system={sysc} tools={toolc} conversation={convc} total={sysc+toolc+convc} chars (~{(sysc+toolc+convc)//4} tokens) per_tool={per_tool}\n")
        pass
        if body.get("model") == "a429":
            self.send_response(429); self.send_header("content-type","application/json"); self.send_header("retry-after","5"); self.end_headers()
            self.wfile.write(b'{"error":{"message":"Rate limit reached for a429: too many requests per day","type":"rate_limit"}}'); return
        if body.get("model") == "r429":
            H.r429 = getattr(H, "r429", 0) + 1
            if H.r429 <= 2:
                self.send_response(429); self.send_header("content-type","application/json"); self.send_header("retry-after","3"); self.end_headers()
                self.wfile.write(b'{"error":{"message":"Rate limit reached, slow down","type":"rate_limit"}}'); return
        if body.get("model") == "loopy":
            self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
            for i in range(80):
                for piece in ["I'll run the command.\n\n", "Actually, I'll just run it.\n\n", "Wait, I'll also check if example-server is in the lunarzero.json under mcp. Yes.\n\n"]:
                    self.wfile.write(f"data: {json.dumps({'choices':[{'delta':{'content':piece}}]})}\n\n".encode()); self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n"); return
        if body.get("model") == "b500":
            self.send_response(503); self.send_header("content-type","application/json"); self.end_headers()
            self.wfile.write(b'{"error":{"message":"service unavailable"}}'); return
        self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
        def send(obj):
            self.wfile.write(f"data: {json.dumps(obj)}\n\n".encode()); self.wfile.flush()
        def call(name, args, cid="call_1"):
            send({"choices":[{"delta":{"tool_calls":[{"index":0,"id":cid,"type":"function","function":{"name":name,"arguments":json.dumps(args)}}]}}]})
            send({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})
        def text(s):
            for w in s.split(" "):
                send({"choices":[{"delta":{"content":w+" "}}]})
            send({"choices":[{"delta":{},"finish_reason":"stop"}]})
        user_text = ""; all_user = ""
        for m in msgs:
            if m["role"] == "user":
                c = m["content"]; user_text = c if isinstance(c, str) else " ".join(p.get("text","") for p in c)
                all_user += " " + user_text
        tool_results = [m for m in msgs if m["role"] == "tool"]
        step = len(tool_results)
        if not tools:
            text("Add --json flag to export" if "--json flag" in all_user else "Title")
        elif "scenario1" in user_text:
            if "slow" in all_user: time.sleep(1.2)  # SLOW_STEP
            plan = [("write", {"filePath": "hello.txt", "content": "hello world\nsecond line\n"}),
                    ("edit", {"filePath": "hello.txt", "oldString": "hello world", "newString": "hello lunar"}),
                    ("bash", {"command": "cat hello.txt && echo done"}),
                    ("glob", {"pattern": "*.txt"}),
                    ("grep", {"pattern": "lunar"}),
                    ("todowrite", {"todos": [{"content": "ship it", "status": "in_progress", "priority": "high"}]})]
            if step < len(plan):
                call(*plan[step], cid=f"call_{step}")
            else:
                text("All done: " + tool_results[-1]["content"][:80].replace("\n", " / "))
        elif "scenario5" in user_text:
            if step == 0: call("write", {"filePath": "main.c", "content": "int main(void) { int x = \"str\"; return y; }\n"})
            else: text("WROTE: " + tool_results[-1]["content"][:400].replace("\n", " / "))
        elif "scenario4" in user_text:
            if step == 0: call("write", {"filePath": "src/lib.rs", "content": "pub fn f() -> i32 { \"x\" }\n"})
            else: text("WROTE: " + tool_results[-1]["content"][:300].replace("\n", " / "))
        elif "scenario3" in user_text:
            if step == 0: call("mini_echo", {"text": "hello mcp"})
            else: text("MCP said: " + tool_results[-1]["content"][:80])
        elif "scenario6" in user_text:
            if step == 0: call("question", {"questions":[{"question":"Which approach?","header":"Approach","options":[{"label":"Alpha","description":"the first way"},{"label":"Beta","description":"the second way"}]}]})
            else: text("You chose: " + tool_results[-1]["content"][:100].replace("\n", " / "))
        elif "scenario7" in user_text:
            send({"choices":[{"delta":{"reasoning_content":"Let me think about this carefully. "}}]})
            send({"choices":[{"delta":{"reasoning_content":"The answer involves markdown."}}]})
            md = "# Heading\n\nSome **bold** and `code` text with a [link](https://x.y).\n\n- item one\n- item two\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quoted text\n\nDone."
            for ch in [md[i:i+7] for i in range(0, len(md), 7)]:
                send({"choices":[{"delta":{"content":ch}}]})
            send({"choices":[{"delta":{},"finish_reason":"stop"}]})
        elif "scenario9" in user_text:
            if step == 0: call("bash", {"command": "\"$LZ_BIN\" skill install https://github.com/anthropics/skills/tree/main/skills/pdf --project"})
            else: text("Installed: " + tool_results[-1]["content"][:160].replace("\n", " / "))
        elif "scenario8" in user_text:
            for ch in ["<thou", "ght>The user greets me. I sh", "ould answer briefly.</thought>", "Hi there! ", "How can I help?"]:
                send({"choices":[{"delta":{"content":ch}}]})
            send({"choices":[{"delta":{},"finish_reason":"stop"}]})
        elif "scenario_plan" in all_user:
            nudged = "open items" in user_text
            if nudged and step == 0 + sum(1 for m in msgs if m["role"]=="tool"):
                pass
            if not nudged:
                if step == 0: call("todowrite", {"todos":[{"content":"Phase A","status":"completed","priority":"high"},{"content":"Phase B: deploy","status":"in_progress","priority":"high"}]})
                else: text("I have completed Phase A. To finish Phase B, the next steps would be: run the deploy.")
            else:
                mine = [m for m in msgs[msgs.index(next(m for m in msgs if m["role"]=="user" and "open items" in (m["content"] if isinstance(m["content"],str) else " ".join(p.get("text","") for p in m["content"])))):] if m["role"]=="tool"]
                if len(mine) == 0: call("bash", {"command": "echo deployed"})
                elif len(mine) == 1: call("todowrite", {"todos":[{"content":"Phase A","status":"completed","priority":"high"},{"content":"Phase B: deploy","status":"completed","priority":"high"}]}, cid="call_t2")
                else: text("NUDGED-AND-FINISHED: all items closed.")
        elif "--json flag" in all_user:
            todos = lambda a,b,c: [{"content":"Add a --json branch to export::render","status":a,"priority":"high"},{"content":"Cover it with a unit test","status":b,"priority":"medium"},{"content":"Run cargo test","status":c,"priority":"medium"}]
            if step == 0: call("todowrite", {"todos": todos("in_progress","pending","pending")})
            elif step == 1: call("read", {"filePath": "src/export.rs"})
            elif step == 2: call("edit", {"filePath": "src/export.rs", "oldString": "pub fn render(notes: &[&str], _json: bool) -> String {\n    let mut out = String::new();", "newString": "pub fn render(notes: &[&str], json: bool) -> String {\n    if json {\n        let items: Vec<String> = notes.iter().map(|n| format!(\"{n:?}\")).collect();\n        return format!(\"[{}]\\n\", items.join(\", \"));\n    }\n    let mut out = String::new();"})
            elif step == 3: call("edit", {"filePath": "src/export.rs", "oldString": "        assert_eq!(render(&[\"a\", \"b\"], false), \"1. a\\n2. b\\n\");\n    }", "newString": "        assert_eq!(render(&[\"a\", \"b\"], false), \"1. a\\n2. b\\n\");\n    }\n\n    #[test]\n    fn json_list() {\n        assert_eq!(render(&[\"a\", \"b\"], true), \"[\\\"a\\\", \\\"b\\\"]\\n\");\n    }"})
            elif step == 4: call("todowrite", {"todos": todos("completed","completed","in_progress")})
            elif step == 5: call("bash", {"command": "cargo test -q 2>&1 | tail -4"})
            elif step == 6: call("todowrite", {"todos": todos("completed","completed","completed")})
            else:
                send({"choices":[{"delta":{"reasoning_content":"Both tests pass; summarise the change briefly."}}]})
                for ch in ["Added the `--json` branch in `src/export.rs:2` — ", "`render` now returns a JSON array when the flag is set, ", "plain numbered list otherwise.\n\n", "- new test `json_list` covers the JSON path\n", "- `cargo test`: **2 passed**, 0 failed\n\n", "`notes --json` prints `[\"buy milk\", \"ship v0.4\"]`."]:
                    send({"choices":[{"delta":{"content":ch}}]})
                send({"choices":[{"delta":{},"finish_reason":"stop"}]})
        elif "scenario_heal" in all_user:
            # turn 1: write a broken script, run the "tests", give up. the runner should
            # feed the failure back; turn 2: fix it and rerun.
            healed = "repair round" in user_text
            mine = [m for m in msgs[len(msgs) - 1 - 2*step:]] if False else None
            if not healed:
                if step == 0: call("write", {"filePath": "check.sh", "content": "#!/bin/sh\necho 'error[E0308]: mismatched types --> src/lib.rs:4:5' >&2\nexit 1\n"})
                elif step == 1: call("bash", {"command": "cargo test -q 2>&1 || sh check.sh"})
                else: text("The tests fail with a type error. You may want to look into src/lib.rs.")
            else:
                idx = next(i for i, m in enumerate(msgs) if m["role"] == "user" and "repair round" in (m["content"] if isinstance(m["content"], str) else " ".join(p.get("text","") for p in m["content"])))
                after = [m for m in msgs[idx:] if m["role"] == "tool"]
                if len(after) == 0: call("write", {"filePath": "check.sh", "content": "#!/bin/sh\necho 'test result: ok. 3 passed'\nexit 0\n"})
                elif len(after) == 1: call("bash", {"command": "cargo test -q 2>&1 || sh check.sh"}, cid="call_h2")
                else: text("HEALED: fixed the type error, tests pass now.")
        elif "scenario_hunks" in all_user:
            new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nM\nn\n"
            if step == 0: call("write", {"filePath": "hunk.txt", "content": new})
            else: text("RESULT: " + tool_results[-1]["content"][:400].replace("\n", " / "))
        elif "scenario_lsp" in all_user:
            # write a type error, declare victory; the LSP repair round should bring the
            # diagnostics back before any cargo command runs; then fix it.
            healed = "language server" in user_text
            bad = "/// Render the notes for the `export` command.\npub fn render(notes: &[&str], _json: bool) -> String {\n    let count: i32 = \"oops\";\n    let mut out = String::new();\n    for (i, n) in notes.iter().enumerate() {\n        out.push_str(&format!(\"{}. {}\\n\", i + 1, n));\n    }\n    out\n}\n"
            good = bad.replace('let count: i32 = \"oops\";', 'let _count: i32 = notes.len() as i32;')
            if not healed:
                if step == 0: call("write", {"filePath": "src/export.rs", "content": bad})
                else: text("Rewrote render(); all good.")
            else:
                idx = next(i for i, m in enumerate(msgs) if m["role"] == "user" and "language server" in (m["content"] if isinstance(m["content"], str) else " ".join(p.get("text","") for p in m["content"])))
                after = [m for m in msgs[idx:] if m["role"] == "tool"]
                if len(after) == 0: call("write", {"filePath": "src/export.rs", "content": good}, cid="call_fix")
                else: text("LSP-HEALED: " + tool_results[-1]["content"][:120].replace("\n", " / "))
        elif "symbol_lookup" in all_user:
            name = user_text.split("symbol_lookup", 1)[1].strip().split()[0] if user_text.strip().split()[-1] != "symbol_lookup" else "render"
            if step == 0: call("symbol", {"name": name})
            else: text("SYMBOL: " + tool_results[-1]["content"][:500].replace("\n", " / "))
        elif "scenario_child" in user_text:
            text("CHILD DONE 42")
        elif "Background task" in user_text:
            text("PARENT GOT: " + user_text.replace("\n", " / ")[:200])
        elif "scenario_bg" in all_user:
            if step == 0: call("task", {"description": "count things", "prompt": "scenario_child count", "subagent_type": "general", "background": True})
            else: text("Started the background task; carrying on.")
        elif "scenario2" in user_text:
            if step == 0: call("bash", {"command": "rm -rf never"})
            else: text("Result: " + tool_results[-1]["content"][:120].replace("\n", " / "))
        else:
            text("Hello from mock!")
        send({"choices":[],"usage":{"prompt_tokens":(sysc+toolc+convc)//4,"completion_tokens":90}})
        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()

HTTPServer(("127.0.0.1", 18080), H).serve_forever()
