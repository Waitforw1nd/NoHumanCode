// Review candidate only. Do not compile or run without a separate signed window.
// Queries only case16 per-process handle metadata; no target memory read, injection or work control.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <winternl.h>
#include <cstddef>
#include <algorithm>
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
    ULONG type = 0;
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
// ProcessHandleInformation (51) and these structures are internal, not exposed
// by the fixed SDK. Layout is based on the independently documented phnt ABI.
// Fail closed on unsupported status, malformed sizes or failed self calibration.
static_assert(sizeof(void*) == 8, "this diagnostic requires x64");
struct NativeHandleEntry {
    HANDLE value; ULONG_PTR handleCount; ULONG_PTR pointerCount; ACCESS_MASK access;
    ULONG objectTypeIndex; ULONG attributes; ULONG reserved;
};
struct NativeHandleSnapshot { ULONG_PTR count; ULONG_PTR reserved; NativeHandleEntry entries[1]; };
static_assert(offsetof(NativeHandleSnapshot, entries) == 16, "unexpected native header");
static_assert(sizeof(NativeHandleEntry) == 40, "unexpected native entry layout");
using NtQueryProcessFn = NTSTATUS (NTAPI*)(HANDLE, PROCESSINFOCLASS, PVOID, ULONG, PULONG);
using NtQueryObjectFn = NTSTATUS (NTAPI*)(HANDLE, OBJECT_INFORMATION_CLASS, PVOID, ULONG, PULONG);
static NtQueryProcessFn queryProcess = nullptr;
static NtQueryObjectFn queryObject = nullptr;
static ULONG jobIndex = 0, processIndex = 0;
static HANDLE calibrationJobA = nullptr, calibrationJobB = nullptr, calibrationProcess = nullptr;
static bool resolveNative() {
    HMODULE module = GetModuleHandleW(L"ntdll.dll");
    wchar_t sys[MAX_PATH]{}, path[MAX_PATH]{};
    UINT n = GetSystemDirectoryW(sys, MAX_PATH); DWORD m = module ? GetModuleFileNameW(module, path, MAX_PATH) : 0;
    if (!module || !n || n >= MAX_PATH || !m || m >= MAX_PATH || _wcsicmp(path, (std::wstring(sys)+L"\\ntdll.dll").c_str()) != 0) { error("ntdll_system_path", GetLastError()); return false; }
    queryProcess = reinterpret_cast<NtQueryProcessFn>(GetProcAddress(module, "NtQueryInformationProcess"));
    queryObject = reinterpret_cast<NtQueryObjectFn>(GetProcAddress(module, "NtQueryObject"));
    if (!queryProcess || !queryObject) { error("ntdll_exports", GetLastError()); return false; }
    std::puts("native_api resolved_from=system_ntdll.dll internal_layout=unverified"); return true;
}
static bool objectType(HANDLE object, const wchar_t* expected, const char* phase) {
    std::vector<unsigned char> bytes(16384); ULONG returned = 0;
    NTSTATUS status = queryObject(object, ObjectTypeInformation, bytes.data(), static_cast<ULONG>(bytes.size()), &returned);
    if (status < 0 || returned > bytes.size() || returned < sizeof(PUBLIC_OBJECT_TYPE_INFORMATION)) { std::printf("type_query phase=%s status=0x%08lx returned=%lu match=0\n", phase, (unsigned long)status, (unsigned long)returned); return false; }
    const auto* info = reinterpret_cast<const PUBLIC_OBJECT_TYPE_INFORMATION*>(bytes.data());
    uintptr_t begin = reinterpret_cast<uintptr_t>(bytes.data()); uintptr_t end = begin + bytes.size();
    uintptr_t name = reinterpret_cast<uintptr_t>(info->TypeName.Buffer); size_t length = info->TypeName.Length;
    if ((length % sizeof(wchar_t)) || name < begin || name > end || length > end - name) { std::printf("type_query phase=%s malformed=1 match=0\n", phase); return false; }
    std::wstring kind(reinterpret_cast<const wchar_t*>(name), length/sizeof(wchar_t));
    bool match = kind == expected;
    std::printf("type_query phase=%s status=0x%08lx match=%d\n", phase, (unsigned long)status, match ? 1 : 0);
    return match;
}
static bool rawTable(HANDLE target, const char* label, std::map<uintptr_t, ULONG>& result) {
    ULONG size = 65536;
    for (int attempt = 0; attempt < 8 && size <= 4*1024*1024; ++attempt) {
        std::vector<unsigned char> bytes(size); ULONG returned = 0;
        NTSTATUS status = queryProcess(target, static_cast<PROCESSINFOCLASS>(51), bytes.data(), size, &returned);
        std::printf("native_table label=%s attempt=%d status=0x%08lx buffer=%lu returned=%lu\n", label, attempt, (unsigned long)status, (unsigned long)size, (unsigned long)returned);
        if (static_cast<ULONG>(status) == 0xc0000004UL || static_cast<ULONG>(status) == 0xc0000023UL) {
            if (returned > 4*1024*1024 || size > 2*1024*1024) return false;
            size = (std::max)(size * 2, returned); continue;
        }
        if (status < 0 || returned < offsetof(NativeHandleSnapshot, entries) || returned > size) return false;
        const auto* info = reinterpret_cast<const NativeHandleSnapshot*>(bytes.data());
        size_t capacity = (returned - offsetof(NativeHandleSnapshot, entries))/sizeof(NativeHandleEntry);
        if (info->count > capacity || info->count > kMaxEntries) return false;
        for (size_t i=0;i<info->count;i++) {
            uintptr_t value = reinterpret_cast<uintptr_t>(info->entries[i].value);
            if (!result.emplace(value, info->entries[i].objectTypeIndex).second) return false;
        }
        std::printf("native_table_complete label=%s entries=%llu\n", label, (unsigned long long)result.size());
        return true;
    }
    return false;
}
static bool calibrate() {
    calibrationJobA = CreateJobObjectW(nullptr, nullptr); calibrationJobB = CreateJobObjectW(nullptr, nullptr);
    if (!calibrationJobA || !calibrationJobB || !DuplicateHandle(GetCurrentProcess(), GetCurrentProcess(), GetCurrentProcess(), &calibrationProcess, 0, FALSE, DUPLICATE_SAME_ACCESS)) { error("calibration_create", GetLastError()); return false; }
    // The two diagnostic-owned Jobs are empty and never assigned any process.
    if (!objectType(calibrationJobA, L"Job", "calibration_job_A") || !objectType(calibrationJobB, L"Job", "calibration_job_B") || !objectType(calibrationProcess, L"Process", "calibration_process")) return false;
    std::map<uintptr_t, ULONG> table;
    if (!rawTable(GetCurrentProcess(), "self_calibration", table)) return false;
    auto a=table.find(reinterpret_cast<uintptr_t>(calibrationJobA)), b=table.find(reinterpret_cast<uintptr_t>(calibrationJobB)), p=table.find(reinterpret_cast<uintptr_t>(calibrationProcess));
    if (a==table.end() || b==table.end() || p==table.end() || !a->second || a->second!=b->second || a->second==p->second || !p->second) return false;
    jobIndex=a->second; processIndex=p->second;
    std::printf("calibration job_index=%lu process_index=%lu two_empty_jobs_same=1 distinct_process=1\n", (unsigned long)jobIndex, (unsigned long)processIndex);
    return true;
}
static bool snapshot(HANDLE carrier, const char* label, Table& out) {
    if (!carrier_identity(carrier)) return false;
    std::map<uintptr_t, ULONG> raw;
    if (!rawTable(carrier,label,raw)) { ++g.snapshotErrors; return false; }
    size_t process=0, job=0, other=0;
    for (const auto& pair: raw) {
        ++g.walked;
        if (pair.second!=jobIndex && pair.second!=processIndex) { ++other; ++g.other; continue; }
        Entry e; e.value=reinterpret_cast<HANDLE>(pair.first);e.type=pair.second;e.kind=pair.second==jobIndex?L"Job":L"Process";
        if(e.kind==L"Job"){++job;++g.job;}else{++process;++g.process;}
        std::printf("table_candidate label=%s value=%llu index=%lu kind=%ls\n",label,(unsigned long long)pair.first,(unsigned long)pair.second,e.kind.c_str());
        if (!out.emplace(pair.first,e).second || out.size()>kMaxCandidates) { ++g.snapshotErrors; return false; }
    }
    std::printf("table_types label=%s process=%llu job=%llu other=%llu\n",label,(unsigned long long)process,(unsigned long long)job,(unsigned long long)other);
    return carrier_identity(carrier);
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
    if (!resolveCompareObjectHandles() || !resolveNative() || !calibrate()) { std::puts("INCONCLUSIVE: native API or local calibration unavailable"); return 8; } // Calibration objects are local and empty.
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
        if (!objectType(v.first, v.entry.kind.c_str(), "target_candidate")) { ++g.queryFailures; std::puts("candidate rejected type_query"); continue; }
        ++g.validated;
        std::printf("candidate value=%llu kind=%ls table_index=%lu same_duplicate_object=1\n",
                    static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(v.entry.value)), v.entry.kind.c_str(), static_cast<unsigned long>(v.entry.type));
        if (v.entry.kind == L"Process") {
            DWORD pid = GetProcessId(v.first), code = 0; FILETIME cr{}, ex{}, ke{}, us{};
            BOOL times = GetProcessTimes(v.first, &cr, &ex, &ke, &us);
            DWORD wait = WaitForSingleObject(v.first, 0); BOOL codeOk = GetExitCodeProcess(v.first, &code);
            if (!pid || !times || wait == WAIT_FAILED || !codeOk) ++g.queryFailures;
            bool pidMatches = pid != 0; // Actual duplicate queried; native table does not provide PID.
            if (!pidMatches) ++g.identityMismatches;
            std::printf("process pid=%lu creation=%llu times_ok=%d wait=%lu exit_ok=%d exit=%lu queried_pid_valid=%d\n",
                        static_cast<unsigned long>(pid), times ? filetime64(cr) : 0ULL, times, static_cast<unsigned long>(wait), codeOk, static_cast<unsigned long>(code), pidMatches ? 1 : 0);
            if (pidMatches && times) processes.push_back(&v);
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
    std::puts("NOTE: native class/layout and cross-process type index are not a public stable contract; duplicate type check and relationships are observations, not stack ownership or last-close proof.");
    // Default wait; only the manager-reviewed attempt-specific release file starts revalidation.
    await_release(carrier, candidates, argv[1]);
    return 0;
}
int wmain(int argc, wchar_t** argv) {
    try { return run(argc, argv); }
    catch (...) { park_unknown("unhandled_cpp_exception"); }
}
