# Merge raw per-sample values (run-*/rep-1/latency.json: samples[key].values)
# into the bench baseline JSON so tools/bench/ab-compare.py can score the
# pooled trimmed mean. Also corrects the arm's commit label: the bench records
# the worktree HEAD, which is wrong for the base arm (its binary is 05ba66b).
import json, sys
src, lat, out, arm_commit = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
base = json.load(open(src))
samples = json.load(open(lat))["samples"]
for key, block in base["metrics"].items():
    vals = (samples.get(key) or {}).get("values")
    if vals:
        block["samples"] = vals
base["commit"] = arm_commit
base["commit_note"] = "bench arm binary built from this commit; the bench's own commit field is the worktree HEAD"
json.dump(base, open(out, "w"), indent=1)
print(out, "metrics with samples:", sum(1 for b in base["metrics"].values() if b.get("samples")))
