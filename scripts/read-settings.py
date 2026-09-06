"""Dump the active config from the app's SQLite settings table as JSON.

输出字段与 REST POST /api/config 的请求体一致（camelCase），值不脱敏，
仅供本机 e2e 测试读取使用；stdout 不应重定向到不受信任的位置。
"""
import json
import os
import sqlite3
import sys

DB = os.path.expanduser(r"~\ImageClient\image-client.db")

con = sqlite3.connect(DB)
row = con.execute("SELECT value FROM settings WHERE key='settings'").fetchone()
con.close()

d = json.loads(row[0]) if row else {}
configs = d.get("configs", [])
active = next((c for c in configs if c.get("id") == d.get("activeId")), None) or (
    configs[0] if configs else {}
)
img = active.get("image", {})
vid = active.get("video", {})

out = {
    "imageApiUrl": img.get("url", ""),
    "imageApiKey": img.get("key", ""),
    "imageApiModel": img.get("model") or "gpt-image-2",
    "videoApiUrl": vid.get("url", ""),
    "videoApiKey": vid.get("key", ""),
    "videoApiModel": vid.get("model") or "kling-video-v3",
    "llmApiUrl": d.get("llmUrl", ""),
    "llmApiKey": d.get("llmKey", ""),
    "llmApiModel": d.get("llmModel") or "gemini-3.7-flash",
    "outputDir": d.get("outputDir", ""),
}
json.dump(out, sys.stdout, ensure_ascii=False)
