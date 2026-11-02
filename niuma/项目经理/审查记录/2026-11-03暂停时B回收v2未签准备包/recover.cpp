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
struct Retained { HANDLE owner; HANDLE source; HANDLE copy; };
static std::vector<Retained> retained; // Provenance retained for explicit preflight abort.
static HANDLE job=nullptr, helper=nullptr;
static const wchar_t* expectedProbe=nullptr;
static const wchar_t* expectedCase=nullptr;
static const wchar_t* expectedPowerShell=nullptr;
static std::wstring attemptDirectory;
static bool preflightPhase=true;
[[noreturn]] static void preflightAbort();

[[noreturn]] static void hold(const char* why) {
    std::printf("RECOVERY_UNKNOWN reason=%s retained=%llu\n",why,(unsigned long long)retained.size());
    std::fflush(stdout);
    if(preflightPhase && !attemptDirectory.empty()) {
        std::puts("PREFLIGHT_ABORT_AVAILABLE: requires explicit 00-abort.request and successful source revalidation");
        const std::wstring request=attemptDirectory+L"\\00-abort.request";
        for(;;){DWORD a=GetFileAttributesW(request.c_str());if(a!=INVALID_FILE_ATTRIBUTES){DWORD stage=GetFileAttributesW((attemptDirectory+L"\\01-case.request").c_str());DWORD stageError=stage==INVALID_FILE_ATTRIBUTES?GetLastError():0;if((a&FILE_ATTRIBUTE_DIRECTORY)||stage!=INVALID_FILE_ATTRIBUTES||(stageError!=ERROR_FILE_NOT_FOUND && stageError!=ERROR_PATH_NOT_FOUND))break;preflightAbort();}Sleep(1000);}
    }
    for(;;) Sleep(1000);
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
    if(status<0 || length>buffer.size() || length<sizeof(PUBLIC_OBJECT_TYPE_INFORMATION)){
        std::printf("type_query handle=%llu expected=%ls ntstatus=0x%08lx bytes=%lu valid=0\n",(unsigned long long)(uintptr_t)h,expected,(unsigned long)status,(unsigned long)length);return false;}
    auto info=reinterpret_cast<const PUBLIC_OBJECT_TYPE_INFORMATION*>(buffer.data());
    uintptr_t begin=(uintptr_t)buffer.data(),end=begin+buffer.size(),p=(uintptr_t)info->TypeName.Buffer;size_t n=info->TypeName.Length;
    bool valid=!(n%sizeof(wchar_t)) && p>=begin && p<=end && n<=end-p;
    bool match=valid && std::wstring((const wchar_t*)p,n/sizeof(wchar_t))==expected;
    std::printf("type_query handle=%llu expected=%ls ntstatus=0x%08lx bytes=%lu valid=%d match=%d\n",(unsigned long long)(uintptr_t)h,expected,(unsigned long)status,(unsigned long)length,valid?1:0,match?1:0);
    return match;
}
static bool identity(Identity& x,const wchar_t* expectedPath,bool mustLive) {
    FILETIME c{},e{},k{},u{};wchar_t path[32768]{};DWORD n=(DWORD)(sizeof(path)/sizeof(path[0]));
    if(!x.handle){std::printf("identity role=%ls handle_missing=1\n",x.label);return false;}
    DWORD actualPid=GetProcessId(x.handle),pidError=actualPid?0:GetLastError();
    BOOL times=GetProcessTimes(x.handle,&c,&e,&k,&u);DWORD timesError=times?0:GetLastError();
    BOOL pathOk=QueryFullProcessImageNameW(x.handle,0,path,&n);DWORD pathError=pathOk?0:GetLastError();
    bool pathMatches=pathOk && _wcsicmp(path,expectedPath)==0;
    DWORD wait=WaitForSingleObject(x.handle,0);
    DWORD waitError=wait==WAIT_FAILED?GetLastError():0;
    std::printf("identity role=%ls pid=%lu pid_error=%lu times_ok=%d times_error=%lu creation=%llu path_ok=%d path_error=%lu path_match=%d wait=%lu wait_error=%lu\n",
        x.label,(unsigned long)actualPid,(unsigned long)pidError,times,(unsigned long)timesError,times?time64(c):0ULL,pathOk,(unsigned long)pathError,pathMatches?1:0,(unsigned long)wait,(unsigned long)waitError);
    return actualPid==x.pid && times && time64(c)==x.created && pathMatches && (!mustLive || wait==WAIT_TIMEOUT);
}
static HANDLE duplicateFrom(HANDLE parent,HANDLE source) {
    HANDLE result=nullptr;
    if(!DuplicateHandle(parent,source,GetCurrentProcess(),&result,0,FALSE,DUPLICATE_SAME_ACCESS))return nullptr;
    retained.push_back({parent,source,result});return result;
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
    HANDLE a=duplicateFrom(owner,source);DWORD firstError=a?0:GetLastError();
    HANDLE b=duplicateFrom(owner,source);DWORD secondError=b?0:GetLastError();
    bool same=a && b && sameObject(a,b);DWORD sameError=(a && b && !same)?GetLastError():0;
    bool firstType=a && typeIs(a,kind),secondType=b && typeIs(b,kind);
    std::printf("same_copies source=%llu first_ok=%d first_error=%lu second_ok=%d second_error=%lu same=%d compare_error=%lu first_type=%d second_type=%d\n",
        (unsigned long long)(uintptr_t)source,a?1:0,(unsigned long)firstError,b?1:0,(unsigned long)secondError,same?1:0,(unsigned long)sameError,firstType?1:0,secondType?1:0);
    if(!a || !b || !same || !firstType || !secondType)return false;
    result=a;return true;
}
static bool jobAndHelper() {
    bool jobType=typeIs(job,L"Job"), processType=typeIs(helper,L"Process");
    DWORD pid=GetProcessId(helper),pidError=pid?0:GetLastError();
    FILETIME c{},e{},k{},u{};DWORD code=0;
    BOOL times=GetProcessTimes(helper,&c,&e,&k,&u);DWORD timesError=times?0:GetLastError();
    DWORD wait=WaitForSingleObject(helper,0),waitError=wait==WAIT_FAILED?GetLastError():0;
    BOOL exitOk=GetExitCodeProcess(helper,&code);DWORD exitError=exitOk?0:GetLastError();
    wchar_t image[32768]{};DWORD imageLength=(DWORD)(sizeof(image)/sizeof(image[0]));
    BOOL imageOk=QueryFullProcessImageNameW(helper,0,image,&imageLength);DWORD imageError=imageOk?0:GetLastError();
    bool imageMatches=imageOk && expectedProbe && _wcsicmp(image,expectedProbe)==0;
    BOOL member=FALSE;BOOL memberOk=IsProcessInJob(helper,job,&member);DWORD memberError=memberOk?0:GetLastError();
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION info{};JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
    BOOL basicOk=QueryInformationJobObject(job,JobObjectBasicAccountingInformation,&info,sizeof(info),nullptr);DWORD basicError=basicOk?0:GetLastError();
    BOOL limitsOk=QueryInformationJobObject(job,JobObjectExtendedLimitInformation,&limits,sizeof(limits),nullptr);DWORD limitsError=limitsOk?0:GetLastError();
    DWORD flags=limits.BasicLimitInformation.LimitFlags;
    std::printf("helper pid=%lu pid_error=%lu times_ok=%d times_error=%lu creation=%llu wait=%lu wait_error=%lu exit_ok=%d exit_error=%lu exit=%lu image_ok=%d image_error=%lu image_matches=%d\n",
        (unsigned long)pid,(unsigned long)pidError,times,(unsigned long)timesError,times?time64(c):0ULL,(unsigned long)wait,(unsigned long)waitError,exitOk,(unsigned long)exitError,(unsigned long)code,imageOk,(unsigned long)imageError,imageMatches?1:0);
    std::printf("job basic_ok=%d basic_error=%lu active=%lu total=%lu limits_ok=%d limits_error=%lu flags=0x%08lx member_ok=%d member_error=%lu member=%d image_unavailable=%d\n",
        basicOk,(unsigned long)basicError,(unsigned long)info.ActiveProcesses,(unsigned long)info.TotalProcesses,limitsOk,(unsigned long)limitsError,(unsigned long)flags,memberOk,(unsigned long)memberError,member,imageOk?0:1);
    // A failed image query is explicitly missing evidence; creation identity,
    // same source object, fixed probe artifact/receipt and job relation remain required.
    return jobType && processType && pid==helperPid && times && time64(c)==helperCreated && wait==WAIT_OBJECT_0 && exitOk && code==0 &&
           (!imageOk || imageMatches) && memberOk && member && basicOk && info.ActiveProcesses==0 && info.TotalProcesses==2 && limitsOk && flags==0x2800;
}
static bool sourceStillOriginal() {
    HANDLE currentJob=duplicateFrom(caseProcess.handle,(HANDLE)(uintptr_t)284);
    DWORD jobError=currentJob?0:GetLastError();
    HANDLE currentHelper=duplicateFrom(caseProcess.handle,(HANDLE)(uintptr_t)296);
    DWORD helperError=currentHelper?0:GetLastError();
    bool jobSame=currentJob && sameObject(job,currentJob),helperSame=currentHelper && sameObject(helper,currentHelper);
    bool jobType=currentJob && typeIs(currentJob,L"Job"),helperType=currentHelper && typeIs(currentHelper,L"Process");
    std::printf("source_recheck job_copy=%d job_error=%lu job_same=%d job_type=%d helper_copy=%d helper_error=%lu helper_same=%d helper_type=%d\n",
        currentJob?1:0,(unsigned long)jobError,jobSame?1:0,jobType?1:0,currentHelper?1:0,(unsigned long)helperError,helperSame?1:0,helperType?1:0);
    return currentJob && currentHelper && jobSame && helperSame && jobType && helperType;
}
[[noreturn]] static void preflightAbort() {
    preflightPhase=false; // A failed abort must not recursively retry on a stale request file.
    if(!job || !caseProcess.handle || !runner.handle || !supervisor.handle || !launcher.handle ||
       !expectedCase || !expectedPowerShell || !identity(caseProcess,expectedCase,true) ||
       !identity(runner,expectedPowerShell,true) || !identity(supervisor,expectedPowerShell,true) ||
       !identity(launcher,expectedPowerShell,true))hold("abort_carrier_or_owner_unconfirmed");
    std::vector<HANDLE> checks; checks.reserve(retained.size()+2);
    HANDLE liveJob=nullptr;
    if(!DuplicateHandle(caseProcess.handle,(HANDLE)(uintptr_t)284,GetCurrentProcess(),&liveJob,0,FALSE,DUPLICATE_SAME_ACCESS))hold("abort_job_duplicate_failed");
    checks.push_back(liveJob);
    if(!sameObject(liveJob,job)||!typeIs(liveJob,L"Job"))hold("abort_job_source_changed");
    for(const auto& r:retained){
        HANDLE verify=nullptr;
        if(!DuplicateHandle(r.owner,r.source,GetCurrentProcess(),&verify,0,FALSE,DUPLICATE_SAME_ACCESS))hold("abort_source_duplicate_failed");
        checks.push_back(verify);
        bool match=sameObject(r.copy,verify)!=FALSE;
        std::printf("abort_source owner=%llu source=%llu copy=%llu verify=%llu match=%d\n",(unsigned long long)(uintptr_t)r.owner,(unsigned long long)(uintptr_t)r.source,(unsigned long long)(uintptr_t)r.copy,(unsigned long long)(uintptr_t)verify,match?1:0);
        if(!match)hold("abort_source_object_changed");
    }
    if(!identity(caseProcess,expectedCase,true)||!identity(runner,expectedPowerShell,true)||
       !identity(supervisor,expectedPowerShell,true)||!identity(launcher,expectedPowerShell,true))hold("abort_owner_changed_after_verification");
    HANDLE confirmJob=nullptr;
    if(!DuplicateHandle(caseProcess.handle,(HANDLE)(uintptr_t)284,GetCurrentProcess(),&confirmJob,0,FALSE,DUPLICATE_SAME_ACCESS))hold("abort_job_final_duplicate_failed");
    checks.push_back(confirmJob);
    if(!sameObject(confirmJob,job))hold("abort_job_final_mismatch");
    std::puts("PREFLIGHT_ABORT_VERIFIED: closing only this tool's copies; not a last-close proof");
    auto closeLocal=[](HANDLE h){BOOL ok=CloseHandle(h);DWORD last=ok?0:GetLastError();std::printf("abort_close copy=%llu ok=%d error=%lu\n",(unsigned long long)(uintptr_t)h,ok,(unsigned long)last);if(!ok)hold("abort_close_failed");};
    for(const auto& r:retained)closeLocal(r.copy);
    for(HANDLE h:checks)closeLocal(h);
    for(auto* x:{&caseProcess,&runner,&supervisor,&launcher})closeLocal(x->handle);
    std::puts("PREFLIGHT_ABORT_COMPLETE");std::fflush(stdout);ExitProcess(0);
}
static bool allowReceipt(const std::wstring& path) {
    std::ifstream in(path.c_str(),std::ios::binary);if(!in)return false;
    std::string data((std::istreambuf_iterator<char>(in)),std::istreambuf_iterator<char>());
    return data=="created=1 query=1 member=0 wait=0\n";
}
static void waitRequest(const std::wstring& root,const wchar_t* phase) {
    std::wstring path=root+L"\\"+phase+L".request";
    std::printf("WAIT_REQUEST phase=%ls\n",phase);std::fflush(stdout);
    for(;;){DWORD a=GetFileAttributesW(path.c_str());DWORD requestError=(a==INVALID_FILE_ATTRIBUTES)?GetLastError():0;
        if(preflightPhase && wcscmp(phase,L"01-case")==0){
            DWORD abortAttributes=GetFileAttributesW((root+L"\\00-abort.request").c_str());
            if(abortAttributes!=INVALID_FILE_ATTRIBUTES){
                if((abortAttributes&FILE_ATTRIBUTE_DIRECTORY)||a!=INVALID_FILE_ATTRIBUTES){preflightPhase=false;hold("abort_and_case_requests_conflict");}
                preflightAbort();
            }
        }
        if(a!=INVALID_FILE_ATTRIBUTES){if(a&FILE_ATTRIBUTE_DIRECTORY)hold("request_is_directory");return;}
        if(requestError!=ERROR_FILE_NOT_FOUND && requestError!=ERROR_PATH_NOT_FOUND)hold("request_query_failed");Sleep(250);}
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
    const std::wstring base=argv[1],matrix=argv[5];attemptDirectory=base;expectedCase=argv[2];expectedPowerShell=argv[3];expectedProbe=argv[4];
    HMODULE kb=nullptr,nt=nullptr;if(!systemModule(L"KernelBase.dll",kb)||!systemModule(L"ntdll.dll",nt))return 3;
    sameObject=(CompareFn)GetProcAddress(kb,"CompareObjectHandles");queryType=(QueryTypeFn)GetProcAddress(nt,"NtQueryObject");if(!sameObject||!queryType)return 4;
    retained.reserve(16384);
    const DWORD rights=PROCESS_DUP_HANDLE|PROCESS_QUERY_INFORMATION|PROCESS_TERMINATE|SYNCHRONIZE;
    for(auto* x:{&caseProcess,&runner,&supervisor,&launcher}){
        x->handle=OpenProcess(rights,FALSE,x->pid);DWORD last=x->handle?0:GetLastError();
        std::printf("open role=%ls ok=%d error=%lu\n",x->label,x->handle?1:0,(unsigned long)last);
    }
    if(!caseProcess.handle||!runner.handle||!supervisor.handle||!launcher.handle)hold("open_identity_failed");
    if(!identity(caseProcess,argv[2],true)||!identity(runner,argv[3],true)||!identity(supervisor,argv[3],true)||!identity(launcher,argv[3],true))hold("identity_or_path_mismatch");
    if(!allowReceipt(matrix+L"\\matrix\\.tmpio5Sbs\\allow"))hold("allow_receipt_mismatch");
    if(!bindOriginal(runner.handle,caseProcess,"runner_to_case")||!bindOriginal(supervisor.handle,runner,"supervisor_to_runner")||!bindOriginal(launcher.handle,supervisor,"launcher_to_supervisor"))hold("original_parent_handle_unconfirmed");
    if(!sameCopies(caseProcess.handle,(HANDLE)(uintptr_t)284,job,L"Job")||!sameCopies(caseProcess.handle,(HANDLE)(uintptr_t)296,helper,L"Process"))hold("original_job_helper_objects_unconfirmed");
    bool initialSources=sourceStillOriginal();bool initialFacts=jobAndHelper();
    if(!initialSources||!initialFacts)hold("initial_job_helper_proof_failed");
    Sleep(250);bool repeatSources=sourceStillOriginal();bool repeatFacts=jobAndHelper();
    if(!repeatSources||!repeatFacts)hold("repeated_job_helper_proof_failed");
    // No names or PID-derived process re-open are used for the helper.
    std::puts("PREFLIGHT_READY: object candidates retained; each stage requires a new request file");
    waitRequest(base,L"01-case");
    if(!identity(caseProcess,argv[2],true)||!identity(runner,argv[3],true)||!sourceStillOriginal()||!jobAndHelper())hold("case_preconditions_changed");
    preflightPhase=false;
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
    for(const auto& r:retained){if(!CloseHandle(r.copy))hold("release_duplicate_failed");}
    for(auto* x : {&caseProcess,&runner,&supervisor,&launcher}){if(!CloseHandle(x->handle))hold("release_process_handle_failed");}
    std::puts("RECOVERY_LOCAL_HANDLES_RELEASED: gate files remain; original matrix case outcome remains unknown");return 0;
}
int wmain(int argc,wchar_t**argv){try{return run(argc,argv);}catch(...){hold("cpp_exception");}}
