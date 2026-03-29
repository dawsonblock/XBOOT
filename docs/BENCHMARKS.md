# Benchmarks

XBOOT now exposes two benchmark paths:

- `zeroboot fork-bench <workdir> [language]`
  - raw restore and request microbenchmark
- `zeroboot bench <server_url> <api_key> <admin_api_key> [--out-dir <path>]`
  - end-to-end API benchmark for the pooled strict lane

The structured `bench` command exercises `/v1/exec` in two modes:

- `cold_strict`
  - sets pool targets to `0` where configuration allows it
- `warm_pooled_strict`
  - sets pool targets to `1` per available language lane

It runs these scenarios:

- Python: tiny expression, import-heavy script, medium CPU, medium stdout
- Node: tiny expression, module load, medium CPU

And these concurrency levels:

- `1`
- `8`
- `32`
- `128`

Artifacts are written to `artifacts/bench/<timestamp>.json` plus a matching Markdown summary.

Use `scripts/bench_compare.py` to compare two JSON artifacts:

```bash
python3 scripts/bench_compare.py artifacts/bench/<baseline>.json artifacts/bench/<candidate>.json
```
