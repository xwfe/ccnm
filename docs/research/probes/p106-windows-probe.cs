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
