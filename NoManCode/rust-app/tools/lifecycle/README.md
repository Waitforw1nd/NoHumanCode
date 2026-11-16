# Lifecycle validation

Run from any cwd using `run-gates.ps1`. All relative arguments resolve from the
repository containing this script, not the invoking shell. Python 3.10+ and the
existing PowerShell/Rust/Node tools are used; nothing is installed. MSVC/SDK
discovery comes from `build.ps1 -Action environment`, inside the acquired window.
Existing `VCToolsInstallDir` / `WindowsSdkDir` values are honored.

From the repository root, after the manager has fixed the committed candidate:

```powershell
$candidate = git rev-parse HEAD
$product = git rev-parse HEAD:NoManCode
$rust = git rev-parse HEAD:NoManCode/rust-app
pwsh -NoProfile -File NoManCode/rust-app/tools/lifecycle/run-gates.ps1 `
  -Head $candidate -ProductTree $product -RustTree $rust `
  -Attempt check-attempt1 -Mode check -StepTimeoutSeconds 1800
```

The expected full SHAs are mandatory and checked before commands start. For an
externally approved candidate, pass the approved SHAs rather than dynamically
substituting a newer HEAD. `check` only checks workspace/all targets. `targeted`
checks, builds debug bins, then runs each `-TestTarget lifecycle_compat` (optionally
`-Filter cancel`). `full` runs the six fixed gates, with optional explicit targets
between debug-bin build and the full tests. No ignored/live/release steps are added.
An explicit target matching zero passing tests fails validation.

Additional explicit modes: `-Mode lib -Filter process_lifetime_tests::cancel`
runs filtered library tests; `-Mode test-build -TestTarget lifecycle_compat`
compiles that integration target with `--no-run`; `-Mode example-build -Example
process_lifetime_probe` builds the named example without starting it. All three
retain check/debug-bin prerequisites. No mode accepts arbitrary commands or
enables ignored tests. A process matrix may instead use its separately reviewed
supervisor inside the manager's same exclusive window.

Default paths are repository-relative `.local/lifecycle/<Attempt>`,
`.local/lifecycle-target`, and `<Attempt>/tmp`. `-EvidenceRoot` / `-TargetDir`
override them. Resolved paths, shared Git directory and repository must be on E.
Output must be outside NoManCode; the runner creates the ignored Rust cwd `target`
directory solely for legacy `tempdir_in("target")` fixtures. Environment enforces
offline, locked commands, jobs=2, dev/test debug=0, incremental=0, test threads=1,
TMP=TEMP on E, and no automatic rustup installation.

The global exclusive-create gate is `<git-common-dir>/nhc-heavy-window.lock`.
Every attempt, successful or failed, **retains this gate**. Before the next attempt,
the manager must reconcile the original session and actual process cleanup proofs,
then archive that exact lock to a new name. There is no automatic stale-PID/age
recovery, force unlock, process-name kill, or rerun-until-green. An existing lock or
attempt is never overwritten. Do not bypass the gate with the internal step script.

Each step has a total validation deadline (default 1800 seconds; configurable
1..7200). Timeout, Ctrl-C, observation errors, or a child's literal
`NHC_TERMINAL_UNKNOWN` marker permanently fail this attempt and prevent subsequent
steps. The **same supervisor keeps the original Process object and log open** and
waits for its direct terminal state. This retention wait is deliberately not another
deadline that would discard ownership. Do not kill the supervisor to make it return;
keep its original execution session available for manager reconciliation. Forced OS
termination cannot be made safe by a Python runner; the persistent gate survives it.
Even an ordinary exit 0 does not prove descendants or remote effects have ended.

Evidence uses new files only: source-before/after (HEAD, dual trees, clean state,
per-file raw SHA256), immutable step-start/result/direct-exit files, raw logs and
their exact sizes/hashes, and a final manifest. A missing result/manifest is
incomplete, not passing. The JSON has no PID-based cleanup assertion. Paths in raw
logs are left intact, so original and published bytes remain identical.

After reviewing a passing attempt, verify/copy without replacing prior evidence:

```powershell
python -B NoManCode/rust-app/tools/lifecycle/publish_evidence.py `
  --attempt-dir .local/lifecycle/check-attempt1 `
  --destination niuma/项目经理/审查记录/2026-11-16生命周期闭环/check-attempt1
```

Publication requires the **same HEAD**, both trees, clean input status, raw source
hashes, exact enumerated plan, exit codes and log hashes. A check-only publication
is explicitly not the full gate suite. Publication is evidence verification, not
product acceptance or process-tree cleanup. Failed/unknown attempt originals remain
available; this publisher refuses to turn them into passing evidence.

Lightweight mock tests (no Cargo or product processes):

```powershell
python -B NoManCode/rust-app/tools/lifecycle/test_lifecycle_gates.py
```

These use an independent temporary mock gate under this tree's `.local`, never the
real shared gate. Actual Cargo, native process supervision and CLI regressions still
require the manager's exclusive real validation window.
