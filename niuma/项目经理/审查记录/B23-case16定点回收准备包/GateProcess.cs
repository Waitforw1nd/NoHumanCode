using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

// Direct file handles are inherited by the child; no pipe pump is needed.
public static class B21GateProcess {
    [StructLayout(LayoutKind.Sequential)] struct SECURITY_ATTRIBUTES { public int length; public IntPtr descriptor; public int inherit; }
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct STARTUPINFO {
        public int cb; public string reserved, desktop, title; public int x,y,xs,ys,xc,yc,fill,flags; public short show,reserved2;
        public IntPtr reservedPtr, stdin, stdout, stderr;
    }
    [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr process, thread; public int pid, tid; }
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern SafeFileHandle CreateFileW(string name,uint access,uint share,ref SECURITY_ATTRIBUTES sa,uint creation,uint flags,IntPtr template);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateProcessW(string app,StringBuilder command,IntPtr pa,IntPtr ta,bool inherit,uint flags,IntPtr env,string dir,ref STARTUPINFO si,out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(SafeFileHandle process,out long created,out long exited,out long kernel,out long user);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(SafeFileHandle process,uint timeout);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(SafeFileHandle process,out uint code);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint ResumeThread(SafeFileHandle thread);
    public sealed class Started {
        public SafeFileHandle Handle, Thread;
        public int Pid, CreationError, LastError;
        public long CreationFileTime;
        public bool Resume() { var r=ResumeThread(Thread); LastError=r==uint.MaxValue?Marshal.GetLastWin32Error():0; return r!=uint.MaxValue; }
        public uint Wait(uint ms) { var r=WaitForSingleObject(Handle,ms); LastError=r==uint.MaxValue?Marshal.GetLastWin32Error():0; return r; }
        public bool ExitCode(out uint code) { bool ok=GetExitCodeProcess(Handle,out code); LastError=ok?0:Marshal.GetLastWin32Error(); return ok; }
    }
    public static Started Start(string exe,string arguments,string cwd,string stdout,string stderr) {
        var sa=new SECURITY_ATTRIBUTES { length=Marshal.SizeOf(typeof(SECURITY_ATTRIBUTES)), inherit=1 };
        using(var o=CreateFileW(stdout,0x40000000,3,ref sa,1,0,IntPtr.Zero))
        using(var e=CreateFileW(stderr,0x40000000,3,ref sa,1,0,IntPtr.Zero))
        using(var i=CreateFileW("NUL",0x80000000,3,ref sa,3,0,IntPtr.Zero)) {
            if(o.IsInvalid || e.IsInvalid || i.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
            var si=new STARTUPINFO { cb=Marshal.SizeOf(typeof(STARTUPINFO)), flags=0x100, stdin=i.DangerousGetHandle(), stdout=o.DangerousGetHandle(), stderr=e.DangerousGetHandle() };
            PROCESS_INFORMATION pi;
            // CREATE_SUSPENDED | CREATE_NO_WINDOW. No new visible console is requested.
            if(!CreateProcessW(exe,new StringBuilder("\""+exe+"\" "+arguments),IntPtr.Zero,IntPtr.Zero,true,0x08000004,IntPtr.Zero,cwd,ref si,out pi)) throw new Win32Exception(Marshal.GetLastWin32Error());
            var result=new Started { Handle=new SafeFileHandle(pi.process,true), Thread=new SafeFileHandle(pi.thread,true), Pid=pi.pid };
            long created, exited, kernel, user;
            if(GetProcessTimes(result.Handle,out created,out exited,out kernel,out user)) result.CreationFileTime=created;
            else result.CreationError=Marshal.GetLastWin32Error();
            return result;
        }
    }
}
