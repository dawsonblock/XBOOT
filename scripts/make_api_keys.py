#!/usr/bin/env python3
import argparse
import hashlib
import hmac
import json
import secrets
import time
import uuid
from pathlib import Path


def build_record(label: str, pepper: str, prefix_prefix: str):
    key_id = f"key_{uuid.uuid4().hex[:8]}"
    prefix = f"{prefix_prefix}{uuid.uuid4().hex[:8]}"
    secret = secrets.token_urlsafe(24)
    token = f"{prefix}.{secret}"
    digest = hmac.new(
        pepper.encode("utf-8"),
        f"{prefix}:{secret}".encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()
    return token, {
        "id": key_id,
        "prefix": prefix,
        "hash": digest,
        "created_at": int(time.time() * 1000),
        "disabled_at": None,
        "label": label,
    }


parser = argparse.ArgumentParser()
parser.add_argument("--count", type=int, default=1)
parser.add_argument("--prefix", default="zb_live_")
parser.add_argument("--output", default="api_keys.json")
parser.add_argument("--pepper-file")
parser.add_argument("--pepper")
parser.add_argument("--label-prefix", default="generated")
args = parser.parse_args()

pepper = args.pepper
if args.pepper_file:
    pepper = Path(args.pepper_file).read_text().strip()
if not pepper:
    raise SystemExit("provide --pepper or --pepper-file")

tokens = []
records = []
for index in range(args.count):
    token, record = build_record(f"{args.label_prefix}-{index + 1}", pepper, args.prefix)
    tokens.append(token)
    records.append(record)

Path(args.output).write_text(json.dumps(records, indent=2) + "\n")
print(f"wrote {len(records)} hashed API key records to {args.output}")
print("bearer tokens (shown once):")
for token in tokens:
    print(token)
