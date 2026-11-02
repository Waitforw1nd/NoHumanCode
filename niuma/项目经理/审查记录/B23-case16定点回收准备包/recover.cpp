// REVIEW ONLY: staged, targeted termination candidate. Do not compile/run without signed approval.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <processsnapshot.h>
#include <winternl.h>
#include <jobapi2.h>
#include <cstdint>
#include <cstdio>
#include <cwchar>
#include <cwctype>
#include <fstream>
#include <initializer_list>
#include <iterator>
#include <regex>
#include <string>
#include <vector>

struct Identity { DWORD pid; unsigned long long created; const wchar_t* label; HANDLE handle; };
static Identity caseProcess{89780,134381029069968888ULL,L"case",nullptr};
static Identity runner{62476,134381028898031298ULL,L"runner",nullptr};
static Identity supervisor{87376,134381028875429933ULL,L"supervisor",nullptr};
static Identity launcher{90776,134381028866695431ULL,L"launcher",nullptr};
static constexpr DWORD helperPid=91236;
static constexpr unsigned long long helperCreated=134381029070411070ULL;
static constexpr DWORD kTerminationCode=0xB23;
using CompareFn=BOOL(WINAPI*)(HANDLE,HANDLE);
using QueryTypeFn=NTSTATUS(NTAPI*)(HANDLE,OBJECT_INFORMATION_CLASS,PVOID,ULONG,PULONG);
static CompareFn sameObject=nullptr;
static QueryTypeFn queryType=nullptr;
static std::vector<HANDLE> retained; // Raw handles intentionally remain open on any error.
static HANDLE job=nullptr, helper=nullptr;
static const wchar_t* expectedProbe=nullptr;

[[noreturn]] static void hold(const char* why) {
    std::printf("RECOVERY_UNKNOWN reason=%s retained=%llu\n",why,(unsigned long long)retained.size());
    std::fflush(stdout); for(;;) Sleep(1000);
}
static unsigned long long time64(FILETIME f){return ((unsigned long long)f.dwHighDateTime<<32)|f.dwLowDateTime;}
static bool systemModule(const wchar_t* name, HMODULE& out) {
    out=GetModuleHandleW(name);wchar_t sys[MAX_PATH]{}, path[MAX_PATH]{};
    UINT a=GetSystemDirectoryW(sys,MAX_PATH);DWORD b=out?GetModuleFileNameW(out,path,MAX_PATH):0;
    return out && a && a<MAX_PATH && b && b<MAX_PATH && _wcsicmp(path,(std::wstring(sys)+L"\\"+name).c_str())==0;
}
static bool typeIs(HANDLE h,const wchar_t* expected) {
    std::vector<unsigned char> buffer(16384);ULONG length=0;
    NTSTATUS status=queryType(h,ObjectTypeInformation,buffer.data(),(ULONG)buffer.size(),&length);
    if(status<0 || length>buffer.size() || length<sizeof(PUBLIC_OBJECT_TYPE_INFORMATION))return false;
    auto info=reinterpret_cast<const PUBLIC_OBJECT_TYPE_INFORMATION*>(buffer.data());
    uintptr_t begin=(uintptr_t)buffer.data(),end=begin+buffer.size(),p=(uintptr_t)info->TypeName.Buffer;size_t n=info->TypeName.Length;
    return !(n%sizeof(wchar_t)) && p>=begin && p<=end && n<=end-p && std::wstring((const wchar_t*)p,n/sizeof(wchar_t))==expected;
}
static bool identity(Identity& x,const wchar_t* expectedPath,bool mustLive) {
    FILETIME c{},e{},k{},u{};wchar_t path[32768]{};DWORD n=(DWORD)(sizeof(path)/sizeof(path[0]));
    if(!x.handle || GetProcessId(x.handle)!=x.pid || !GetProcessTimes(x.handle,&c,&e,&k,&u) || time64(c)!=x.created ||
       !QueryFullProcessImageNameW(x.handle,0,path,&n) || _wcsicmp(path,expectedPath)!=0)return false;
    DWORD wait=WaitForSingleObject(x.handle,0);
    std::printf("identity role=%ls pid=%lu creation=%llu wait=%lu path_match=1\n",x.label,(unsigned long)x.pid,x.created,(unsigned long)wait);
    return !mustLive || wait==WAIT_TIMEOUT;
}
static HANDLE duplicateFrom(HANDLE parent,HANDLE source) {
    HANDLE result=nullptr;
    if(!DuplicateHandle(parent,source,GetCurrentProcess(),&result,0,FALSE,DUPLICATE_SAME_ACCESS))return nullptr;
    retained.push_back(result);return result;
}
static bool bindOriginal(HANDLE parent,Identity& child,const char* label) {
    HPSS snap=nullptr;HPSSWALK marker=nullptr;
    const auto flags=(PSS_CAPTURE_FLAGS)(PSS_CAPTURE_HANDLES|PSS_CAPTURE_HANDLE_TYPE_SPECIFIC_INFORMATION);
    DWORD rc=PssCaptureSnapshot(parent,flags,0,&snap);if(rc!=ERROR_SUCCESS){std::printf("bind stage=%s capture_error=%lu\n",label,(unsigned long)rc);return false;}
    rc=PssWalkMarkerCreate(nullptr,&marker);if(rc!=ERROR_SUCCESS){PssFreeSnapshot(GetCurrentProcess(),snap);return false;}
    size_t matches=0,seen=0,errors=0;
    for(;;){PSS_HANDLE_ENTRY entry{};rc=PssWalkSnapshot(snap,PSS_WALK_HANDLES,marker,&entry,sizeof(entry));
        if(rc==ERROR_NO_MORE_ITEMS)break;
        if(rc!=ERROR_SUCCESS || ++seen>4096){++errors;break;}
        if(!(entry.Flags&PSS_HANDLE_HAVE_TYPE) || !(entry.Flags&PSS_HANDLE_HAVE_TYPE_SPECIFIC_INFORMATION) ||
           entry.ObjectType!=PSS_OBJECT_TYPE_PROCESS || entry.TypeSpecificInformation.Process.ProcessId!=child.pid)continue;
        HANDLE copy=duplicateFrom(parent,entry.Handle);if(!copy){++errors;continue;}
        if(sameObject(copy,child.handle))++matches;else ++errors;
    }
    DWORD a=PssWalkMarkerFree(marker),b=PssFreeSnapshot(GetCurrentProcess(),snap);
    std::printf("bind stage=%s seen=%llu matching_original=%llu errors=%llu walk=%lu marker_free=%lu snapshot_free=%lu\n",label,
        (unsigned long long)seen,(unsigned long long)matches,(unsigned long long)errors,(unsigned long)rc,(unsigned long)a,(unsigned long)b);
    return matches>0 && errors==0 && rc==ERROR_NO_MORE_ITEMS && a==ERROR_SUCCESS && b==ERROR_SUCCESS;
}
static bool sameCopies(HANDLE owner,HANDLE source,HANDLE& result,const wchar_t* kind) {
    HANDLE a=duplicateFrom(owner,source),b=duplicateFrom(owner,source);
    if(!a || !b || !sameObject(a,b) || !typeIs(a,kind) || !typeIs(b,kind))return false;
    result=a;return true;
}
static bool jobAndHelper() {
    if(!typeIs(job,L"Job") || !typeIs(helper,L"Process") || GetProcessId(helper)!=helperPid)return false;
    FILETIME c{},e{},k{},u{};DWORD code=0;
    if(!GetProcessTimes(helper,&c,&e,&k,&u) || time64(c)!=helperCreated || WaitForSingleObject(helper,0)!=WAIT_OBJECT_0 || !GetExitCodeProcess(helper,&code) || code!=0)return false;
    wchar_t image[32768]{};DWORD imageLength=(DWORD)(sizeof(image)/sizeof(image[0]));
    if(!QueryFullProcessImageNameW(helper,0,image,&imageLength) || !expectedProbe || _wcsicmp(image,expectedProbe)!=0)return false;
    BOOL member=FALSE;if(!IsProcessInJob(helper,job,&member) || !member)return false;
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION info{};JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
    if(!QueryInformationJobObject(job,JobObjectBasicAccountingInformation,&info,sizeof(info),nullptr) ||
       !QueryInformationJobObject(job,JobObjectExtendedLimitInformation,&limits,sizeof(limits),nullptr))return false;
    DWORD flags=limits.BasicLimitInformation.LimitFlags;
    std::printf("original_candidates job_active=%lu total=%lu flags=0x%08lx helper=%lu creation=%llu exit=%lu member=%d\n",
        (unsigned long)info.ActiveProcesses,(unsigned long)info.TotalProcesses,(unsigned long)flags,(unsigned long)helperPid,helperCreated,(unsigned long)code,member);
    return info.ActiveProcesses==0 && info.TotalProcesses==2 && flags==0x2800;
}
static bool sourceStillOriginal() {
    HANDLE currentJob=duplicateFrom(caseProcess.handle,(HANDLE)(uintptr_t)284);
    HANDLE currentHelper=duplicateFrom(caseProcess.handle,(HANDLE)(uintptr_t)296);
    return currentJob && currentHelper && sameObject(job,currentJob) && sameObject(helper,currentHelper) &&
           typeIs(currentJob,L"Job") && typeIs(currentHelper,L"Process");
}
static bool allowReceipt(const std::wstring& path) {
    std::ifstream in(path.c_str(),std::ios::binary);if(!in)return false;
    std::string data((std::istreambuf_iterator<char>(in)),std::istreambuf_iterator<char>());
    return data=="created=1 query=1 member=0 wait=0\n";
}
static void waitRequest(const std::wstring& root,const wchar_t* phase) {
    std::wstring path=root+L"\\"+phase+L".request";
    std::printf("WAIT_REQUEST phase=%ls\n",phase);std::fflush(stdout);
    for(;;){DWORD a=GetFileAttributesW(path.c_str());if(a!=INVALID_FILE_ATTRIBUTES){if(a&FILE_ATTRIBUTE_DIRECTORY)hold("request_is_directory");return;}
        DWORD e=GetLastError();if(e!=ERROR_FILE_NOT_FOUND && e!=ERROR_PATH_NOT_FOUND)hold("request_query_failed");Sleep(250);}
}
static bool lateReceipt(const std::wstring& path,DWORD pid,unsigned long long created,DWORD code) {
    std::ifstream in(path.c_str(),std::ios::binary);if(!in)return false;
    std::string s((std::istreambuf_iterator<char>(in)),std::istreambuf_iterator<char>());if(s.size()>65536)return false;
    return std::regex_search(s,std::regex("\\\"pid\\\"\\s*:\\s*"+std::to_string(pid)+"([^0-9]|$)")) &&
           std::regex_search(s,std::regex("\\\"creation_filetime\\\"\\s*:\\s*"+std::to_string(created)+"([^0-9]|$)")) &&
           std::regex_search(s,std::regex("\\\"late_exit_query_ok\\\"\\s*:\\s*true")) &&
           std::regex_search(s,std::regex("\\\"late_exit_code\\\"\\s*:\\s*"+std::to_string(code)+"([^0-9]|$)")) &&
           std::regex_search(s,std::regex("\\\"late_signal_utc\\\"\\s*:\\s*\\\"[0-9TZ:.-]+\\\""));
}
static void awaitLate(const std::wstring& path,DWORD pid,unsigned long long created,DWORD code,const char* phase) {
    for(int i=0;i<120;i++){if(lateReceipt(path,pid,created,code)){std::printf("LATE_RECEIPT phase=%s validated=1\n",phase);return;}Sleep(250);}
    hold("late_receipt_missing_or_mismatch");
}
static DWORD terminateAndWait(Identity& x) {
    if(!TerminateProcess(x.handle,kTerminationCode))hold("terminate_failed");
    if(WaitForSingleObject(x.handle,10000)!=WAIT_OBJECT_0)hold("original_handle_signal_unconfirmed");
    DWORD code=0;if(!GetExitCodeProcess(x.handle,&code) || code!=kTerminationCode)hold("exit_code_unconfirmed");
    std::printf("TERMINATED role=%ls pid=%lu creation=%llu original_signal=1 exit=%lu\n",x.label,(unsigned long)x.pid,x.created,(unsigned long)code);return code;
}
static int run(int argc,wchar_t** argv) {
    setvbuf(stdout,nullptr,_IONBF,0);
    if(argc!=6 || wcslen(argv[1])<4 || towupper(argv[1][0])!=L'E' || argv[1][1]!=L':' || argv[1][2]!=L'\\')return 2;
    // argv: new E attempt directory, case exe path, PowerShell exe path, probe exe path, original matrix attempt directory.
    const std::wstring base=argv[1],matrix=argv[5];expectedProbe=argv[4];
    HMODULE kb=nullptr,nt=nullptr;if(!systemModule(L"KernelBase.dll",kb)||!systemModule(L"ntdll.dll",nt))return 3;
    sameObject=(CompareFn)GetProcAddress(kb,"CompareObjectHandles");queryType=(QueryTypeFn)GetProcAddress(nt,"NtQueryObject");if(!sameObject||!queryType)return 4;
    retained.reserve(16384);
    const DWORD rights=PROCESS_DUP_HANDLE|PROCESS_QUERY_INFORMATION|PROCESS_TERMINATE|SYNCHRONIZE;
    caseProcess.handle=OpenProcess(rights,FALSE,caseProcess.pid);runner.handle=OpenProcess(rights,FALSE,runner.pid);supervisor.handle=OpenProcess(rights,FALSE,supervisor.pid);launcher.handle=OpenProcess(rights,FALSE,launcher.pid);
    if(!caseProcess.handle||!runner.handle||!supervisor.handle||!launcher.handle)hold("open_identity_failed");
    if(!identity(caseProcess,argv[2],true)||!identity(runner,argv[3],true)||!identity(supervisor,argv[3],true)||!identity(launcher,argv[3],true))hold("identity_or_path_mismatch");
    if(!allowReceipt(matrix+L"\\matrix\\.tmpio5Sbs\\allow"))hold("allow_receipt_mismatch");
    if(!bindOriginal(runner.handle,caseProcess,"runner_to_case")||!bindOriginal(supervisor.handle,runner,"supervisor_to_runner")||!bindOriginal(launcher.handle,supervisor,"launcher_to_supervisor"))hold("original_parent_handle_unconfirmed");
    if(!sameCopies(caseProcess.handle,(HANDLE)(uintptr_t)284,job,L"Job")||!sameCopies(caseProcess.handle,(HANDLE)(uintptr_t)296,helper,L"Process"))hold("original_job_helper_objects_unconfirmed");
    if(!sourceStillOriginal()||!jobAndHelper())hold("initial_job_helper_proof_failed");Sleep(250);if(!sourceStillOriginal()||!jobAndHelper())hold("repeated_job_helper_proof_failed");
    // No names or PID-derived process re-open are used for the helper.
    std::puts("PREFLIGHT_READY: object candidates retained; each stage requires a new request file");
    waitRequest(base,L"01-case");
    if(!identity(caseProcess,argv[2],true)||!identity(runner,argv[3],true)||!sourceStillOriginal()||!jobAndHelper())hold("case_preconditions_changed");
    terminateAndWait(caseProcess);if(!jobAndHelper())hold("post_case_job_unconfirmed");
    std::puts("CASE_SIGNAL_JOB_ZERO_CONFIRMED");
    waitRequest(base,L"02-runner");if(!identity(runner,argv[3],true)||!identity(supervisor,argv[3],true)||!jobAndHelper())hold("runner_preconditions_changed");
    DWORD runnerCode=terminateAndWait(runner);
    awaitLate(matrix+L"\\incident-late-signal.json",runner.pid,runner.created,runnerCode,"supervisor_runner");
    waitRequest(base,L"03-supervisor");if(!identity(supervisor,argv[3],true)||!identity(launcher,argv[3],true)||!jobAndHelper())hold("supervisor_preconditions_changed");
    DWORD supervisorCode=terminateAndWait(supervisor);
    awaitLate(matrix+L"\\launcher-late-signal.json",supervisor.pid,supervisor.created,supervisorCode,"launcher_supervisor");
    std::puts("LAUNCHER_LATE_SIGNAL_CONFIRMED: no stdin/Ctrl-C control channel; launcher termination requires separate request");
    waitRequest(base,L"04-launcher");if(!identity(launcher,argv[3],true)||!jobAndHelper())hold("launcher_preconditions_changed");
    terminateAndWait(launcher);
    std::puts("LAUNCHER_ORIGINAL_SIGNAL_CONFIRMED: gate files remain; duplicated Job and all other objects remain held");
    waitRequest(base,L"05-release-diagnostic-objects");
    if(!jobAndHelper())hold("release_job_helper_unconfirmed");
    for(auto* x : {&caseProcess,&runner,&supervisor,&launcher}){DWORD code=0;if(WaitForSingleObject(x->handle,0)!=WAIT_OBJECT_0 || !GetExitCodeProcess(x->handle,&code) || code!=kTerminationCode)hold("release_process_signal_unconfirmed");}
    // Releasing our Job duplicates can be the last close; requires its own manager decision.
    for(HANDLE h:retained){if(!CloseHandle(h))hold("release_duplicate_failed");}
    for(auto* x : {&caseProcess,&runner,&supervisor,&launcher}){if(!CloseHandle(x->handle))hold("release_process_handle_failed");}
    std::puts("RECOVERY_LOCAL_HANDLES_RELEASED: gate files remain; original matrix case outcome remains unknown");return 0;
}
int wmain(int argc,wchar_t**argv){try{return run(argc,argv);}catch(...){hold("cpp_exception");}}
