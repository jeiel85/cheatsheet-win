"""Regenerates the model tables in README.md / README.en.md from data/profiles.json.
Run: python tools/update-readme-models.py   (no third-party packages)"""
import json, os, re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SITE = "https://jeiel85.github.io/cheatsheet-win/"

with open(os.path.join(ROOT, "data", "profiles.json"), encoding="utf-8") as f:
    data = json.load(f)
profiles = sorted(data["profiles"], key=lambda p: (p["vendor"], p["name"]))


def codes(p):
    m = p.get("match", {})
    out = [re.sub(r"[\\^$()?:\[\]]", "", c).replace("|", "/") for c in m.get("product", [])]
    out += ["MT " + c for c in m.get("mtm", [])]
    if not out:
        out = [re.sub(r"[\\^$()?:\[\]]", "", c).replace("|", "/") for c in m.get("family", [])]
    s = ", ".join(out)
    return s if len(s) <= 90 else s[:87] + "…"


def table(ko):
    head = ("| 제조사 | 모델 | 연식 | 자동 인식 기준 | 확인 |\n|---|---|---|---|:-:|\n" if ko else
            "| Vendor | Model | Years | Detected by | Verified |\n|---|---|---|---|:-:|\n")
    rows = []
    for p in profiles:
        ok = "🟡" if p.get("verified") == "partial" else "✅"
        rows.append("| %s | [%s](%s#m-%s) | %s | %s | %s |" % (
            p["vendor"], p["name"].replace("|", "/"), SITE, p["id"], p.get("years", ""), codes(p), ok))
    asus = sum(1 for p in profiles if p["vendor"] == "ASUS")
    summary = ("**%d개 프로필** (ASUS %d · Lenovo %d) · 데이터 %s · ✅ 공식 설명서로 전체 확인 🟡 일부 확인\n\n" if ko else
               "**%d profiles** (ASUS %d · Lenovo %d) · data %s · ✅ fully checked against the official manual 🟡 partly verified\n\n") % (
        len(profiles), asus, len(profiles) - asus, data.get("dataVersion", ""))
    return summary + head + "\n".join(rows) + "\n"


for name, ko in (("README.md", True), ("README.en.md", False)):
    path = os.path.join(ROOT, name)
    with open(path, encoding="utf-8") as f:
        text = f.read()
    new = re.sub(r"(<!-- models:start -->\n).*?(<!-- models:end -->)", lambda m: m.group(1) + table(ko) + m.group(2), text, flags=re.S)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(new)
    print("updated", name)
