using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

// Direct file handles survive the runner; no pipe reader must remain alive.
public static class NhcDirectProcess {
    [StructLayout(LayoutKind.Sequential)] struct SECURITY_ATTRIBUTES { public int length; public IntPtr descriptor; public int inherit; }
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct STARTUPINFO {
        public int cb; public string reserved, desktop, title; public int x,y,xs,ys,xc,yc,fill,flags; public short show,reserved2;
        public IntPtr reservedPtr, stdin, stdout, stderr;
    }
    [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr process, thread; public int pid, tid; }
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern SafeFileHandle CreateFileW(string name,uint access,uint share,ref SECURITY_ATTRIBUTES sa,uint creation,uint flags,IntPtr template);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateProcessW(string app,StringBuilder command,IntPtr pa,IntPtr ta,bool inherit,uint flags,IntPtr env,string dir,ref STARTUPINFO si,out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(SafeFileHandle process,out long created,out long exited,out long kernel,out long user);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint ResumeThread(SafeFileHandle thread);
    public sealed class Started {
        public SafeFileHandle Handle, Thread; public int Pid; public long CreationFileTime;
        public void Resume() { if(ResumeThread(Thread)==uint.MaxValue) throw new Win32Exception(Marshal.GetLastWin32Error()); }
    }
    public static Started Start(string exe,string arguments,string cwd,string stdout,string stderr) {
        var sa=new SECURITY_ATTRIBUTES { length=Marshal.SizeOf(typeof(SECURITY_ATTRIBUTES)), inherit=1 };
        using(var o=CreateFileW(stdout,0x40000000,3,ref sa,1,0,IntPtr.Zero))
        using(var e=CreateFileW(stderr,0x40000000,3,ref sa,1,0,IntPtr.Zero))
        using(var i=CreateFileW("NUL",0x80000000,3,ref sa,3,0,IntPtr.Zero)) {
            if(o.IsInvalid || e.IsInvalid || i.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
            var si=new STARTUPINFO { cb=Marshal.SizeOf(typeof(STARTUPINFO)), flags=0x100, stdin=i.DangerousGetHandle(), stdout=o.DangerousGetHandle(), stderr=e.DangerousGetHandle() };
            PROCESS_INFORMATION pi;
            if(!CreateProcessW(exe,new StringBuilder("\""+exe+"\" "+arguments),IntPtr.Zero,IntPtr.Zero,true,4,IntPtr.Zero,cwd,ref si,out pi)) throw new Win32Exception(Marshal.GetLastWin32Error());
            var handle=new SafeFileHandle(pi.process,true);
            long created, exited, kernel, user;
            if(!GetProcessTimes(handle,out created,out exited,out kernel,out user)) created=0;
            return new Started { Handle=handle, Thread=new SafeFileHandle(pi.thread,true), Pid=pi.pid, CreationFileTime=created };
        }
    }
}
