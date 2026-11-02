// Review candidate only. Do not compile or run without a separate signed window.
// Captures only case16 handle metadata; never reads process memory or controls work.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <processsnapshot.h>
#include <jobapi2.h>
#include <cstdint>
#include <cstdio>
#include <cwchar>
#include <cwctype>
#include <map>
#include <string>
#include <vector>

constexpr DWORD kCarrierPid = 89780;
constexpr unsigned long long kCarrierCreation = 134381029069968888ULL;
constexpr size_t kMaxEntries = 4096;
constexpr size_t kMaxCandidates = 256;

struct Entry {
    HANDLE value = nullptr;
    PSS_OBJECT_TYPE type = PSS_OBJECT_TYPE_UNKNOWN;
    std::wstring kind;
    bool specific = false;
    DWORD processId = 0;
    DWORD threadId = 0;
};
struct Candidate {
    Entry entry;
    HANDLE first = nullptr;
    HANDLE second = nullptr;
    HANDLE third = nullptr;
};
using Table = std::map<uintptr_t, Entry>;
struct Counts {
    size_t walked = 0, process = 0, thread = 0, job = 0, other = 0;
    size_t noTypeName = 0, duplicateValue = 0, snapshotErrors = 0;
    size_t abMissing = 0, abMismatch = 0, cMissing = 0, cMismatch = 0;
    size_t duplicateFailures = 0, comparisonFailures = 0, queryFailures = 0, identityMismatches = 0;
    size_t duplicates = 0, validated = 0;
} g;
using CompareObjectHandlesFn = BOOL (WINAPI*)(HANDLE, HANDLE);
static CompareObjectHandlesFn sameObject = nullptr;

static bool resolveCompareObjectHandles() {
    HMODULE module = GetModuleHandleW(L"KernelBase.dll");
    if (!module) { std::printf("error stage=GetModuleHandle_KernelBase code=%lu\n", (unsigned long)GetLastError()); return false; }
    wchar_t systemDir[MAX_PATH]{}, modulePath[MAX_PATH]{};
    UINT sysLength = GetSystemDirectoryW(systemDir, MAX_PATH);
    DWORD pathLength = GetModuleFileNameW(module, modulePath, MAX_PATH);
    if (!sysLength || sysLength >= MAX_PATH || !pathLength || pathLength >= MAX_PATH) {
        std::puts("error stage=KernelBase_path_unavailable"); return false;
    }
    const std::wstring expected = std::wstring(systemDir) + L"\\KernelBase.dll";
    if (_wcsicmp(modulePath, expected.c_str()) != 0) { std::puts("error stage=KernelBase_path_mismatch"); return false; }
    sameObject = reinterpret_cast<CompareObjectHandlesFn>(GetProcAddress(module, "CompareObjectHandles"));
    if (!sameObject) { std::printf("error stage=GetProcAddress_CompareObjectHandles code=%lu\n", (unsigned long)GetLastError()); return false; }
    std::puts("compare_api resolved_from=system_KernelBase.dll");
    return true;
}

static unsigned long long filetime64(FILETIME f) {
    return (static_cast<unsigned long long>(f.dwHighDateTime) << 32) | f.dwLowDateTime;
}
static void error(const char* stage, DWORD value) {
    std::printf("error stage=%s code=%lu\n", stage, static_cast<unsigned long>(value));
    std::fflush(stdout);
}
static bool carrier_identity(HANDLE h) {
    FILETIME c{}, e{}, k{}, u{};
    if (GetProcessId(h) != kCarrierPid || !GetProcessTimes(h, &c, &e, &k, &u)) {
        error("carrier_identity", GetLastError()); return false;
    }
    DWORD wait = WaitForSingleObject(h, 0);
    std::printf("carrier pid=%lu creation=%llu wait=%lu\n",
                static_cast<unsigned long>(kCarrierPid), filetime64(c), static_cast<unsigned long>(wait));
    std::fflush(stdout);
    return filetime64(c) == kCarrierCreation && wait == WAIT_TIMEOUT;
}
static bool snapshot(HANDLE carrier, const char* label, Table& out) {
    if (!carrier_identity(carrier)) return false;
    HPSS snap = nullptr;
    const auto flags = static_cast<PSS_CAPTURE_FLAGS>(PSS_CAPTURE_HANDLES |
        PSS_CAPTURE_HANDLE_NAME_INFORMATION | PSS_CAPTURE_HANDLE_TYPE_SPECIFIC_INFORMATION);
    DWORD rc = PssCaptureSnapshot(carrier, flags, 0, &snap);
    if (rc != ERROR_SUCCESS) { ++g.snapshotErrors; error("PssCaptureSnapshot", rc); return false; }
    HPSSWALK marker = nullptr;
    rc = PssWalkMarkerCreate(nullptr, &marker);
    if (rc != ERROR_SUCCESS) { ++g.snapshotErrors; error("PssWalkMarkerCreate", rc); PssFreeSnapshot(GetCurrentProcess(), snap); return false; }
    size_t walked = 0;
    size_t localProcess = 0, localThread = 0, localJob = 0, localOther = 0, localNoTypeName = 0;
    bool ok = true;
    for (;;) {
        PSS_HANDLE_ENTRY item{};
        rc = PssWalkSnapshot(snap, PSS_WALK_HANDLES, marker, &item, sizeof(item));
        if (rc == ERROR_NO_MORE_ITEMS) break;
        if (rc != ERROR_SUCCESS || ++walked > kMaxEntries) { ++g.snapshotErrors; error("PssWalkSnapshot_or_limit", rc); ok = false; break; }
        ++g.walked;
        std::wstring typeName;
        if (item.TypeName && item.TypeNameLength && item.TypeNameLength % sizeof(wchar_t) == 0)
            typeName.assign(item.TypeName, item.TypeNameLength / sizeof(wchar_t));
        std::wstring kind;
        if ((item.Flags & PSS_HANDLE_HAVE_TYPE) && item.ObjectType == PSS_OBJECT_TYPE_PROCESS) kind = L"Process";
        else if ((item.Flags & PSS_HANDLE_HAVE_TYPE) && item.ObjectType == PSS_OBJECT_TYPE_THREAD) kind = L"Thread";
        else if (typeName == L"Job") kind = L"Job";
        if (kind.empty()) { ++g.other; ++localOther; if (typeName.empty()) { ++g.noTypeName; ++localNoTypeName; } continue; } // Never deliberately duplicate unknown/socket handles.
        if (kind == L"Process") { ++g.process; ++localProcess; } else if (kind == L"Thread") { ++g.thread; ++localThread; } else { ++g.job; ++localJob; }
        Entry e; e.value = item.Handle; e.type = item.ObjectType; e.kind = kind;
        e.specific = (item.Flags & PSS_HANDLE_HAVE_TYPE_SPECIFIC_INFORMATION) != 0;
        if (e.specific && kind == L"Process") e.processId = item.TypeSpecificInformation.Process.ProcessId;
        if (e.specific && kind == L"Thread") {
            e.processId = item.TypeSpecificInformation.Thread.ProcessId;
            e.threadId = item.TypeSpecificInformation.Thread.ThreadId;
        }
        std::printf("snapshot_candidate label=%s value=%llu kind=%ls specific=%d process_id=%lu thread_id=%lu\n",
                    label, static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(item.Handle)), kind.c_str(), e.specific ? 1 : 0,
                    static_cast<unsigned long>(e.processId), static_cast<unsigned long>(e.threadId));
        if (!out.emplace(reinterpret_cast<uintptr_t>(item.Handle), e).second) { ++g.duplicateValue; ok = false; break; }
        if (out.size() > kMaxCandidates) { ++g.snapshotErrors; error("candidate_limit", ERROR_BUFFER_OVERFLOW); ok = false; break; }
    }
    DWORD markerFree = PssWalkMarkerFree(marker);
    DWORD snapshotFree = PssFreeSnapshot(GetCurrentProcess(), snap);
    std::printf("snapshot label=%s walked=%llu candidates=%llu walk_rc=%lu marker_free=%lu snapshot_free=%lu\n",
                label, static_cast<unsigned long long>(walked), static_cast<unsigned long long>(out.size()),
                static_cast<unsigned long>(rc), static_cast<unsigned long>(markerFree), static_cast<unsigned long>(snapshotFree));
    std::printf("snapshot_types label=%s process=%llu thread=%llu job=%llu other=%llu no_type_name=%llu\n", label,
                (unsigned long long)localProcess, (unsigned long long)localThread, (unsigned long long)localJob,
                (unsigned long long)localOther, (unsigned long long)localNoTypeName);
    std::fflush(stdout);
    if (markerFree != ERROR_SUCCESS || snapshotFree != ERROR_SUCCESS) ++g.snapshotErrors;
    return ok && markerFree == ERROR_SUCCESS && snapshotFree == ERROR_SUCCESS && carrier_identity(carrier);
}
static bool compatible(const Entry& a, const Entry& b) {
    if (a.kind != b.kind || a.type != b.type || a.specific != b.specific) return false;
    if (a.specific && (a.processId != b.processId || a.threadId != b.threadId)) return false;
    return true;
}
static bool duplicate(HANDLE carrier, HANDLE value, HANDLE* result) {
    *result = nullptr;
    return DuplicateHandle(carrier, value, GetCurrentProcess(), result, 0, FALSE, DUPLICATE_SAME_ACCESS) != FALSE;
}
static void print_counts() {
    std::printf("COUNTS walked=%llu process=%llu thread=%llu job=%llu other=%llu no_type_name=%llu duplicate_value=%llu snapshot_errors=%llu ab_missing=%llu ab_mismatch=%llu c_missing=%llu c_mismatch=%llu duplicate_failures=%llu comparison_failures=%llu query_failures=%llu identity_mismatches=%llu duplicates=%llu validated=%llu\n",
        (unsigned long long)g.walked, (unsigned long long)g.process, (unsigned long long)g.thread, (unsigned long long)g.job,
        (unsigned long long)g.other, (unsigned long long)g.noTypeName, (unsigned long long)g.duplicateValue, (unsigned long long)g.snapshotErrors,
        (unsigned long long)g.abMissing, (unsigned long long)g.abMismatch, (unsigned long long)g.cMissing, (unsigned long long)g.cMismatch,
        (unsigned long long)g.duplicateFailures, (unsigned long long)g.comparisonFailures, (unsigned long long)g.queryFailures, (unsigned long long)g.identityMismatches,
        (unsigned long long)g.duplicates, (unsigned long long)g.validated);
}
[[noreturn]] static void park_unknown(const char* reason) {
    print_counts();
    std::printf("RELEASE_UNKNOWN reason=%s retain_all_duplicates=1\n", reason);
    std::fflush(stdout);
    for (;;) Sleep(1000);
}
static void await_release(HANDLE carrier, std::vector<Candidate>& candidates, const wchar_t* releasePath) {
    std::puts("QUERY_COMPLETE: retain all duplicates and carrier; waiting for attempt-specific release.request");
    std::fflush(stdout);
    for (;;) {
        DWORD attributes = GetFileAttributesW(releasePath);
        if (attributes != INVALID_FILE_ATTRIBUTES) {
            if (attributes & FILE_ATTRIBUTE_DIRECTORY) park_unknown("release_path_is_directory");
            break;
        }
        DWORD last = GetLastError();
        if (last != ERROR_FILE_NOT_FOUND && last != ERROR_PATH_NOT_FOUND) park_unknown("release_path_query_failed");
        Sleep(1000);
    }
    if (!carrier_identity(carrier)) park_unknown("carrier_identity_or_liveness");
    std::vector<HANDLE> verification;
    verification.reserve(candidates.size());
    for (auto& v : candidates) {
        if (!v.first && !v.second && !v.third) continue;
        HANDLE verify = nullptr;
        if (!duplicate(carrier, v.entry.value, &verify)) { error("release_duplicate", GetLastError()); park_unknown("source_duplicate_failed"); }
        verification.push_back(verify); // Hold even mismatched temporary duplicates.
        ++g.duplicates;
        bool same = (!v.first || sameObject(v.first, verify)) &&
                    (!v.second || sameObject(v.second, verify)) &&
                    (!v.third || sameObject(v.third, verify));
        std::printf("release_source value=%llu verification_copy=%llu compare_all=%d\n", static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(v.entry.value)), static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(verify)), same ? 1 : 0);
        if (!same) park_unknown("source_object_changed");
    }
    if (!carrier_identity(carrier)) park_unknown("carrier_changed_after_verification");
    std::puts("RELEASE_VERIFIED: source slot comparisons passed; closing diagnostic-owned copies only; this is not a last-close proof");
    auto closeLocal = [](HANDLE h) {
        if (!h) return;
        BOOL ok = CloseHandle(h); DWORD last = ok ? 0 : GetLastError();
        std::printf("release_close copy=%llu ok=%d error=%lu\n", (unsigned long long)reinterpret_cast<uintptr_t>(h), ok, (unsigned long)last);
        if (!ok) park_unknown("close_local_copy_failed");
    };
    for (auto& v : candidates) { closeLocal(v.first); closeLocal(v.second); closeLocal(v.third); }
    for (HANDLE h : verification) closeLocal(h);
    closeLocal(carrier);
    std::puts("RELEASE_COMPLETE");
}
static int run(int argc, wchar_t** argv) {
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    if (argc != 2 || wcslen(argv[1]) < 4 || towupper(argv[1][0]) != L'E' || argv[1][1] != L':' || argv[1][2] != L'\\') {
        error("expected_absolute_E_release_path", ERROR_INVALID_PARAMETER); return 1;
    }
    if (!resolveCompareObjectHandles()) return 8; // Fail before opening the carrier or duplicating handles.
    HANDLE carrier = OpenProcess(PROCESS_DUP_HANDLE | PROCESS_QUERY_INFORMATION | SYNCHRONIZE, FALSE, kCarrierPid);
    if (!carrier) { error("OpenProcess_carrier", GetLastError()); return 2; }
    if (!carrier_identity(carrier)) { CloseHandle(carrier); return 3; }
    Table a, b, c;
    if (!snapshot(carrier, "A", a) || !snapshot(carrier, "B", b)) { print_counts(); CloseHandle(carrier); return 4; }
    std::vector<Candidate> candidates;
    candidates.reserve(kMaxCandidates);
    bool haveDuplicate = false;
    for (const auto& pair : a) {
        auto it = b.find(pair.first);
        if (it == b.end()) { ++g.abMissing; continue; }
        if (!compatible(pair.second, it->second)) { ++g.abMismatch; continue; }
        candidates.emplace_back();
        Candidate& v = candidates.back();
        v.entry = pair.second; // Allocate/copy before retaining any new object.
        if (!carrier_identity(carrier)) { if (haveDuplicate) { print_counts(); std::puts("QUERY_PARTIAL reason=carrier_identity_during_copy"); await_release(carrier, candidates, argv[1]); return 5; } CloseHandle(carrier); return 5; }
        if (!duplicate(carrier, v.entry.value, &v.first)) { ++g.duplicateFailures; error("duplicate_first", GetLastError()); continue; }
        haveDuplicate = true;
        ++g.duplicates;
        std::printf("duplicate source=%llu copy=%llu round=1\n", (unsigned long long)reinterpret_cast<uintptr_t>(v.entry.value), (unsigned long long)reinterpret_cast<uintptr_t>(v.first));
        if (!duplicate(carrier, v.entry.value, &v.second)) {
            ++g.duplicateFailures; error("duplicate_second", GetLastError()); continue;
        }
        ++g.duplicates;
        std::printf("duplicate source=%llu copy=%llu round=2\n", (unsigned long long)reinterpret_cast<uintptr_t>(v.entry.value), (unsigned long long)reinterpret_cast<uintptr_t>(v.second));
    }
    for (const auto& pair : b) if (a.find(pair.first) == a.end()) ++g.abMissing;
    if (!snapshot(carrier, "C", c)) { print_counts(); if (haveDuplicate) { std::puts("QUERY_PARTIAL reason=snapshot_C_failed"); await_release(carrier, candidates, argv[1]); return 6; } CloseHandle(carrier); return 6; }
    for (const auto& pair : b) {
        auto it = c.find(pair.first);
        if (it == c.end()) ++g.cMissing;
        else if (!compatible(pair.second, it->second)) ++g.cMismatch;
    }
    for (const auto& pair : c) if (b.find(pair.first) == b.end()) ++g.cMissing;
    std::vector<Candidate*> processes, jobs;
    for (auto& v : candidates) {
        auto it = c.find(reinterpret_cast<uintptr_t>(v.entry.value));
        if (it == c.end() || !compatible(v.entry, it->second)) continue;
        if (!v.first || !v.second || !sameObject(v.first, v.second)) { ++g.comparisonFailures; std::printf("candidate rejected stage=AB value=%llu\n", static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(v.entry.value))); continue; }
        if (!duplicate(carrier, v.entry.value, &v.third)) { ++g.duplicateFailures; error("duplicate_third", GetLastError()); continue; }
        ++g.duplicates;
        std::printf("duplicate source=%llu copy=%llu round=3\n", (unsigned long long)reinterpret_cast<uintptr_t>(v.entry.value), (unsigned long long)reinterpret_cast<uintptr_t>(v.third));
        if (!sameObject(v.first, v.third)) { ++g.comparisonFailures; std::puts("candidate rejected stage=C"); continue; }
        ++g.validated;
        std::printf("candidate value=%llu kind=%ls snapshot_specific=%d same_duplicate_object=1\n",
                    static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(v.entry.value)), v.entry.kind.c_str(), v.entry.specific ? 1 : 0);
        if (v.entry.kind == L"Process") {
            DWORD pid = GetProcessId(v.first), code = 0; FILETIME cr{}, ex{}, ke{}, us{};
            BOOL times = GetProcessTimes(v.first, &cr, &ex, &ke, &us);
            DWORD wait = WaitForSingleObject(v.first, 0); BOOL codeOk = GetExitCodeProcess(v.first, &code);
            if (!pid || !times || wait == WAIT_FAILED || !codeOk) ++g.queryFailures;
            bool pidMatches = v.entry.specific && pid == v.entry.processId;
            if (!pidMatches) ++g.identityMismatches;
            std::printf("process pid=%lu creation=%llu times_ok=%d wait=%lu exit_ok=%d exit=%lu snapshot_pid_match=%d\n",
                        static_cast<unsigned long>(pid), times ? filetime64(cr) : 0ULL, times, static_cast<unsigned long>(wait), codeOk, static_cast<unsigned long>(code), pidMatches ? 1 : 0);
            if (pidMatches && times) processes.push_back(&v);
        } else if (v.entry.kind == L"Thread") {
            DWORD tid = GetThreadId(v.first), pid = GetProcessIdOfThread(v.first), wait = WaitForSingleObject(v.first, 0);
            if (!tid || !pid || wait == WAIT_FAILED) ++g.queryFailures;
            if (!v.entry.specific || pid != v.entry.processId || tid != v.entry.threadId) ++g.identityMismatches;
            std::printf("thread tid=%lu pid=%lu wait=%lu snapshot_ids_match=%d\n", static_cast<unsigned long>(tid), static_cast<unsigned long>(pid), static_cast<unsigned long>(wait), v.entry.specific && pid == v.entry.processId && tid == v.entry.threadId ? 1 : 0);
        } else if (v.entry.kind == L"Job") {
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION info{};
            BOOL ok = QueryInformationJobObject(v.first, JobObjectBasicAccountingInformation, &info, sizeof(info), nullptr);
            DWORD err = ok ? 0 : GetLastError();
            if (!ok) ++g.queryFailures;
            std::printf("job query_ok=%d error=%lu active=%lu total=%lu\n", ok, static_cast<unsigned long>(err), static_cast<unsigned long>(info.ActiveProcesses), static_cast<unsigned long>(info.TotalProcesses));
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
            BOOL limitsOk = QueryInformationJobObject(v.first, JobObjectExtendedLimitInformation, &limits, sizeof(limits), nullptr);
            DWORD limitsError = limitsOk ? 0 : GetLastError();
            if (!limitsOk) ++g.queryFailures;
            DWORD flags = limits.BasicLimitInformation.LimitFlags;
            std::printf("job_limits query_ok=%d error=%lu flags=0x%08lx breakaway=%d silent_breakaway=%d kill_on_close=%d\n",
                        limitsOk, static_cast<unsigned long>(limitsError), static_cast<unsigned long>(flags),
                        (flags & JOB_OBJECT_LIMIT_BREAKAWAY_OK) != 0, (flags & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) != 0,
                        (flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE) != 0);
            if (ok) jobs.push_back(&v);
        }
    }
    for (auto* p : processes) for (auto* j : jobs) {
        BOOL inside = FALSE; BOOL ok = IsProcessInJob(p->first, j->first, &inside); DWORD err = ok ? 0 : GetLastError();
        if (!ok) ++g.queryFailures;
        std::printf("relationship process_value=%llu job_value=%llu query_ok=%d error=%lu in_job=%d\n",
                    static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(p->entry.value)),
                    static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(j->entry.value)), ok, static_cast<unsigned long>(err), inside);
    }
    if (!carrier_identity(carrier)) { if (haveDuplicate) { print_counts(); std::puts("QUERY_PARTIAL reason=carrier_changed_before_wait"); await_release(carrier, candidates, argv[1]); return 7; } CloseHandle(carrier); return 7; }
    print_counts();
    std::puts("NOTE: snapshots and comparisons do not establish stack ownership, preclude ABA, or prove last-close.");
    // Default wait; only the manager-reviewed attempt-specific release file starts revalidation.
    await_release(carrier, candidates, argv[1]);
    return 0;
}
int wmain(int argc, wchar_t** argv) {
    try { return run(argc, argv); }
    catch (...) { park_unknown("unhandled_cpp_exception"); }
}
