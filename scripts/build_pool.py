#!/usr/bin/env python3
"""Build assets/pool.json — LunarZero's free-tier model pool.

Membership comes from the embedded catalog snapshot (assets/models.json.gz) using
per-provider free-tier rules; quality and speed scores are computed here from
model metadata (parameter counts, the price the same model fetches on paid
providers, release date, reasoning/context) rather than hand-ranked; rate
limits are the providers' published free-tier caps.
"""
import gzip, json, math, re, datetime, sys, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
CAT = json.load(gzip.open(ROOT / "assets/models.json.gz"))
TODAY = datetime.date(2026, 9, 11)

# provider id → (display, signup, env vars, OpenAI-compatible base url, note, speed prior 0-1, default free-tier limits)
PROVIDERS = {
    "groq":       ("Groq", "https://console.groq.com/keys", ["GROQ_API_KEY"], "https://api.groq.com/openai/v1",
                   "LPU inference, very fast; ~1000 requests/day per model", 0.95, {"rpm": 30, "rpd": 1000, "tpm": 12000, "tpd": 500000}),
    "cerebras":   ("Cerebras", "https://cloud.cerebras.ai/", ["CEREBRAS_API_KEY"], "https://api.cerebras.ai/v1",
                   "Fastest tokens/s; 1M tokens/day", 1.0, {"rpm": 30, "tpm": 60000, "tpd": 1000000}),
    "google":     ("Google AI Studio", "https://aistudio.google.com/apikey", ["GEMINI_API_KEY", "GOOGLE_GENERATIVE_AI_API_KEY", "GOOGLE_API_KEY"],
                   "https://generativelanguage.googleapis.com/v1beta/openai", "Gemini Flash free tier (per-model daily caps), Gemma generous", 0.7, {"rpm": 10, "rpd": 250, "tpm": 250000}),
    "openrouter": ("OpenRouter", "https://openrouter.ai/keys", ["OPENROUTER_API_KEY"], "https://openrouter.ai/api/v1",
                   ":free models — 20 RPM, 50 RPD (1000 RPD after a one-time $10 credit)", 0.45, {"rpm": 20, "rpd": 50}),
    "kilo":       ("Kilo Gateway", "https://app.kilo.ai/", ["KILO_API_KEY"], "https://api.kilo.ai/api/gateway/v1",
                   ":free models", 0.45, {"rpm": 20, "rpd": 200}),
    "nvidia":     ("NVIDIA NIM", "https://build.nvidia.com/", ["NVIDIA_API_KEY"], "https://integrate.api.nvidia.com/v1",
                   "Large open models; 40 RPM; one tool call per turn", 0.5, {"rpm": 40}),
    "mistral":    ("Mistral", "https://console.mistral.ai/api-keys", ["MISTRAL_API_KEY"], "https://api.mistral.ai/v1",
                   "Experiment plan: 1 request/s, 500k tokens/min, 1B tokens/month", 0.6, {"rpm": 60, "tpm": 500000}),
    "huggingface":("Hugging Face", "https://huggingface.co/settings/tokens", ["HF_TOKEN"], "https://router.huggingface.co/v1",
                   "Monthly inference credits via the router", 0.45, {"rpm": 30}),
    "ollama-cloud":("Ollama Cloud", "https://ollama.com/settings/keys", ["OLLAMA_API_KEY"], "https://ollama.com/v1",
                   "Free tier with hourly/daily usage caps", 0.5, {"rpm": 20, "rpd": 300}),
    "cohere":     ("Cohere", "https://dashboard.cohere.com/api-keys", ["COHERE_API_KEY"], "https://api.cohere.ai/compatibility/v1",
                   "Trial key: 20 RPM, 1000 calls/month", 0.5, {"rpm": 20, "rpd": 33}),
    "cloudflare-workers-ai": ("Cloudflare Workers AI", "https://dash.cloudflare.com/profile/api-tokens", ["CLOUDFLARE_API_KEY", "CLOUDFLARE_API_TOKEN"],
                   "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/ai/v1", "Also export CLOUDFLARE_ACCOUNT_ID; 10k neurons/day", 0.4, {"rpm": 30}),
    "zai":        ("Z.AI", "https://z.ai/manage-apikey/apikey-list", ["ZHIPU_API_KEY", "ZAI_API_KEY"], "https://api.z.ai/api/paas/v4",
                   "GLM *-flash models are free", 0.6, {"rpm": 30, "tpd": 1000000}),
    "github-models": ("GitHub Models", "https://github.com/settings/personal-access-tokens/new", ["GITHUB_MODELS_TOKEN", "GITHUB_TOKEN"],
                   "https://models.github.ai/inference", "Fine-grained PAT with models:read; small daily caps, 8k context", 0.55, {"rpm": 10, "rpd": 50}),
}

# per-model limit overrides (published free-tier caps that differ from the provider default)
LIMITS = {
    ("google", "gemini-2.5-flash-lite"): {"rpm": 15, "rpd": 1000, "tpm": 250000},
    ("google", "gemini-3.1-flash-lite-preview"): {"rpm": 15, "rpd": 1000, "tpm": 250000},
    ("groq", "llama-3.1-8b-instant"): {"rpm": 30, "rpd": 14400, "tpm": 6000, "tpd": 500000},
    ("groq", "openai/gpt-oss-120b"): {"rpm": 30, "rpd": 1000, "tpm": 8000, "tpd": 200000},
    ("groq", "openai/gpt-oss-20b"): {"rpm": 30, "rpd": 1000, "tpm": 8000, "tpd": 200000},
}
# GitHub Models is not in the catalog snapshot; a hand list of its free-tier chat models
GITHUB_MODELS = [
    ("openai/gpt-4.1", "GPT-4.1", 8000, True, 2025), ("openai/gpt-4.1-mini", "GPT-4.1 mini", 8000, True, 2025),
    ("openai/gpt-4o", "GPT-4o", 8000, True, 2024), ("openai/gpt-4o-mini", "GPT-4o mini", 8000, True, 2024),
]

TEXT_EXCLUDE = re.compile(r"whisper|tts|guard|safety|moderat|embed|ocr|rerank|image|lyria|veo|imagen|banana|vision-only|transcri|-vl-|omni-vl|playai|content-safety|arabic", re.I)

def free_member(pid, mid, m):
    """Free-tier membership rule per provider."""
    cost = m.get("cost") or {}
    zero = cost.get("input", 1) == 0 and cost.get("output", 1) == 0
    if TEXT_EXCLUDE.search(mid) or TEXT_EXCLUDE.search(m.get("name", "")):
        return False
    if m.get("modalities", {}).get("output") not in (None, ["text"]):
        return False
    if pid == "openrouter" or pid == "kilo":
        return mid.endswith(":free")
    if pid == "google":
        return any(k in mid for k in ("flash", "gemma")) and "image" not in mid and "live" not in mid and "tts" not in mid and "native-audio" not in mid
    if pid == "zai":
        return mid.endswith("-flash")
    if pid == "cerebras" or pid == "groq" or pid == "nvidia" or pid == "ollama-cloud" or pid == "huggingface" or pid == "cloudflare-workers-ai":
        return True
    if pid == "mistral":
        return not any(k in mid for k in ("embed", "moderation", "ocr", "codestral-embed", "voxtral"))
    if pid == "cohere":
        return mid.startswith("command")
    return False

PARAM_RE = re.compile(r"(\d+(?:\.\d+)?)b(?:-a(\d+(?:\.\d+)?)b)?", re.I)
def params(mid, name):
    """(total_b, active_b) parsed from id/name, else guesses from family words."""
    for text in (mid, name):
        m = PARAM_RE.search(text.replace("_", "-"))
        if m:
            total = float(m.group(1)); active = float(m.group(2)) if m.group(2) else total
            return total, active
    t = (mid + " " + name).lower()
    guesses = [("deepseek-v4-pro", (671, 37)), ("deepseek-v4", (671, 37)), ("deepseek-v3", (671, 37)), ("kimi-k2", (1000, 32)), ("glm-5", (355, 32)), ("glm-4.7", (355, 32)),
               ("glm-4.5-air", (106, 12)), ("glm-4.5", (355, 32)), ("gemini", (200, 40)), ("gemma-4-31b", (31, 31)), ("gemma", (27, 27)), ("minimax-m", (456, 46)),
               ("qwen3-coder-next", (80, 3)), ("qwen3-coder", (480, 35)), ("qwen3.5", (397, 17)), ("mistral-large", (123, 123)), ("mistral-medium", (60, 60)), ("mistral-small", (24, 24)),
               ("magistral-medium", (60, 60)), ("magistral-small", (24, 24)), ("devstral", (24, 24)), ("codestral", (22, 22)), ("ministral", (8, 8)), ("command-a", (111, 111)), ("command-r-plus", (104, 104)), ("command-r", (32, 32)),
               ("gpt-4.1-mini", (30, 30)), ("gpt-4.1", (200, 200)), ("gpt-4o-mini", (30, 30)), ("gpt-4o", (200, 200)), ("gpt-oss-120b", (117, 5)), ("gpt-oss-20b", (21, 4)), ("nemotron-3-ultra", (550, 55)),
               ("nemotron-3-super", (120, 12)), ("nemotron", (30, 3)), ("hermes", (70, 70)), ("laguna-s", (70, 70)), ("laguna-xs", (14, 14)), ("step-3", (196, 38))]
    for k, v in guesses:
        if k in t:
            return v
    return (30.0, 30.0)

def norm(mid):
    s = mid.lower()
    s = re.sub(r"^@cf/", "", s); s = re.sub(r"^[a-z0-9_.-]+/", "", s)
    s = re.sub(r"(:free|-free|:latest|-instruct|-it|-preview|-\d{4}|-\d{2}-\d{4}|:\d+b|-fp8|-latest)$", "", s)
    s = s.replace(":", "-").replace("_", "-")
    return s

def paid_price():
    """normalized name → max paid output $/M across every provider (quality proxy)."""
    out = {}
    for pid, p in CAT.items():
        for mid, m in p.get("models", {}).items():
            c = m.get("cost") or {}
            price = c.get("output") or 0
            if price > 0:
                n = norm(mid); out[n] = max(out.get(n, 0), price)
    return out
PRICE = paid_price()

def quality(pid, mid, m, total_b, active_b):
    size = min(1.0, math.log10(max(total_b, 1)) / math.log10(700))
    price = PRICE.get(norm(mid), 0)
    price_s = min(1.0, math.log1p(price) / math.log1p(12))
    rel = m.get("release_date")
    rec = 0.0
    if rel:
        try:
            d = datetime.date.fromisoformat(rel); age = (TODAY - d).days
            rec = max(0.0, 1.0 - age / 540)
        except ValueError: pass
    ctx = m.get("limit", {}).get("context") or 0
    ctx_s = min(1.0, ctx / 262144)
    reason = 1.0 if m.get("reasoning") else 0.0
    coder = 0.05 if re.search(r"coder|code|devstral|codestral", mid, re.I) else 0.0
    q = 0.35 * size + 0.30 * price_s + 0.15 * rec + 0.10 * reason + 0.10 * ctx_s + coder
    return round(q * 100)

def speed(pid, active_b):
    prior = PROVIDERS[pid][5]
    small = 1.0 - min(1.0, math.log10(max(active_b, 1)) / math.log10(500))
    return round((0.6 * prior + 0.4 * small) * 100)

models = []
for pid, (name, signup, env, base, note, sp, limits) in PROVIDERS.items():
    src = CAT.get(pid, {}).get("models", {})
    if pid == "github-models":
        for mid, mname, ctx, tools, year in GITHUB_MODELS:
            fake = {"name": mname, "release_date": f"{year}-06-01", "tool_call": tools, "reasoning": False, "limit": {"context": ctx}}
            t, a = params(mid, mname)
            models.append(dict(provider=pid, id=mid, name=mname, quality=quality(pid, mid, fake, t, a), speed=speed(pid, a), context=ctx, tools=tools, vision=False, reasoning=False, params_b=t, active_b=a))
        continue
    for mid, m in src.items():
        if not free_member(pid, mid, m):
            continue
        if not m.get("tool_call") and pid in ("nvidia", "huggingface", "ollama-cloud", "cloudflare-workers-ai", "openrouter", "kilo"):
            continue  # agent use needs tools; keep tool-less only where the free tier is tiny anyway
        t, a = params(mid, m.get("name", ""))
        ctx = m.get("limit", {}).get("context") or 32768
        entry = dict(provider=pid, id=mid, name=m.get("name") or mid, quality=quality(pid, mid, m, t, a), speed=speed(pid, a), context=ctx,
                     tools=bool(m.get("tool_call")), vision="image" in (m.get("modalities", {}).get("input") or []), reasoning=bool(m.get("reasoning")), params_b=t, active_b=a)
        if (pid, mid) in LIMITS:
            entry["limits"] = LIMITS[(pid, mid)]
        models.append(entry)

models.sort(key=lambda x: (-x["quality"], -x["speed"], x["provider"], x["id"]))
out = {
    "version": TODAY.isoformat(),
    "method": "membership: per-provider free-tier rules over the catalog snapshot; quality = 0.35*size + 0.30*paid-price-of-same-model + 0.15*recency + 0.10*reasoning + 0.10*context (+coder bonus); speed = 0.6*provider prior + 0.4*small-active-params; limits = published free-tier caps",
    "providers": {pid: {"name": v[0], "signup": v[1], "env": v[2], "base_url": v[3], "note": v[4], "speed": v[5], "limits": v[6]} for pid, v in PROVIDERS.items()},
    "models": models,
}
json.dump(out, open(ROOT / "assets/pool.json", "w"), indent=1)
from collections import Counter
print(len(models), "models", Counter(m["provider"] for m in models))
for m in models[:25]:
    print(f'{m["quality"]:>3} spd{m["speed"]:>3} {m["provider"]:<14} {m["id"]:<50} tools={m["tools"]} ctx={m["context"]}')
