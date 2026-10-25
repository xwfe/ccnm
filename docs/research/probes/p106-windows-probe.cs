// P106 实测用的小程序：Windows 自带 .NET Framework 的 csc.exe 编译（C# 5）。
// 只在 Win11 测试机的临时目录里跑，做完删掉。
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;

static class P106
{
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr GetStdHandle(int nStdHandle);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetHandleInformation(IntPtr h, uint mask, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetHandleInformation(IntPtr h, out uint flags);
    [DllImport("kernel32.dll")]
    static extern uint GetFileType(IntPtr h);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool IsProcessInJob(IntPtr process, IntPtr job, out bool result);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool QueryInformationJobObject(IntPtr job, int cls, byte[] info, int len, out int ret);
    [DllImport("kernel32.dll")]
    static extern IntPtr GetCurrentProcess();

    const uint HANDLE_FLAG_INHERIT = 1;
    static readonly int[] StdIds = { -10, -11, -12 };
    static readonly string[] StdNames = { "stdin", "stdout", "stderr" };

    static int Main(string[] args)
    {
        string mode = args.Length > 0 ? args[0] : "";
        switch (mode)
        {
            case "args": return Args(args);
            case "echo": return Echo();
            case "sleep": Thread.Sleep(int.Parse(args[1]) * 1000); return 0;
            case "tree": return Tree(args[1], int.Parse(args[2]), int.Parse(args[3]), args[4] == "orphan");
            case "spawnbg": return SpawnBg(args[1] == "noinherit", args[2]);
            case "injob": return InJob();
            case "sink": return Sink();
            case "sink-nt": return SinkWith(ReadNt);
            case "sink-ov": return SinkWith(ReadOverlapped);
            case "echo-nt": return EchoNt();
            case "mode": return Mode();
            case "echo-rs": return EchoRs(args.Length > 1 && args[1] == "awake");
            case "awake-check": return AwakeCheck();
            case "echo-mt": return EchoMt(args.Length > 1 && args[1] == "awake");
            case "awake": SetThreadExecutionState(0x80000000 | 0x1); Thread.Sleep(int.Parse(args[1]) * 1000); return 0;
            case "names": return Names();
            case "source": return Source(long.Parse(args[1]));
            default: Console.Error.WriteLine("modes: args echo sleep tree spawnbg injob"); return 2;
        }
    }

    static int Args(string[] args)
    {
        Console.WriteLine("argc=" + args.Length);
        for (int i = 0; i < args.Length; i++) Console.WriteLine("[" + i + "]=" + args[i]);
        Console.WriteLine("cmdline=" + Environment.CommandLine);
        string t = Environment.GetEnvironmentVariable("CCNM_TRANSPORT");
        Console.WriteLine("CCNM_TRANSPORT=" + (t == null ? "(unset)" : t));
        Console.WriteLine("HOME=" + Environment.GetEnvironmentVariable("HOME"));
        for (int i = 0; i < 3; i++)
        {
            IntPtr h = GetStdHandle(StdIds[i]);
            uint flags;
            GetHandleInformation(h, out flags);
            // GetFileType: 1 磁盘文件，2 字符设备（控制台），3 管道
            Console.WriteLine(StdNames[i] + " type=" + GetFileType(h) + " inherit=" + (flags & HANDLE_FLAG_INHERIT));
        }
        return 0;
    }

    // 标准输入原样抄到标准输出，读到 EOF 退出；每次读到就 flush，模拟 MCP 一来一回。
    static int Echo()
    {
        Stream i = Console.OpenStandardInput();
        Stream o = Console.OpenStandardOutput();
        byte[] buf = new byte[65536];
        long total = 0;
        int n;
        while ((n = i.Read(buf, 0, buf.Length)) > 0)
        {
            o.Write(buf, 0, n);
            o.Flush();
            total += n;
        }
        Console.Error.WriteLine("echo done bytes=" + total);
        return 0;
    }

    // 三层进程树：每层把自己的 pid 和父 pid 追加到文件；orphan 时中间那层起完孩子就退出，
    // 留下父进程已经不在的孙进程。孩子都不重定向、不弹窗。
    static int Tree(string file, int depth, int secs, bool orphan)
    {
        int me = Process.GetCurrentProcess().Id;
        if (depth > 1)
        {
            ProcessStartInfo si = new ProcessStartInfo(Process.GetCurrentProcess().MainModule.FileName,
                "tree \"" + file + "\" " + (depth - 1) + " " + secs + " " + (orphan ? "orphan" : "keep"));
            si.UseShellExecute = false;
            si.CreateNoWindow = true;
            Process c = Process.Start(si);
            File.AppendAllText(file, "depth=" + depth + " pid=" + me + " child=" + c.Id + "\r\n");
            if (orphan && depth == 2) return 0;
        }
        else
        {
            File.AppendAllText(file, "depth=" + depth + " pid=" + me + " leaf\r\n");
        }
        Thread.Sleep(secs * 1000);
        return 0;
    }

    // 起一个后台子进程就退出。子进程三路都重定向成管道（和 Rust 的 Stdio::piped/null 一样，
    // 子进程自己的标准句柄不是 sshd 那条通道），但 CreateProcess 的 bInheritHandles 是 TRUE，
    // 本进程所有可继承的句柄都会被它带走。noinherit 时先把本进程三个标准句柄改成不可继承。
    static int SpawnBg(bool noinherit, string secs)
    {
        if (noinherit)
            for (int i = 0; i < 3; i++)
                if (!SetHandleInformation(GetStdHandle(StdIds[i]), HANDLE_FLAG_INHERIT, 0))
                    Console.Error.WriteLine("SetHandleInformation " + StdNames[i] + " failed " + Marshal.GetLastWin32Error());
        ProcessStartInfo si = new ProcessStartInfo(Process.GetCurrentProcess().MainModule.FileName, "sleep " + secs);
        si.UseShellExecute = false;
        si.CreateNoWindow = true;
        si.RedirectStandardInput = true;
        si.RedirectStandardOutput = true;
        si.RedirectStandardError = true;
        Process c = Process.Start(si);
        Console.WriteLine("spawned pid=" + c.Id + " noinherit=" + noinherit);
        Console.Out.Flush();
        return 0;
    }

    // 只收：读到 EOF，把字节数和 sha256 打到标准输出。
    static int Sink()
    {
        Stream i = Console.OpenStandardInput();
        System.Security.Cryptography.SHA256 sha = System.Security.Cryptography.SHA256.Create();
        byte[] buf = new byte[65536];
        long total = 0;
        int n;
        while ((n = i.Read(buf, 0, buf.Length)) > 0)
        {
            sha.TransformBlock(buf, 0, n, null, 0);
            total += n;
        }
        sha.TransformFinalBlock(buf, 0, 0);
        Console.WriteLine("sink bytes=" + total + " sha256=" + BitConverter.ToString(sha.Hash).Replace("-", "").ToLower());
        return 0;
    }

    // 只发：写 n 个字节（0..255 循环），sha256 打到标准错误。
    static int Source(long n)
    {
        Stream o = Console.OpenStandardOutput();
        System.Security.Cryptography.SHA256 sha = System.Security.Cryptography.SHA256.Create();
        byte[] buf = new byte[65536];
        for (int k = 0; k < buf.Length; k++) buf[k] = (byte)k;
        long left = n;
        while (left > 0)
        {
            int m = (int)Math.Min(left, buf.Length);
            o.Write(buf, 0, m);
            sha.TransformBlock(buf, 0, m, null, 0);
            left -= m;
        }
        o.Flush();
        sha.TransformFinalBlock(buf, 0, 0);
        Console.Error.WriteLine("source bytes=" + n + " sha256=" + BitConverter.ToString(sha.Hash).Replace("-", "").ToLower());
        return 0;
    }

    // ---- 异步句柄的两种读法（2026-10-25 加）----
    // Win32-OpenSSH 给子进程的 stdin 是带 FILE_FLAG_OVERLAPPED 的命名管道。.NET Framework
    // 用同步 ReadFile 读这种句柄会卡住；下面两种读法用来区分"读法的问题"和"管道本身的问题"。

    [DllImport("ntdll.dll")]
    static extern int NtReadFile(IntPtr file, IntPtr ev, IntPtr apc, IntPtr apcCtx, IntPtr iosb,
        IntPtr buf, uint len, IntPtr offset, IntPtr key);
    [DllImport("ntdll.dll")]
    static extern int NtQueryInformationFile(IntPtr file, IntPtr iosb, out uint info, uint len, int cls);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool ReadFile(IntPtr h, IntPtr buf, uint len, IntPtr read, IntPtr ovl);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetOverlappedResult(IntPtr h, IntPtr ovl, out uint n, bool wait);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr CreateEvent(IntPtr attr, bool manual, bool initial, string name);

    const int STATUS_PENDING = 0x103;
    const int STATUS_END_OF_FILE = unchecked((int)0xC0000011);
    const int STATUS_PIPE_BROKEN = unchecked((int)0xC000014B);
    const int CHUNK = 65536;
    static readonly IntPtr NtBuf = Marshal.AllocHGlobal(CHUNK);   // 异步 I/O 完成前内存不能动，所以放非托管内存
    static readonly IntPtr NtIosb = Marshal.AllocHGlobal(16);     // IO_STATUS_BLOCK：x64 上 Status 8 字节 + Information 8 字节
    static readonly IntPtr Ovl = Marshal.AllocHGlobal(32);        // OVERLAPPED：x64 上 32 字节，hEvent 在偏移 24

    // 照抄 Rust std 的 Handle::synchronous_read：NtReadFile 不带事件，挂起就等句柄本身。
    static int ReadNt(IntPtr h)
    {
        Marshal.WriteInt64(NtIosb, 0, STATUS_PENDING);
        Marshal.WriteInt64(NtIosb, 8, 0);
        int st = NtReadFile(h, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, NtIosb, NtBuf, CHUNK, IntPtr.Zero, IntPtr.Zero);
        if (st == STATUS_PENDING)
        {
            WaitForSingleObject(h, 0xFFFFFFFF);
            st = Marshal.ReadInt32(NtIosb, 0);
        }
        if (st == STATUS_END_OF_FILE || st == STATUS_PIPE_BROKEN) return 0;
        if (st == STATUS_PENDING) throw new Exception("still pending after wait");
        if (st < 0) throw new Exception("NtReadFile status 0x" + st.ToString("x"));
        return (int)Marshal.ReadInt64(NtIosb, 8);
    }

    // 正规的异步读：OVERLAPPED 带自己的事件，ERROR_IO_PENDING 时 GetOverlappedResult 等完成。
    static IntPtr OvlEvent = IntPtr.Zero;
    static int ReadOverlapped(IntPtr h)
    {
        if (OvlEvent == IntPtr.Zero) OvlEvent = CreateEvent(IntPtr.Zero, true, false, null);
        for (int k = 0; k < 32; k += 8) Marshal.WriteInt64(Ovl, k, 0);
        Marshal.WriteIntPtr(Ovl, 24, OvlEvent);
        uint n;
        if (!ReadFile(h, NtBuf, CHUNK, IntPtr.Zero, Ovl))
        {
            int e = Marshal.GetLastWin32Error();
            if (e == 109 || e == 38) return 0;           // ERROR_BROKEN_PIPE、ERROR_HANDLE_EOF
            if (e != 997) throw new Exception("ReadFile error " + e);   // 997 = ERROR_IO_PENDING
        }
        if (!GetOverlappedResult(h, Ovl, out n, true))
        {
            int e = Marshal.GetLastWin32Error();
            if (e == 109 || e == 38) return 0;
            throw new Exception("GetOverlappedResult error " + e);
        }
        return (int)n;
    }

    static int SinkWith(Func<IntPtr, int> read)
    {
        IntPtr h = GetStdHandle(-10);
        System.Security.Cryptography.SHA256 sha = System.Security.Cryptography.SHA256.Create();
        byte[] buf = new byte[CHUNK];
        long total = 0;
        int n;
        while ((n = read(h)) > 0)
        {
            Marshal.Copy(NtBuf, buf, 0, n);
            sha.TransformBlock(buf, 0, n, null, 0);
            total += n;
        }
        sha.TransformFinalBlock(buf, 0, 0);
        Console.WriteLine("sink bytes=" + total + " sha256=" + BitConverter.ToString(sha.Hash).Replace("-", "").ToLower());
        return 0;
    }

    static int EchoNt()
    {
        IntPtr h = GetStdHandle(-10);
        Stream o = Console.OpenStandardOutput();
        byte[] buf = new byte[CHUNK];
        long total = 0;
        int n;
        while ((n = ReadNt(h)) > 0)
        {
            Marshal.Copy(NtBuf, buf, 0, n);
            o.Write(buf, 0, n);
            o.Flush();
            total += n;
        }
        Console.Error.WriteLine("echo-nt done bytes=" + total);
        return 0;
    }

    [DllImport("ntdll.dll")]
    static extern int NtWriteFile(IntPtr file, IntPtr ev, IntPtr apc, IntPtr apcCtx, IntPtr iosb,
        IntPtr buf, uint len, IntPtr offset, IntPtr key);
    [DllImport("kernel32.dll")]
    static extern uint SetThreadExecutionState(uint flags);

    // 照抄 Rust std 的 Handle::synchronous_write，并像 write_all 一样写到全部写完。
    static void WriteNt(IntPtr h, IntPtr buf, int len)
    {
        int done = 0;
        while (done < len)
        {
            Marshal.WriteInt64(NtIosb, 0, STATUS_PENDING);
            Marshal.WriteInt64(NtIosb, 8, 0);
            int st = NtWriteFile(h, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, NtIosb,
                new IntPtr(buf.ToInt64() + done), (uint)(len - done), IntPtr.Zero, IntPtr.Zero);
            if (st == STATUS_PENDING)
            {
                WaitForSingleObject(h, 0xFFFFFFFF);
                st = Marshal.ReadInt32(NtIosb, 0);
            }
            if (st == STATUS_PENDING) throw new Exception("write still pending after wait");
            if (st < 0) throw new Exception("NtWriteFile status 0x" + st.ToString("x"));
            done += (int)Marshal.ReadInt64(NtIosb, 8);
        }
    }

    // 读写都用 Rust std 的办法；awake 时请求系统运行期间别睡（ES_CONTINUOUS | ES_SYSTEM_REQUIRED），进程退出就失效。
    static int EchoRs(bool awake)
    {
        if (awake) SetThreadExecutionState(0x80000000 | 0x1);
        IntPtr hin = GetStdHandle(-10), hout = GetStdHandle(-11);
        long total = 0;
        int n;
        while ((n = ReadNt(hin)) > 0)
        {
            WriteNt(hout, NtBuf, n);
            total += n;
        }
        Console.Error.WriteLine("echo-rs done bytes=" + total);
        return 0;
    }

    // 和上面两个一样，但缓冲和 IO_STATUS_BLOCK 由调用方给，两个线程各用各的。
    static int ReadNtWith(IntPtr h, IntPtr buf, IntPtr iosb, int len)
    {
        Marshal.WriteInt64(iosb, 0, STATUS_PENDING);
        Marshal.WriteInt64(iosb, 8, 0);
        int st = NtReadFile(h, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, iosb, buf, (uint)len, IntPtr.Zero, IntPtr.Zero);
        if (st == STATUS_PENDING)
        {
            WaitForSingleObject(h, 0xFFFFFFFF);
            st = Marshal.ReadInt32(iosb, 0);
        }
        if (st == STATUS_END_OF_FILE || st == STATUS_PIPE_BROKEN) return 0;
        if (st == STATUS_PENDING) throw new Exception("read still pending after wait");
        if (st < 0) throw new Exception("NtReadFile status 0x" + st.ToString("x"));
        return (int)Marshal.ReadInt64(iosb, 8);
    }

    static void WriteNtWith(IntPtr h, IntPtr buf, IntPtr iosb, int len)
    {
        int done = 0;
        while (done < len)
        {
            Marshal.WriteInt64(iosb, 0, STATUS_PENDING);
            Marshal.WriteInt64(iosb, 8, 0);
            int st = NtWriteFile(h, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, iosb,
                new IntPtr(buf.ToInt64() + done), (uint)(len - done), IntPtr.Zero, IntPtr.Zero);
            if (st == STATUS_PENDING)
            {
                WaitForSingleObject(h, 0xFFFFFFFF);
                st = Marshal.ReadInt32(iosb, 0);
            }
            if (st == STATUS_PENDING) throw new Exception("write still pending after wait");
            if (st < 0) throw new Exception("NtWriteFile status 0x" + st.ToString("x"));
            done += (int)Marshal.ReadInt64(iosb, 8);
        }
    }

    // ccnm 的 mcp-serve 是一个线程读 stdin、另一个线程写 stdout。这里照这个结构：
    // 读线程只管读、塞进队列，写线程只管写，谁也不等谁。
    static int EchoMt(bool awake)
    {
        if (awake) SetThreadExecutionState(0x80000000 | 0x1);
        IntPtr hin = GetStdHandle(-10), hout = GetStdHandle(-11);
        System.Collections.Concurrent.BlockingCollection<byte[]> q = new System.Collections.Concurrent.BlockingCollection<byte[]>();
        long readTotal = 0, writeTotal = 0;
        Thread w = new Thread(() =>
        {
            IntPtr wb = Marshal.AllocHGlobal(CHUNK), wi = Marshal.AllocHGlobal(16);
            foreach (byte[] c in q.GetConsumingEnumerable())
            {
                Marshal.Copy(c, 0, wb, c.Length);
                WriteNtWith(hout, wb, wi, c.Length);
                writeTotal += c.Length;
            }
        });
        w.Start();
        IntPtr rb = Marshal.AllocHGlobal(CHUNK), ri = Marshal.AllocHGlobal(16);
        int n;
        while ((n = ReadNtWith(hin, rb, ri, CHUNK)) > 0)
        {
            byte[] c = new byte[n];
            Marshal.Copy(rb, c, 0, n);
            q.Add(c);
            readTotal += n;
        }
        q.CompleteAdding();
        w.Join();
        Console.Error.WriteLine("echo-mt done read=" + readTotal + " written=" + writeTotal);
        return 0;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandleEx(IntPtr h, int cls, IntPtr info, uint size);
    [DllImport("kernelbase.dll")]
    static extern bool CompareObjectHandles(IntPtr a, IntPtr b);

    // 三个标准句柄背后的管道名，以及 stdin 和 stdout 是不是同一个内核对象。
    static int Names()
    {
        IntPtr info = Marshal.AllocHGlobal(1024);
        for (int i = 0; i < 3; i++)
        {
            if (GetFileInformationByHandleEx(GetStdHandle(StdIds[i]), 2, info, 1024))   // FileNameInfo
            {
                int len = Marshal.ReadInt32(info, 0);
                Console.WriteLine(StdNames[i] + " name=" + Marshal.PtrToStringUni(new IntPtr(info.ToInt64() + 4), len / 2));
            }
            else Console.WriteLine(StdNames[i] + " name error " + Marshal.GetLastWin32Error());
        }
        Console.WriteLine("stdin==stdout same object: " + CompareObjectHandles(GetStdHandle(-10), GetStdHandle(-11)));
        Console.WriteLine("stdout==stderr same object: " + CompareObjectHandles(GetStdHandle(-11), GetStdHandle(-12)));
        return 0;
    }

    static int AwakeCheck()
    {
        uint prev = SetThreadExecutionState(0x80000000 | 0x1);
        Console.WriteLine("SetThreadExecutionState prev=0x" + prev.ToString("x"));
        ProcessStartInfo si = new ProcessStartInfo("powercfg", "/requests");
        si.UseShellExecute = false;
        si.RedirectStandardOutput = true;
        si.RedirectStandardInput = true;
        si.CreateNoWindow = true;
        Process p = Process.Start(si);
        p.StandardInput.Close();
        Console.WriteLine(p.StandardOutput.ReadToEnd());
        p.WaitForExit();
        return 0;
    }

    // FileModeInformation(16)：带 FILE_SYNCHRONOUS_IO_ALERT(0x10) 或 _NONALERT(0x20) 就是同步句柄，都没有就是异步。
    static int Mode()
    {
        for (int i = 0; i < 3; i++)
        {
            uint mode;
            int st = NtQueryInformationFile(GetStdHandle(StdIds[i]), NtIosb, out mode, 4, 16);
            Console.WriteLine(StdNames[i] + " status=0x" + st.ToString("x") + " mode=0x" + mode.ToString("x") +
                ((mode & 0x30) == 0 ? " ASYNC" : " sync"));
        }
        return 0;
    }

    static int InJob()
    {
        bool inJob;
        IsProcessInJob(GetCurrentProcess(), IntPtr.Zero, out inJob);
        Console.WriteLine("in_job=" + inJob);
        if (inJob)
        {
            // JobObjectBasicLimitInformation(2)：x64 上 64 字节，LimitFlags 在偏移 16。
            byte[] info = new byte[64];
            int ret;
            if (QueryInformationJobObject(IntPtr.Zero, 2, info, info.Length, out ret))
                Console.WriteLine("limit_flags=0x" + BitConverter.ToUInt32(info, 16).ToString("x"));
            else
                Console.WriteLine("query failed " + Marshal.GetLastWin32Error());
        }
        return 0;
    }
}
