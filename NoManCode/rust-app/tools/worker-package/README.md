# Prepare a worker/Host pair

This separate W3 tool prepares independent local artifacts with verified hashes. It does not install them,
change a current package, run Host/worker, enable `run_wasm`, or prove runtime
binding. The lifecycle runner and CLI/HTTP validation remain a separate candidate.

The narrow B/D contract is one compile-time input: **NHC_PACKAGED_WORKER_SHA256**,
64 lower-case hex characters, consumed via `option_env!` only in Host `src/main.rs`.
The protocol version stays in `src/wasm_worker_protocol.rs`; there is no runtime or
compile-time protocol environment override. A lexical check rejects a missing,
duplicate, library or worker digest consumer before any build. Comments and string
examples are excluded. This check does not prove macro expansion, actual bootstrap
use or the installed binary's identity; independent production tests must do that.
The current lifecycle baseline has no Host digest consumer and therefore fails
closed until the separately reviewed B bootstrap exists. The attempt retains a
failed `preflight.json` with `build_started: false` and no compiler invocation.

When the manager has authorized a later real build, from the repository root:

```powershell
# Set the already installed MSVC/SDK roots in the parent process if discovery
# needs them; see ../lifecycle/README.md and the local validation report.
pwsh -NoProfile -File NoManCode/rust-app/tools/worker-package/package-worker.ps1 `
  -Head <approved-head> -ProductTree <approved-product-tree> -RustTree <approved-rust-tree> `
  -Pair candidate-attempt1 -Profile debug
```

No build is authorized merely by this example. `debug` is the default; `release`
exists as an explicit future selection. Both bins use the same fixed Windows MSVC
target, profile, default Cargo features, locked/offline dependencies and source
snapshot. This version intentionally has no arbitrary feature/config/command flags.
It reuses discovered MSVC/SDK setup and jobs2/debug0/incremental0 from lifecycle
tooling. Relative paths resolve from the repository, all active outputs to E.

The sequence is:

1. Exclusively create the shared `<git-common-dir>/nhc-heavy-window.lock`, verify
   the exact approved HEAD/dual trees, clean tracked/untracked build inputs and raw
   source hashes. Keep this gate even on success for manager reconciliation.
2. Build only `peachsh-wasm-worker`, without an inherited packaged digest. Select
   its unique executable from this invocation's Cargo JSON and exact source path;
   require successful `build-finished`. No scan of old target artifacts.
3. Copy independent worker bytes to a new `.staging-<Pair>` directory. Verify
   source/copy SHA256 and single-link status. Freeze this copy before calculating
   the digest for the Host build.
4. Build only `peachsh`, with the digest scoped to that invocation. A shared
   library/worker must not consume it. Copy the exact Host artifact, then verify
   the frozen worker again, both pair files, source hashes and protocol constant.
5. Write `pair.json` and atomically rename the complete stage to a new final pair
   directory. Existing pairs and destination races are never overwritten. There
   is no current-pointer switch or installation. Failure retains partial staging,
   logs and the gate; it does not create a usable half-pair or change an old pair.

Defaults: `.local/worker-packages/<Pair>`,
`.local/worker-package-evidence/<Pair>`, `.local/lifecycle-target` and
`<evidence>/tmp`. Override with `-PackageRoot/-EvidenceRoot/-TargetDir`. Staging
and pair must share a parent; evidence, staging, pair and mutable target cannot
overlap. Every attempt/name is new, never reused. Do not delete a retained gate
or use PID absence to call cleanup complete.

Each build uses the lifecycle supervisor's total deadline (default 1800 seconds).
Timeout/interrupt/terminal-unknown stops further builds while the same supervisor
retains the original Process object until its direct terminal state. Persistent
gate retention still does not prove descendant cleanup. A prepared pair records
`runtime_binding_verified: false`; artifact hashes and recorded build settings
cannot substitute for Host runtime pin/identity/protocol/fail-closed acceptance.

Pure mock tests, safe while the manager owns the real build window:

```powershell
python -B NoManCode/rust-app/tools/worker-package/test_worker_package.py
```

Tests mock every build process; they exercise actual byte copies/new-directory
publication under this worktree's `.local`. No Cargo, real Host/worker, install,
release or global gate is invoked. Mocking release command selection is not a
release build result.
