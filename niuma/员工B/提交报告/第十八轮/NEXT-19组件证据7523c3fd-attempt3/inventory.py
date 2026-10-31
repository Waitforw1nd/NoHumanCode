import hashlib, json, os
from pathlib import Path
run = Path(__file__).resolve().parent
repo = run.parents[3]
roots = [run / "tmp", repo / ".local" / "next17-a10-evidence", repo / "NoManCode/rust-app/.local/next19-b-11e1882b-attempt1/tmp", repo / "NoManCode/rust-app/.local/next19-b-aac917d4-attempt2/tmp"]
entries, errors = [], []
for root in roots:
    for current, dirs, files in os.walk(root, followlinks=False):
        dirs.sort()
        for name in sorted(files):
            path = Path(current) / name
            rel = path.relative_to(repo).as_posix()
            try:
                if path.is_symlink():
                    entries.append({"path": rel, "symlink": os.readlink(path)})
                    continue
                h = hashlib.sha256()
                with path.open("rb") as handle:
                    for block in iter(lambda: handle.read(1024 * 1024), b""):
                        h.update(block)
                entries.append({"path":rel,"bytes":path.stat().st_size,"sha256":h.hexdigest()})
            except OSError as exc:
                errors.append({"path":rel,"error":type(exc).__name__})
output = {"roots":[p.relative_to(repo).as_posix() for p in roots],"files":entries,"errors":errors}
import sys
(run / "logs" / (sys.argv[1] + ".json")).write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(json.dumps({"files":len(entries),"errors":errors},ensure_ascii=False))
