"""Verify publication and current frozen input bytes, without executing product code."""
from pathlib import Path
import hashlib
import json
import subprocess
from datetime import datetime, timezone

report_dir = Path(__file__).resolve().parent
root = report_dir.parents[3].parent / "nhc-lifecycle-20261116-integration"
original = root / ".local/lifecycle/integrated-full-3"
published = root / "niuma/项目经理/审查记录/2026-11-16生命周期闭环/验证/integrated-full-3"


def read(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def git(*args):
    return subprocess.check_output(["git", *args], cwd=root)


publication = read(published / "publication.json")
manifest = read(original / "manifest.json")
source = read(original / "source-before.json")
checks = {}
artifacts = []
for item in publication["files"]:
    src, dst = original / item["path"], published / item["path"]
    okay = (src.read_bytes() == dst.read_bytes() and src.stat().st_size == item["bytes"]
            and sha(src) == item["source_sha256"] == item["published_sha256"])
    artifacts.append(dict(item, verified=okay))
    checks["published:" + item["path"]] = okay
expected = {x["path"] for x in manifest["artifacts"]} | {"manifest.json"}
checks["publication-exact-set"] = expected == {x["path"] for x in publication["files"]} and len(expected) == len(publication["files"])
checks["publication-directory-set"] = {p.name for p in published.iterdir() if p.is_file()} == expected | {"publication.json"}
checks["candidate"] = publication["candidate"] == manifest["candidate"]
checks["publication-scope"] = publication["full_gate_suite"] and publication["mode"] == "full" and publication["process_tree_cleanup"] == "not_proven_by_this_runner"
checks["manifest-original-published-bytes"] = (original / "manifest.json").read_bytes() == (published / "manifest.json").read_bytes()
checks["source-before-after-bytes"] = (original / "source-before.json").read_bytes() == (original / "source-after.json").read_bytes()
inputs = ["NoManCode", ".cargo", ".gitattributes", "Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"]
checks["source-input-list"] = set(filter(None, git("ls-files", "-z", "--", *inputs).decode().split("\0"))) == {x["path"] for x in source["files"]}
checks["input-status-clean"] = not git("status", "--porcelain=v1", "-z", "--untracked-files=all", "--", *inputs)
observed_head = git("rev-parse", "HEAD").decode().strip()
changed_paths = list(filter(None, git("diff", "--name-only", "-z", manifest["candidate"]["head"], observed_head).decode().split("\0")))
checks["post-run-docs-only"] = all(path.startswith("niuma/") for path in changed_paths)
checks["current-product-tree-equal"] = git("rev-parse", observed_head + ":NoManCode").decode().strip() == manifest["candidate"]["product_tree"]
checks["current-rust-tree-equal"] = git("rev-parse", observed_head + ":NoManCode/rust-app").decode().strip() == manifest["candidate"]["rust_tree"]
files = []
for item in source["files"]:
    path = root / item["path"]
    okay = path.is_file() and path.stat().st_size == item["bytes"] and sha(path) == item["sha256"]
    checks["source-file:" + item["path"]] = okay
    files.append(dict(item, verified=okay))
bridge = read(report_dir / "full3组件等价与A交叉核验.json")
checks["previous-bridge-candidate"] = all(bridge["full3_candidate"][k] == v for k, v in manifest["candidate"].items())
for item in bridge["bridge"]:
    checks["component-bridge:" + item["path"]] = sha(root / item["path"]) == item["matrix"]["sha256"] == item["full3"]["sha256"]
report = dict(checked_utc=datetime.now(timezone.utc).isoformat(), candidate=manifest["candidate"],
              publication_sha256=sha(published / "publication.json"), manifest_sha256=sha(original / "manifest.json"),
              original_scope="62 declared artifacts plus manifest; manager closure is separate",
              published_count=len(artifacts), source_count=len(files), checks=checks,
              mismatches=[k for k, v in checks.items() if not v], published_files=artifacts, source_files=files,
              no_product_execution=True, no_source_mutation=True, observed_head=observed_head, post_run_changed_paths=changed_paths, prior_attempt="full-3发布与最终源码核验.json retains the initial frozen-head mismatch after manager merged documentation")
with (report_dir / "full-3发布与最终源码核验-v2.json").open("x", encoding="utf-8", newline="\n") as stream:
    json.dump(report, stream, ensure_ascii=False, indent=2)
    stream.write("\n")
print(json.dumps({"checks": len(checks), "published_count": len(artifacts), "source_count": len(files), "mismatches": report["mismatches"]}))
raise SystemExit(0 if not report["mismatches"] else 1)
