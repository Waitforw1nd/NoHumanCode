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
snapshot. This version intentionally has no arbitrary feature/config/command flags;
ambient Cargo configuration and environment may still change build behavior.
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

`ambient-inputs.json` fingerprints relevant inherited environment values and
Cargo configuration files without recording their contents; the files are checked
again after each build. Each invocation also records the discovered MSVC/SDK and
command paths plus command-file hashes after environment setup. A rustup shim or
wrapper may select other tools; the records do not attest the effective compiler,
all build inputs, or reproducibility. They may expose local paths. `pair.json` is
self-described evidence, never an authentication authority.

The Windows integration test `tests/worker_package_bootstrap.rs` exercises the
production bootstrap in isolated copied test executables. Its ignored real-pair
case uses a prepared pair, a separately fixed real worker with different bytes,
and an unbound Host; no `pair.json` is used as an oracle. Only the manager runs
these process tests in the supervised window. After candidate integration, set
the three paths below to approved, fixed inputs on E and run from the repository
root, using the full candidate Git IDs and a new attempt name:

```powershell
$env:NHC_TEST_PAIR_A = 'E:\path\to\pair-a'
$env:NHC_TEST_FOREIGN_WORKER = 'E:\path\to\baseline\peachsh-wasm-worker.exe'
$env:NHC_TEST_UNBOUND_HOST = 'E:\path\to\unbound\peachsh.exe'
python -B NoManCode/rust-app/tools/worker-package/run-bootstrap-check.py `
  --head <full-head> --product-tree <full-product-tree> --rust-tree <full-rust-tree> `
  --attempt real-pair-attempt1 --step-timeout-seconds 300
```

The harness records input paths and SHA256 in the retained temporary fixture
directory, prints its location, copies artifacts into isolated case directories,
and uses isolated data and workspace paths. The gate runner checks source IDs,
requires a passing test, and retains its gate and evidence. Each original child
is owned by `tests/support/owned_child.rs`, with no window, file-backed logs and
a 30-second observation deadline; ordinary failure/unwind invokes its exact-child kill/wait
guard. Timeout or uncertain observation writes unknown evidence, emits
`NHC_TERMINAL_UNKNOWN` and retains that original Child indefinitely. Do not kill
the test or supervisor, start another step, unlock, remove the fixture or infer
descendant cleanup from direct exit; the manager reconciles the same objects and
gate. An outer deadline likewise stops admission while the original supervisor
retains the Cargo process. Neither direct exit nor this test proves descendant
cleanup. Protocol and public `run_wasm` execution remain separate gates.

Cross-review found that the baseline shared `owned_child` accepts a direct exit
first observed after the deadline as success. C's fix `bf1bda27` must be
integrated and verified before treating the deadline as strictly enforced.

The non-ignored independent bootstrap case can be selected through the existing
`run-gates.ps1 -Mode targeted -TestTarget worker_package_bootstrap -Filter
packaged_bootstrap_rejects_mismatch_and_pins_fixed_copy` with candidate IDs,
a fresh attempt and an appropriate outer deadline. The child's harness output
must show exactly one passing test per case.

Pure mock tests, safe while the manager owns the real build window:

```powershell
python -B NoManCode/rust-app/tools/worker-package/test_worker_package.py
```

Tests mock every build process; they exercise actual byte copies/new-directory
publication under this worktree's `.local`. No Cargo, real Host/worker, install,
release or global gate is invoked. Mocking release command selection is not a
release build result.
