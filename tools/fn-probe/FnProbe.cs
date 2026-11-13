// FnProbe — looks for ANY signal Windows can see when the Fn key is pressed alone.
//
// Read-only. Nothing is written to the device; ASUS ACPI is only queried with DSTS ("get device status").
// Run it at the laptop's own keyboard (over Remote Desktop, Fn never reaches the remote PC).
//
// Channels:
//   1. WH_KEYBOARD_LL keyboard hook          (scan codes)
//   2. Raw Input from every HID collection   (keyboard, consumer, vendor pages …)
//   3. Vendor WMI events in root\wmi          (ASUS ATK, Lenovo, ThinkPad … every event class it finds)
//   4. ASUS only: ATKACPI DSTS scan           (compare thousands of status values with Fn held vs. released)
//
// Build: csc -nologo -platform:x64 -r:System.Management.dll -r:System.Windows.Forms.dll -out:FnProbe.exe FnProbe.cs
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Management;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Windows.Forms;
using Microsoft.Win32;
using Microsoft.Win32.SafeHandles;

static class FnProbe
{
    // ───────────────────────── report ─────────────────────────
    static readonly StringBuilder Report = new StringBuilder();
    static readonly Stopwatch Clock = Stopwatch.StartNew();
    static readonly object Gate = new object();
    static string phase = "";
    static bool recording = true;
    /// phase -> signature -> count. A signature identifies a source (device, WMI class, virtual key …).
    static readonly Dictionary<string, Dictionary<string, int>> Hits = new Dictionary<string, Dictionary<string, int>>();

    static void Say(string s) { lock (Gate) { Console.WriteLine(s); Report.AppendLine(s); } }
    static void Event(string channel, string text, string sig)
    {
        lock (Gate)
        {
            if (!recording) return;
            string line = string.Format("  {0,7:0.000}s [{1}] {2,-9} {3}", Clock.Elapsed.TotalSeconds, phase, channel, text);
            Console.WriteLine(line);
            Report.AppendLine(line);
            Dictionary<string, int> d;
            if (!Hits.TryGetValue(phase, out d)) Hits[phase] = d = new Dictionary<string, int>();
            int n; d.TryGetValue(sig, out n); d[sig] = n + 1;
        }
    }

    static bool Korean = System.Globalization.CultureInfo.CurrentUICulture.TwoLetterISOLanguageName == "ko";
    static string T(string en, string ko) { return Korean ? ko : en; }

    // ───────────────────────── keyboard hook ─────────────────────────
    delegate IntPtr HookProc(int code, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] static extern IntPtr SetWindowsHookEx(int id, HookProc p, IntPtr mod, uint tid);
    [DllImport("user32.dll")] static extern IntPtr CallNextHookEx(IntPtr h, int c, IntPtr w, IntPtr l);
    [DllImport("kernel32.dll")] static extern IntPtr GetModuleHandle(string n);
    [StructLayout(LayoutKind.Sequential)] struct KB { public uint vk, scan, flags, time; public IntPtr extra; }
    static HookProc hookProc;

    // ───────────────────────── raw input ─────────────────────────
    const int WM_INPUT = 0x00FF;
    const uint RIDEV_INPUTSINK = 0x100, RIM_TYPEHID = 2, RIM_TYPEKEYBOARD = 1, RIDI_DEVICEINFO = 0x2000000b, RID_INPUT = 0x10000003, RIDI_DEVICENAME = 0x20000007;
    [StructLayout(LayoutKind.Sequential)] struct RID { public ushort page, usage; public uint flags; public IntPtr target; }
    [StructLayout(LayoutKind.Sequential)] struct RIDL { public IntPtr dev; public uint type; }
    [DllImport("user32.dll")] static extern bool RegisterRawInputDevices(RID[] d, uint n, uint sz);
    [DllImport("user32.dll")] static extern uint GetRawInputDeviceList([Out] RIDL[] l, ref uint n, uint sz);
    [DllImport("user32.dll")] static extern uint GetRawInputDeviceInfo(IntPtr d, uint cmd, IntPtr data, ref uint sz);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern uint GetRawInputDeviceInfo(IntPtr d, uint cmd, StringBuilder data, ref uint sz);
    [DllImport("user32.dll")] static extern uint GetRawInputData(IntPtr h, uint cmd, IntPtr data, ref uint sz, uint hdr);
    static readonly Dictionary<IntPtr, string> DevNames = new Dictionary<IntPtr, string>();

    class Sink : NativeWindow
    {
        public Sink() { CreateHandle(new CreateParams()); }
        protected override void WndProc(ref Message m)
        {
            if (m.Msg == WM_INPUT) OnRawInput(m.LParam);
            base.WndProc(ref m);
        }
    }

    static void OnRawInput(IntPtr h)
    {
        int hdr = IntPtr.Size * 2 + 8;
        uint size = 0;
        GetRawInputData(h, RID_INPUT, IntPtr.Zero, ref size, (uint)hdr);
        IntPtr buf = Marshal.AllocHGlobal((int)size);
        try
        {
            if (GetRawInputData(h, RID_INPUT, buf, ref size, (uint)hdr) != size) return;
            uint type = (uint)Marshal.ReadInt32(buf);
            IntPtr dev = Marshal.ReadIntPtr(buf, 8);
            string dn; DevNames.TryGetValue(dev, out dn);
            if (type == RIM_TYPEHID)
            {
                uint each = (uint)Marshal.ReadInt32(buf, hdr), count = (uint)Marshal.ReadInt32(buf, hdr + 4);
                var bytes = new byte[Math.Min(64, each * count)];
                Marshal.Copy(buf + hdr + 8, bytes, 0, bytes.Length);
                Event("RAW HID", (dn ?? "?") + "  " + BitConverter.ToString(bytes), "HID " + (dn ?? "?") + " " + BitConverter.ToString(bytes, 0, Math.Min(bytes.Length, 16)));  // payload included: an Fn bit may flip inside a periodic report
            }
            else if (type == RIM_TYPEKEYBOARD)
            {
                ushort make = (ushort)Marshal.ReadInt16(buf, hdr), fl = (ushort)Marshal.ReadInt16(buf, hdr + 2), vk = (ushort)Marshal.ReadInt16(buf, hdr + 6);
                Event("RAW KBD", string.Format("make=0x{0:X2} flags=0x{1:X} vk=0x{2:X2}  {3}", make, fl, vk, dn ?? ""), string.Format("KEY vk=0x{0:X2} make=0x{1:X2}", vk, make));
            }
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    static void RegisterRawInput(IntPtr hwnd)
    {
        uint n = 0;
        GetRawInputDeviceList(null, ref n, (uint)Marshal.SizeOf(typeof(RIDL)));
        var list = new RIDL[n];
        GetRawInputDeviceList(list, ref n, (uint)Marshal.SizeOf(typeof(RIDL)));
        var regs = new List<RID> { new RID { page = 1, usage = 6, flags = RIDEV_INPUTSINK, target = hwnd } };
        var seen = new HashSet<uint> { (1u << 16) | 6 };
        foreach (var d in list)
        {
            uint sz = 512; var sb = new StringBuilder(512);
            GetRawInputDeviceInfo(d.dev, RIDI_DEVICENAME, sb, ref sz);
            string name = sb.ToString();
            int cut = name.IndexOf("#{"); if (cut > 0) name = name.Substring(0, cut);
            if (d.type == RIM_TYPEHID)
            {
                // RID_DEVICE_INFO is 32 bytes; for HID the usage page/usage sit at offsets 20/22.
                uint isz = 32; IntPtr p = Marshal.AllocHGlobal(32);
                for (int i = 0; i < 32; i += 4) Marshal.WriteInt32(p, i, 0);
                Marshal.WriteInt32(p, 32);
                uint got = GetRawInputDeviceInfo(d.dev, RIDI_DEVICEINFO, p, ref isz);
                ushort page = (ushort)Marshal.ReadInt16(p, 20), usage = (ushort)Marshal.ReadInt16(p, 22);
                Marshal.FreeHGlobal(p);
                if (got == uint.MaxValue || page == 0) { DevNames[d.dev] = "HID (info unavailable) " + name; continue; }
                DevNames[d.dev] = string.Format("HID page 0x{0:X4}/0x{1:X2} {2}", page, usage, name);
                // Skip pointer/touch collections (constant noise while the finger moves).
                bool noisy = page == 0x0D || (page == 1 && (usage == 1 || usage == 2));
                if (!noisy && seen.Add(((uint)page << 16) | usage))
                    regs.Add(new RID { page = page, usage = usage, flags = RIDEV_INPUTSINK, target = hwnd });
            }
            else if (d.type == RIM_TYPEKEYBOARD) DevNames[d.dev] = "keyboard " + name;
        }
        bool ok = RegisterRawInputDevices(regs.ToArray(), (uint)regs.Count, (uint)Marshal.SizeOf(typeof(RID)));
        Say(T("Raw input: ", "원시 입력: ") + regs.Count + T(" collections registered (", "개 컬렉션 등록 (") + (ok ? "OK" : "FAILED") + ")");
        foreach (var kv in DevNames) Say("    " + kv.Value);
    }

    // ───────────────────────── vendor WMI events ─────────────────────────
    static readonly List<ManagementEventWatcher> Watchers = new List<ManagementEventWatcher>();

    static void WatchVendorWmi()
    {
        try
        {
            var scope = new ManagementScope(@"root\wmi");
            scope.Connect();
            var names = new List<string>();
            using (var s = new ManagementObjectSearcher(scope, new SelectQuery("meta_class")))
                foreach (ManagementClass c in s.Get())
                {
                    string n = c["__CLASS"] as string ?? "";
                    var path = c.Derivation;
                    bool isEvent = path != null && path.Cast<string>().Any(x => x == "WMIEvent");
                    if (isEvent && System.Text.RegularExpressions.Regex.IsMatch(n, "asus|atk|lenovo|lnv|ibm|thinkpad|tpacpi|hotkey|fn", System.Text.RegularExpressions.RegexOptions.IgnoreCase))
                        names.Add(n);
                }
            if (names.Count == 0) Say(T("Vendor WMI events: none found in root\\wmi", "제조사 WMI 이벤트: root\\wmi에 없음"));
            foreach (var n in names)
            {
                try
                {
                    var w = new ManagementEventWatcher(scope, new EventQuery("SELECT * FROM " + n));
                    string cls = n;
                    w.EventArrived += (s, e) =>
                    {
                        var parts = new List<string>();
                        foreach (var prop in e.NewEvent.Properties)
                            if (!prop.Name.StartsWith("__") && prop.Name != "InstanceName" && prop.Name != "Active")
                                parts.Add(prop.Name + "=" + FormatValue(prop.Value));
                        Event("WMI", cls + " " + string.Join(" ", parts), "WMI " + cls + " " + string.Join(" ", parts));
                    };
                    w.Start();
                    Watchers.Add(w);
                    Say(T("Vendor WMI event watched: ", "제조사 WMI 이벤트 감시: ") + n);
                }
                catch (Exception ex) { Say(T("Vendor WMI event not available: ", "제조사 WMI 이벤트 사용 불가: ") + n + " (" + ex.Message + ")"); }
            }
        }
        catch (Exception ex) { Say("WMI: " + ex.Message); }
    }

    static string FormatValue(object v)
    {
        if (v == null) return "null";
        var arr = v as Array;
        if (arr != null) return "[" + string.Join(",", arr.Cast<object>().Take(32)) + "]";
        if (v is uint || v is int) return string.Format("0x{0:X}", Convert.ToUInt32(v));
        return v.ToString();
    }

    // ───────────────────────── ASUS ATKACPI DSTS scan ─────────────────────────
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFile(string name, uint access, uint share, IntPtr sec, uint disp, uint flags, IntPtr tmpl);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle h, uint code, byte[] inBuf, int inSize, byte[] outBuf, int outSize, out int returned, IntPtr ov);
    const uint IOCTL_ATK = 0x0022240C, DSTS = 0x53545344;

    static SafeFileHandle OpenAtk()
    {
        var h = CreateFile(@"\\.\ATKACPI", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
        return h.IsInvalid ? null : h;
    }

    static uint? Dsts(SafeFileHandle h, uint id)
    {
        var inBuf = new byte[16];
        BitConverter.GetBytes(DSTS).CopyTo(inBuf, 0);
        BitConverter.GetBytes(8u).CopyTo(inBuf, 4);
        BitConverter.GetBytes(id).CopyTo(inBuf, 8);
        var outBuf = new byte[16];
        int got;
        if (!DeviceIoControl(h, IOCTL_ATK, inBuf, inBuf.Length, outBuf, outBuf.Length, out got, IntPtr.Zero) || got < 4) return null;
        return BitConverter.ToUInt32(outBuf, 0);
    }

    static IEnumerable<uint> AtkIds()
    {
        for (uint g = 0x0001; g <= 0x0013; g++)
            for (uint lo = 0; lo <= 0xFF; lo++)
                yield return (g << 16) | lo;
    }

    static Dictionary<uint, uint> Snapshot(SafeFileHandle h, IEnumerable<uint> ids)
    {
        var d = new Dictionary<uint, uint>();
        foreach (var id in ids) { var v = Dsts(h, id); if (v.HasValue) d[id] = v.Value; }
        return d;
    }

    static void Countdown(string what, int secs)
    {
        for (int i = secs; i > 0; i--) { Console.Write("\r  " + what + " " + i + "… "); Thread.Sleep(1000); }
        Console.WriteLine();
    }

    static void AsusScan()
    {
        var h = OpenAtk();
        if (h == null) { Say(T("ASUS ATKACPI: not present (not an ASUS, or the ASUS driver is not installed) — skipped", "ASUS ATKACPI: 없음 (ASUS가 아니거나 ASUS 드라이버 미설치) — 건너뜀")); return; }
        using (h)
        {
            Say("");
            Say(T("== ASUS ACPI scan (read-only DSTS) ==", "== ASUS ACPI 스캔 (읽기 전용 DSTS) =="));
            var ids = AtkIds().ToList();
            var sw = Stopwatch.StartNew();
            Snapshot(h, ids.Take(64));
            double per = sw.Elapsed.TotalMilliseconds / 64;
            Say(string.Format(T("  {0} ids, about {1:0.0} ms each", "  {0}개 ID, 하나당 약 {1:0.0} ms"), ids.Count, per));

            Console.WriteLine();
            Console.WriteLine(T("  Do NOT touch Fn. Press Enter to take the 'released' snapshots (3 s).", "  Fn을 누르지 마세요. Enter를 누르면 '뗀 상태'를 3초 동안 찍어요."));
            Console.ReadLine();
            var released = Snapshot(h, ids);
            var answering = released.Keys.ToList();
            // Values that drift on their own (fan speed, temperatures …) are not candidates.
            var noisy = new HashSet<uint>();
            for (int i = 0; i < 2; i++)
            {
                Thread.Sleep(1200);
                var again = Snapshot(h, answering);
                foreach (var id in answering) if (!again.ContainsKey(id) || again[id] != released[id]) noisy.Add(id);
            }
            Say(string.Format(T("  {0} values answer, {1} drift on their own (ignored)", "  응답하는 값 {0}개, 저절로 바뀌는 값 {1}개(제외)"), answering.Count, noisy.Count));
            var stable = answering.Where(id => !noisy.Contains(id)).ToList();

            Console.WriteLine(T("  Now press and HOLD Fn alone. Scanning starts in 3 s — keep holding until it says release.", "  이제 Fn만 누르고 계속 누르고 계세요. 3초 뒤 스캔해요 — '떼세요'가 나올 때까지 누르고 계세요."));
            Countdown(T("hold Fn", "Fn 누르고 있기"), 3);
            var held = Snapshot(h, stable);
            Console.WriteLine(T("  Release Fn.", "  Fn을 떼세요."));
            Thread.Sleep(1500);
            var cands = stable.Where(id => held.ContainsKey(id) && held[id] != released[id]).ToList();
            Say(string.Format(T("  changed while Fn was held: {0}", "  Fn을 누르는 동안 바뀐 값: {0}개"), cands.Count));
            foreach (var id in cands) Say(string.Format("    id 0x{0:X8}: released 0x{1:X8} -> held 0x{2:X8}", id, released[id], held[id]));
            if (cands.Count == 0) return;

            // Structured confirmation: the value must follow Fn in every window.
            phase = "confirm";
            var ok = cands.ToDictionary(id => id, id => true);
            for (int round = 1; round <= 2; round++)
            {
                foreach (var hold in new[] { true, false })
                {
                    Console.WriteLine(hold ? T("  HOLD Fn now", "  지금 Fn 누르고 있기") : T("  RELEASE Fn now", "  지금 Fn 떼기"));
                    Thread.Sleep(1200); // time to react
                    var end = DateTime.UtcNow.AddSeconds(1.5);
                    while (DateTime.UtcNow < end)
                    {
                        foreach (var id in cands)
                        {
                            var v = Dsts(h, id);
                            uint expect = hold ? held[id] : released[id];
                            if (!v.HasValue || v.Value != expect) ok[id] = false;
                        }
                        Thread.Sleep(50);
                    }
                }
            }
            Console.WriteLine(T("  Release Fn.", "  Fn을 떼세요."));
            foreach (var id in cands)
            {
                Say(string.Format(T("  id 0x{0:X8}: {1}", "  id 0x{0:X8}: {1}"), id, ok[id] ? T("FOLLOWS Fn ✓", "Fn을 따라 바뀜 ✓") : T("does not follow Fn", "Fn과 무관")));
                if (ok[id]) Event("ATK DSTS", string.Format("id 0x{0:X8} follows Fn: released 0x{1:X8}, held 0x{2:X8}", id, released[id], held[id]), "DSTS follows " + id);
            }
        }
    }

    // ───────────────────────── listening phases ─────────────────────────
    static void Listen(string name, string instruction, int secs)
    {
        Console.WriteLine();
        Say("== " + instruction + " ==");
        phase = name;
        Countdown(T("listening", "듣는 중"), secs);
    }

    [STAThread]
    static int Main(string[] args)
    {
        Console.OutputEncoding = Encoding.UTF8;
        Say("FnProbe — " + DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss"));
        using (var k = Registry.LocalMachine.OpenSubKey(@"HARDWARE\DESCRIPTION\System\BIOS"))
            if (k != null)
                foreach (var n in new[] { "SystemManufacturer", "SystemProductName", "SystemFamily", "SystemVersion", "BaseBoardProduct", "BIOSVersion" })
                    Say(string.Format("  {0}: {1}", n, k.GetValue(n)));
        if (SystemInformation.TerminalServerSession)
            Say(T("!! This is a Remote Desktop session: the Fn key of the keyboard you are typing on never reaches this PC. Run FnProbe at the laptop itself.",
                  "!! 원격 데스크톱 세션이에요. 지금 손으로 누르는 키보드의 Fn은 이 PC에 전달되지 않아요. 노트북 앞에서 직접 실행하세요."));
        Say("");

        // Listeners run on a UI thread with its own message loop.
        var ready = new ManualResetEvent(false);
        var ui = new Thread(() =>
        {
            hookProc = (c, w, l) =>
            {
                if (c >= 0)
                {
                    var kb = (KB)Marshal.PtrToStructure(l, typeof(KB));
                    int m = w.ToInt32();
                    Event("LL HOOK", string.Format("{0} vk=0x{1:X2} scan=0x{2:X2} flags=0x{3:X}", m == 0x100 || m == 0x104 ? "down" : "up  ", kb.vk, kb.scan, kb.flags), string.Format("KEY vk=0x{0:X2} make=0x{1:X2}", kb.vk, kb.scan));
                }
                return CallNextHookEx(IntPtr.Zero, c, w, l);
            };
            SetWindowsHookEx(13, hookProc, GetModuleHandle(null), 0);
            var sink = new Sink();
            RegisterRawInput(sink.Handle);
            ready.Set();
            Application.Run();
        });
        ui.SetApartmentState(ApartmentState.STA);
        ui.IsBackground = true;
        ui.Start();
        ready.WaitOne();
        WatchVendorWmi();

        Console.WriteLine();
        Console.WriteLine(T("Steps follow. Each listens for a few seconds; just do what it says.", "단계별로 몇 초씩 들어요. 안내대로만 해 주세요."));
        Console.WriteLine(T("Press Enter to start.", "Enter를 누르면 시작해요."));
        Console.ReadLine();

        Listen("control", T("1/4 Press the letter A twice (to check that keys are seen)", "1/4 글자 A를 두 번 누르세요 (키가 잡히는지 확인)"), 5);
        Listen("idle", T("2/4 Don't touch anything (background noise)", "2/4 아무것도 누르지 마세요 (잡음 확인)"), 5);
        Listen("fn", T("3/4 Press and HOLD Fn alone for ~2 s, three times", "3/4 Fn만 2초씩 꾹 누르기, 세 번"), 12);
        Listen("fn+f", T("4/4 Press Fn+F (or Fn+F5 etc.) once, for comparison", "4/4 비교용으로 Fn+F(또는 Fn+F5 등)를 한 번 누르세요"), 6);
        phase = "";

        AsusScan();
        foreach (var w in Watchers) try { w.Stop(); } catch { }

        // Verdict — computed from a snapshot taken after recording stopped (the listener thread keeps running).
        Dictionary<string, Dictionary<string, int>> snap;
        lock (Gate)
        {
            recording = false;
            snap = Hits.ToDictionary(kv => kv.Key, kv => new Dictionary<string, int>(kv.Value));
        }
        Func<string, Dictionary<string, int>> P = name => { Dictionary<string, int> d; return snap.TryGetValue(name, out d) ? d : new Dictionary<string, int>(); };
        var control = P("control"); var idle = P("idle"); var fn = P("fn"); var confirmHits = P("confirm");

        Say("");
        Say("== " + T("Summary", "요약") + " ==");
        foreach (var name in new[] { "control", "idle", "fn", "fn+f", "confirm" })
        {
            var d = P(name);
            Say(string.Format("  {0,-8} {1}", name, d.Count == 0 ? "—" : string.Join(", ", d.Select(kv => kv.Key + " ×" + kv.Value))));
        }
        // The input check needs the A key itself (vk 0x41), not just any traffic.
        bool controlOk = control.Keys.Any(k => k.StartsWith("KEY vk=0x41"));
        // An Fn signal is a source/payload that is new in the Fn phase (not in idle/control), or clearly busier
        // than in the idle baseline (Fn phase is 12 s, idle 5 s). HID signatures include the report bytes, so a
        // state bit that changes while Fn is held shows up as a new signature even at an unchanged report rate.
        var fnSignals = fn.Where(kv =>
        {
            int idleN; idle.TryGetValue(kv.Key, out idleN);
            bool inControl = control.ContainsKey(kv.Key);
            return (idleN == 0 && !inControl) || kv.Value > idleN * 12.0 / 5.0 * 3 + 2;
        }).Select(kv => kv.Key).ToList();
        bool dstsFollows = confirmHits.Keys.Any(k => k.StartsWith("DSTS follows"));

        if (!controlOk)
            Say(T("  ✗ The A key wasn't seen — input isn't reaching this session (Remote Desktop? an elevated window in front?). Run it again at the laptop.",
                  "  ✗ A 키가 잡히지 않았어요. 이 세션에 입력이 안 들어와요(원격 데스크톱? 관리자 권한 창?). 노트북 앞에서 다시 실행하세요."));
        else if (fnSignals.Count > 0 || dstsFollows)
        {
            Say(T("  ✓ Something follows the Fn key on its own. Please send this report:", "  ✓ Fn만 눌렀을 때 따라오는 신호가 있어요. 이 보고서를 보내 주세요:"));
            foreach (var sig in fnSignals) Say("      " + sig);
            if (dstsFollows) Say("      " + string.Join(", ", confirmHits.Keys.Where(k => k.StartsWith("DSTS follows"))));
        }
        else
            Say(T("  ✗ Nothing follows the Fn key on any channel Windows offers. This laptop handles Fn inside the keyboard controller.",
                  "  ✗ Fn만 눌렀을 때 Windows가 볼 수 있는 어떤 경로로도 신호가 오지 않아요. 이 노트북은 Fn을 키보드 컨트롤러 안에서 처리해요."));

        string desktop = Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory);
        string file = Path.Combine(desktop, "FnProbe-" + DateTime.Now.ToString("yyyyMMdd-HHmmss") + ".txt");
        File.WriteAllText(file, Report.ToString(), new UTF8Encoding(false));
        Console.WriteLine();
        Console.WriteLine(T("Report saved: ", "보고서 저장: ") + file);
        Console.WriteLine(T("Press Enter to exit.", "Enter를 누르면 끝나요."));
        Console.ReadLine();
        return 0;
    }
}
