#!/usr/bin/env python3
import argparse
import json
import secrets
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--count', type=int, default=1)
parser.add_argument('--prefix', default='zb_live_')
parser.add_argument('--output', default='api_keys.json')
args = parser.parse_args()
keys = [args.prefix + secrets.token_urlsafe(24) for _ in range(args.count)]
Path(args.output).write_text(json.dumps(keys, indent=2) + '\n')
print(f'wrote {len(keys)} keys to {args.output}')
