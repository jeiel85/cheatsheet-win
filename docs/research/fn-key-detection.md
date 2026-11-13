> Research notes behind Fn Cheatsheet's design (2026-11-13). Section 2.3 was written for the first WPF prototype; the shipping app is Rust + Win32 and implements it in `src/trigger.rs` (rules, unit-tested) and `src/hook.rs` (hooks). Section 6 has measurements on real hardware.

# cheatsheet-win: technical research (Fn detection, trigger design, model ID, prior art)

Researched 2026-11-13. Labels used below:
- **[doc]**: official vendor or Microsoft documentation.
- **[src]**: read directly in source code. Linux kernel files are from `torvalds/linux` master, fetched on the research date; line numbers may drift.
- **[local]**: measured on the dev machine, an ASUS ExpertBook P1503CVA running Windows 11 in a non-elevated session.
- **[community]**: forum posts or issue threads.
- **UNVERIFIED**: not confirmed.

---

## 0. Bottom line

1. **On the target laptops, Fn pressed alone is not observable from Windows user mode.** No documented scancode, HID usage, or ACPI/WMI event reports a bare Fn press on ASUS or Lenovo. The only exception found is a Linux driver comment about one 2011 ThinkPad (E420), marked "TO BE VERIFIED" (see 1.2). Design on the assumption that Fn cannot be seen. **Confirmed by measurement on an ASUS ExpertBook P1503CVA: no signal on any channel (§6.1).**
2. **Many Fn+key combinations are observable**, through three different channels:
   - normal keyboard scancodes, which a `WH_KEYBOARD_LL` hook sees;
   - vendor ACPI/WMI events, which reach WMI or a vendor driver;
   - vendor HID reports on page 0xFF31, on ASUS ROG/TUF/some Zenbook USB or I2C keyboards.
   Which channel a given combination uses depends on the model.
3. **Reading the Fn-lock state:**
   - **Lenovo IdeaPad/Legion/Yoga (consumer BU):** readable from firmware. Linux uses `HALS` bit 10; on Windows it is the Lenovo `\\.\EnergyDrv` IOCTL, the same call Lenovo Legion Toolkit makes.
   - **ThinkPad:** firmware has `HKEY.GMKS` (0x603 = on, 0x602 = off), but no documented Windows user-mode API was found.
   - **ASUS:** on most models the lock state is kept by software (ASUS System Control Interface, the Linux driver, or G-Helper), not read back from firmware. On some platforms the hardware explicitly has no readback.
4. **Recommended trigger:** hold a modifier alone, using PowerToys' pattern:
   - start a timer on the first keydown;
   - cancel if any other key goes down;
   - cancel on key-up;
   - default about 900 ms;
   - never swallow the key.

   Default to **Ctrl held alone**. Avoid Win, because releasing it alone opens Start. Avoid Alt, because releasing it alone activates the menu bar. Avoid Right Alt and Right Ctrl, because on Korean layouts they are the Hangul and Hanja keys.

---

## 1. Can user-mode software see Fn held down?

### 1.1 General background
- Fn is a local modifier inside the keyboard microcontroller or EC. It changes which code other keys send and has no USB HID usage or PS/2 scancode of its own. [community/FAQ] https://jdebp.uk/FGA/fn-key-is-local.html
- Microsoft PowerToys Keyboard Manager docs say: "The Fn (function) key cannot be remapped (in most cases)." [doc] https://learn.microsoft.com/en-us/windows/powertoys/keyboard-manager
- PowerToys maintainers in an issue: the Fn key is "a hardware level key and we can't detect it". [community] https://github.com/microsoft/PowerToys/issues/32855
- AutoHotkey community: Fn does not appear in KeyHistory. The usual workaround is to bind whatever key a Fn combination produces, or to toggle Fn-lock so the F-key sends a plain F-code. [community] https://www.autohotkey.com/boards/viewtopic.php?p=398691 ; key list doc https://www.autohotkey.com/docs/v2/KeyList.htm
- Counter-example, not a target vendor: Framework laptops have open EC firmware that can be reprogrammed so Fn emits a scancode (E016). This shows the EC decides, not Windows. https://howett.net/posts/2021-12-framework-ec

### 1.2 Lenovo ThinkPad (`thinkpad_acpi`)
- **Hotkey path.** Fn combinations are reported by the ACPI `HKEY` device as `0x1xxx` event codes: 0x1001 = Fn+F1 through 0x100C = Fn+F12, plus Fn+Backspace/Insert/Delete/Home/End/PgUp/PgDn/Space and others. [doc] https://docs.kernel.org/admin-guide/laptops/thinkpad-acpi.html
- **Mask.** A hotkey mask decides whether firmware or the OS driver handles each key: "If a key is 'masked' (bit set to 0 in the mask), the firmware will handle it." Volume, mute, ThinkLight and (on IBM models) brightness-up are "always handled by the firmware, even when unmasked." The docs also say: "not all Fn key combinations are supported through ACPI depending on the ThinkPad model". [doc] same URL.
- **Fn alone.** The doc's "never propagated" list contains `0x6005: KEYBOARD: Fn key pressed (TO BE VERIFIED)`. In source, `TP_HKEY_EV_KEY_FN = 0x6005, /* Fn key pressed? E420 */`. The handler ignores it: "key press events, we just ignore them as long as the EC is still reporting them in the normal keyboard stream". [src] https://github.com/torvalds/linux/blob/master/drivers/platform/x86/lenovo/thinkpad_acpi.c (around L236 and L3875). So at most one older model (E420) is known to fire an ACPI event on Fn. There is no evidence for current ThinkPads, and **no Windows user-mode interface is documented** for HKEY events. UNVERIFIED whether any 2018+ ThinkPad raises 0x6005.
- **Fn+Esc (FnLock).** The event is `TP_HKEY_EV_KEY_FN_ESC = 0x6060` ("Fn+Esc key pressed X240"). The driver then calls the ACPI method `GMKS`, and the kernel patch says GMKS returns **0x603 when FnLock is on and 0x602 when off**; `SMKS` sets it. [src] same file around L237 and L3882; patch summary https://lore.kernel.org/all/20210315195823.23212-1-esteve.varela@gmail.com/T/ (lore returned 403 to direct fetch; the content was confirmed through search summary and the code).
- **Newer extended hotkeys.**
  - 0x1300–0x1319 range (2017+ models).
  - 0x131e: Fn+U and Fn+S, USB-C security.
  - 0x131f / 0x1401: Fn+F8 performance-mode toggle, 2024 / 2025+.
  - 0x131c: Fn+double-tap TrackPoint toggle.
  - `KEY_FN_RIGHT_SHIFT`, used by "Lenovo Quick Clean".

  [src] thinkpad_acpi.c L180–193, L3300–3345.
- **Polling fallback.** The driver can also poll "CMOS NVRAM" at least 10 times per second for some extra hotkeys on old models. [doc] kernel doc above. Not applicable to Windows user mode.
- **Fn combinations that become ordinary keys** (visible to a LL hook): Fn+B = Break, Fn+P = Pause, Fn+S = SysRq, Fn+K = ScrLk on 2017+ ThinkPads. On 6-row keyboards Lenovo also documents Fn+Ctrl+B and similar. [doc] https://support.lenovo.com/us/en/solutions/ht503647 (403 to automated fetch; content from search summary), https://support.lenovo.com/bo/ko/solutions/ht074004 . The exact scancode or VK each produces was not verified; UNVERIFIED, measure on hardware.
- **BIOS options that change Fn behavior** (ThinkPad, Config > Keyboard/Mouse): "Fn and Ctrl Key swap", "Fn Sticky key", "F1–F12 as Primary Function". [doc] https://support.lenovo.com/np/he/solutions/ht074187 ; ThinkPad manual tables (e.g. https://www.manualslib.com/manual/1029450/Thinkpad-E460.html?page=76).
  - These settings are readable through `Lenovo_BiosSetting` in `root\wmi`: "Lenovo BIOS Setup using WMI Deployment Guide", supported on all ThinkPads from 2018 or newer. [doc] https://support.lenovo.com/pa/nb/solutions/ht100612 and https://download.lenovo.com/pccbbs/thinkvantage_en/cdrt_resources/2020_supplement.pdf (`gwmi -class Lenovo_BiosSetting -namespace root\wmi`).
  - The **exact WMI setting names** (for example `FnCtrlKeySwap`, `FnSticky`, `F1F12asPrimaryFunction`) are **UNVERIFIED**. Enumerate `Lenovo_BiosSetting.CurrentSetting` on a target ThinkPad.
  - This gives the *default/BIOS* preference, not necessarily the live Fn+Esc state. Whether `Lenovo_BiosSetting` needs admin is UNVERIFIED.
- **Fn/Ctrl swap.** If a user enables this swap, the physical Fn key sends Ctrl, which a LL hook can see. Worth noting in the UI, but not something to rely on.

### 1.3 Lenovo IdeaPad / Yoga / Legion / ThinkBook (`ideapad-laptop`, VPC2004)
- **Fn-lock in firmware.** The ACPI method `HALS` holds `HALS_FNLOCK_SUPPORT_BIT = 9` and `HALS_FNLOCK_STATE_BIT = 10`. `SALS` sets it: `SALS_FNLOCK_ON = 0xe`, `SALS_FNLOCK_OFF = 0xf`. The Linux driver exposes this as the sysfs `fn_lock` attribute. [src] https://github.com/torvalds/linux/blob/master/drivers/platform/x86/lenovo/ideapad-laptop.c (L94–105, L735–750)
- **WMI hotkey events** (`IDEAPAD_WMI_KEY` | code):
  - 0x02 = "FnLock (handled by the firmware)", ignored by the driver;
  - 0x01 = customizable "star" key;
  - 0x10 = Fn+R refresh-rate toggle;
  - 0x3d = performance toggle (Fn+Q);
  - and others.

  WMI GUIDs: `26CAB2E5-5CF1-46AE-AAC3-4A12B6BA50E6` (Yoga 3), `56322276-8493-4CE8-A783-98C991274F5E` (Yoga 700), `8FC0DE0C-B4E4-43FD-B0F3-8871711C1294` (Legion 5, "fn_keys"). [src] same file L1317–1370, L2350–2353.
- **On Windows: Lenovo Legion Toolkit** (GPL-3.0, archived 2025-07):
  - **Fn-lock read/write** via `CreateFile("\\.\EnergyDrv")` + `DeviceIoControl(IOCTL_ENERGY_SETTINGS = 0x831020E8, in = 0x2)`. Lock is **bit 10** of the result, matching HALS bit 10. Set with 0xE / 0xF. [src] https://github.com/BartoszCichecki/LenovoLegionToolkit/blob/master/LenovoLegionToolkit.Lib/Features/FnLockFeature.cs , https://github.com/BartoszCichecki/LenovoLegionToolkit/blob/master/LenovoLegionToolkit.Lib/System/Drivers.cs
  - **Fn-key event stream:** WMI `root\WMI`, `SELECT * FROM LENOVO_UTILITY_EVENT`, property `PressTypeDataVal`. Values from LLT's `SpecialKey` enum: 1 = Fn+F9, **2 = FnLockOn, 3 = FnLockOff**, 4/45 = Fn+PrtSc, 12/13 = camera on/off, 16 or 0x0041002A = Fn+R, 42 = Fn+N, 62 = Fn+F4, 63 = Fn+F8, 24–38 and 64–66 = keyboard backlight levels. [src] https://github.com/BartoszCichecki/LenovoLegionToolkit/blob/master/LenovoLegionToolkit.Lib/System/Management/WMI.LenovoUtilityEvent.cs , https://github.com/BartoszCichecki/LenovoLegionToolkit/blob/master/LenovoLegionToolkit.Lib/Enums.cs (L461–488)
  - **Admin:** the LLT README says it "needs an account with Administrator rights". Whether `\\.\EnergyDrv` or `LENOVO_UTILITY_EVENT` *specifically* need elevation is UNVERIFIED. https://github.com/BartoszCichecki/LenovoLegionToolkit
- **Lenovo per-model HTML user guides** list Fn combinations. Example, Legion 5 16IRX9: Fn+B Break, Fn+P Pause, Fn+S SysRq, Fn+K ScrLk, Fn+Q mode, Fn+R refresh rate, Fn+N device info. [doc] https://download.lenovo.com/pccbbs/pubs/legion5_16irx9/ug/html_en/en/Dual-functionKeysWithoutPrintedIcons.html . IdeaPad "Hotkey mode" BIOS option: https://download.lenovo.com/pccbbs/pubs/ip1_14_15/html_en/EN/feature_intro_hotkey_mode.html

### 1.4 ASUS notebooks (ATK WMI: `asus-wmi` / `asus-nb-wmi`)
- **Channel.**
  - WMI management GUID `97845ED0-4E6D-11DE-8A39-0800200C9A66` (methods DSTS/DEVS).
  - Notify GUID `0B3CBB35-E3C2-45ED-91C2-4C5A6D195D1C`, carrying hotkey codes.

  [src] https://github.com/torvalds/linux/blob/master/include/linux/platform_data/x86/asus-wmi.h (L8), https://github.com/torvalds/linux/blob/master/drivers/platform/x86/asus-nb-wmi.c (L29)
- **Notify codes** (asus-nb-wmi keymap):
  - 0x10/0x20 brightness
  - 0x30/0x31/0x32 volume/mute
  - 0x33/0x35 screen lock
  - 0x38 Armoury Crate
  - 0x5C Fn+Space Power4Gear
  - 0x6B touchpad toggle
  - 0x7C mic mute
  - 0x86 MyASUS key
  - 0x88 radio
  - 0x8A color mode
  - 0x9D `KEY_FN_F`
  - 0xAE Fn+F5 fan mode
  - 0xC4/0xC5 keyboard backlight
  - 0xCA/0xCB noise-cancel (ExpertBook B9)
  - **0x2B PrintScreen and 0x2C CapsLock "also send via PS/2" on newer models**

  [src] asus-nb-wmi.c L608–690
- **Fn-lock.** `NOTIFY_FNLOCK_TOGGLE = 0x4e` is a *toggle notification*. The Linux driver keeps `fnlock_locked` itself, flips it, and pushes it with `DEVS(ASUS_WMI_DEVID_FNLOCK = 0x00100023, mode)`. Support check: `DSTS(0x00100023)` has `PRESENCE_BIT 0x00010000` set and `BIT(0)` ("BIOS disabled") clear. [src] https://github.com/torvalds/linux/blob/master/drivers/platform/x86/asus-wmi.c (L56–77, L4647–4720); asus-wmi.h L95, L173–175. **DSTS does not return the current lock state**, only support/presence.
- **No readback on some platforms.** A 2026 asus-wmi patch for ProArt P16 (N-Key keyboard 0B05:19B6): DEVS for FNLOCK is a no-op there, fn-lock must go through a HID feature report, and the `fn_lock` show path returns -EOPNOTSUPP because "the hardware provides no readback". [src/list] https://patchew.org/linux/20260430080527.12754-1-marcus@grenangen.se/
- **Fn-alone code:** none in the keymap.

### 1.5 ASUS USB/I2C-HID keyboards (`hid-asus`: ROG/TUF N-Key, Zenbook I2C, T100 etc.)
- **Vendor usage page** `HID_UP_ASUSVENDOR = 0xff310000`. [src] https://github.com/torvalds/linux/blob/master/include/linux/hid.h (L172)
- **Keyboards covered.** Device IDs (vendor 0x0B05): `I2C_KEYBOARD 0x8585`, `I2C_ZENBOOK_KEYBOARD 0x4b42`, ROG N-KEY `0x1866`, `0x19b6`, and others. [src] https://github.com/torvalds/linux/blob/master/drivers/hid/hid-ids.h (L214–238)
- **Report format.** Input report ID `0x5d`, feature/keyboard report `0x5a`.
- **Mapped vendor usages:**
  - 0x10/0x20 brightness
  - 0x4E Fn+Esc (`KEY_FN_ESC`)
  - 0x6b touchpad
  - 0x7c mic mute
  - 0x86 MyASUS
  - 0x88 RF kill
  - 0xb5 calc
  - 0xc4/0xc5/0xc7 keyboard backlight
  - 0x38 ROG key
  - 0xba Fn+C Splendid
  - 0x5c Fn+Space
  - 0x99/0xae Fn+F5 fan
  - 0x92 Fn+Ret calc
  - 0xb2/0xb3 Fn+Left/Right Aura
  - 0x6a ScreenPad
  - 0x4b arrows/PgUp toggle

  [src] https://github.com/torvalds/linux/blob/master/drivers/hid/hid-asus.c (L1287–1321)
- **Fn-lock on the HID path.** It is set with the feature report `{0x5a, 0xd0, 0x4e, enabled}`, and the driver tracks the state itself (`QUIRK_HID_FN_LOCK`). [src] hid-asus.c L412–423, L691–693
- **Filtered codes.** Codes 0xEA/0xEC/0x02/0x8A/0x9E are filtered as "Spurious HID codes sent by QUIRK_ROG_NKEY_KEYBOARD devices". [src] hid-asus.c L60–65, L545–551
- **Fn-alone code:** none found. UNVERIFIED by hardware capture.
- **On Windows: G-Helper** (GPL-3.0). It opens the ASUS HID collection that exposes report ID 0x5A (HidSharp) and reads input reports. `data[1]` is the key code, and 236 (0xEC) is ignored. [src] https://github.com/seerge/g-helper/blob/main/app/Input/KeyboardListener.cs , https://github.com/seerge/g-helper/blob/main/app/USB/AsusHid.cs (PIDs list L13)

### 1.6 ASUS on Windows: user-mode APIs (G-Helper source + local measurements)
- **`\\.\ATKACPI` driver interface** (ASUS System Control Interface): `DeviceIoControl(0x0022240C)`, input = `[methodId u32][argLen u32][devId u32][arg u32]`. Method IDs: DSTS 0x53545344, DEVS 0x53564544, INIT 0x54494E49. [src] https://github.com/seerge/g-helper/blob/main/app/AsusACPI.cs (L32–39, L153 `FnLock = 0x00100023`)
  - [local] As a **non-elevated** user, `CreateFile("\\.\ATKACPI")` succeeded. `DSTS(0x00100023)` returned **0x00010000** (presence bit only), which matches the Linux reading: Fn-lock supported, state not reported.
- **WMI** `root\wmi`: class `AsusAtkWmi_WMNB`, methods DSTS/DEVS/INIT/...; events `AsusAtkWmiEvent` (G-Helper subscribes with `SELECT * FROM AsusAtkWmiEvent`). [src] AsusACPI.cs L478, L981–990
  - [local] Enumerating `AsusAtkWmi_WMNB` instances **failed with Access denied when not elevated**, so the WMI method route needs admin.
  - [local] `Register-CimIndicationEvent ... AsusAtkWmiEvent` was **accepted** when not elevated. Whether events are actually delivered without elevation is UNVERIFIED (needs a key press test).
  - [community] G-Helper author's PowerShell example (run "as admin"): `Invoke-CimMethod (Get-CimInstance -Namespace root/wmi -ClassName AsusAtkWmi_WMNB) -MethodName DEVS -Arguments @{Device_ID=0x00100023; Control_status=1}` toggled Fn-lock on a Vivobook. https://github.com/seerge/g-helper/issues/422
- **Where ASUS software keeps the Fn-lock state on Windows** [local, P1503CVA]. Both items below are UNVERIFIED as the live state; to confirm, press Fn+Esc and re-read both.
  - Registry: `HKLM\SOFTWARE\ASUS\ASUS System Control Interface\AsusOptimization\ASUS Keyboard Hotkeys`, value `FnSwitch = 1` (also `FnSwitchPrompt`, `AutoLockKeybd`). Readable without admin.
  - File: `C:\ProgramData\ASUS\ASUS System Control Interface\AsusOptimization\Customization.ini`, section `[FunctionKeyLock] default=0 value=1`. A G-Helper user reports that in this file `FunctionKeyLock` value 0 equals `Control_status=0` equals hotkeys active. [community] https://github.com/seerge/g-helper/issues/422
  - Treat both as undocumented, vendor-private and liable to change.
- **Model differences in Fn-lock handling** [community]: G-Helper's author says "On Vivobooks Fn-lock is native and is handled by hardware itself (unlike all other models, where g-helper literally emulates it)". https://github.com/seerge/g-helper/issues/1581 . In G-Helper, `IsHardwareFnLock()` = `DeviceGet(FnLock) >= 0`. Otherwise G-Helper remaps F-keys in software. [src] https://github.com/seerge/g-helper/blob/main/app/Input/InputDispatcher.cs (L817–847)
- **ASUS official docs:**
  - Fn+Esc hotkey/F1–F12 switching "is not supported on all models"; check for an LED on the Fn key.
  - The mode is set in the "ASUS Keyboard Hotkeys" app or MyASUS (requires "ASUS System Control Interface V2" or later).
  - Using Fn+Esc changes the MyASUS setting synchronously.
  - "Hotkey feature may be different in different models, please refer to the User Manual."

  [doc] https://www.asus.com/us/support/FAQ/1038312/ ; the troubleshooting FAQ lists Fn+Esc as needing the ASUS System Control Interface driver: https://rog.asus.com/support/FAQ/1015071
- [local] **Dev box keyboard path:** the internal keyboard is `ACPI\ATK3001` "PC/AT Enhanced PS/2 keyboard" (i8042), so the EC handles Fn. There is no HID device on page 0xFF31. Other devices: ASUS System Control Interface v3 (`ACPI\ASUS2018`), I2C touchpad `ASUF1300`, and Intel HID Event Filter `INTC1078`.

### 1.7 Summary: which Fn combos a user-mode app can observe

| Channel | How to observe in user mode | Admin? | Examples |
|---|---|---|---|
| Ordinary scancode, EC translates Fn+key | `WH_KEYBOARD_LL` / Raw Input | No | Volume/mute on many models (ASUS hwdb maps scancodes 0xED/0xEE/0xEF to vol+/vol−/mute: https://github.com/systemd/systemd/blob/main/hwdb.d/60-keyboard.hwdb L296–300); ThinkPad/Legion Fn+B/P/S/K → Break/Pause/SysRq/ScrLk; ASUS PrtSc/CapsLock "also send via PS/2" |
| Consumer-page HID | `WH_KEYBOARD_LL` sees `VK_VOLUME_*`, `VK_MEDIA_*` | No | Media keys |
| ASUS ATK WMI notify | WMI `root\wmi:AsusAtkWmiEvent` | Delivery without admin UNVERIFIED | 0x4E Fn+Esc, 0x6B touchpad, 0x7C mic, 0x86 MyASUS, 0xC4/C5 keyboard backlight |
| ASUS vendor HID 0xFF31 (ROG/TUF/some Zenbook) | Open the HID collection, read report 0x5A | Probably not (G-Helper does it); UNVERIFIED | 0x4E Fn+Esc, 0xAE Fn+F5, 0x38 ROG key |
| Lenovo consumer WMI | `root\WMI:LENOVO_UTILITY_EVENT.PressTypeDataVal` | UNVERIFIED | 2/3 FnLock on/off, Fn+R, Fn+Q, Fn+N |
| ThinkPad ACPI HKEY | No documented Windows API (consumed by Lenovo hotkey software) | n/a | 0x1001–0x100C Fn+F1..F12, 0x6060 Fn+Esc |
| Brightness | Usually ACPI video, not keyboard; a LL hook usually won't see it (UNVERIFIED per model) | n/a | |

**Using "first Fn-combo event" as a fallback trigger.** This works only after the user presses a combination, and only for combinations routed through a channel above. On the PS/2 EC path, Fn+F-keys with a firmware-handled action (brightness, airplane, etc.) emit nothing to the keyboard stream. **Not recommended as the primary trigger.** It could serve as an optional "hint" mode.

---

## 2. Trigger design

### 2.1 Prior art
- **PowerToys Shortcut Guide** (MIT) [doc] https://learn.microsoft.com/en-us/windows/powertoys/shortcut-guide
  - Default activation: Win+Shift+?.
  - "Hold Windows key" options: Off / Show taskbar indicators (default) / Open Shortcut Guide.
  - "Hold duration (ms)": 100–5000, **default 900 ms**.
  - "Close Shortcut Guide when the Windows key is released" (on by default).
  - "Exclude apps" list.
- **PowerToys implementation** (central LL hook in the runner): https://github.com/microsoft/PowerToys/blob/main/src/runner/centralized_kb_hook.cpp (L95–150) [src]
  - On `WM_KEYDOWN/WM_SYSKEYDOWN`, if no key was pressed before, `SetTimer(duration)` for that VK.
  - If a *different* VK goes down, `KillTimer` for the tracked key. Autorepeat of the same VK does not cancel.
  - On key-up, `KillTimer`.
  - The event is always passed on with `CallNextHookEx`, never swallowed.
  - Keystrokes PowerToys injects are tagged in `dwExtraInfo` and ignored.
  - Clamp of 100–5000 ms and default 900: https://github.com/microsoft/PowerToys/blob/main/src/modules/ShortcutGuide/ShortcutGuideModuleInterface/dllmain.cpp (L196–199, L411–426).
- **macOS CheatSheet** (the model for this app): hold ⌘ "a bit longer", release to hide. Third-party write-ups put the delay at about 2 s and say it is adjustable. UNVERIFIED (official site returns 410). https://www.producthunt.com/p/media-atelier/cheatsheet-2 , https://www.igeeksblog.com/view-mac-apps-keyboard-shortcuts-with-cheatsheet/

### 2.2 Choosing the trigger key (Fn not available)
| Candidate | Side effect when released alone | Verdict |
|---|---|---|
| Win | Opens Start. Also conflicts with PowerToys' own Win-hold. AutoHotkey sends a "menu mask" key just to prevent this (https://www.autohotkey.com/docs/v2/lib/_MenuMaskKey.htm). | Avoid |
| Alt | Activates the menu bar of the focused window (same AHK doc) | Avoid |
| Right Alt / Right Ctrl | On Korean 104-key layouts these are Hangul and Hanja (`VK_HANGUL 0x15`, `VK_HANJA 0x19`) and switch IME mode. https://learn.microsoft.com/globalization/input/korean-ime , https://github.com/microsoft/wslg/issues/98 , https://learn.microsoft.com/windows/win32/inputdev/virtual-key-codes | Avoid |
| Left Ctrl (or either Ctrl) | None in general. Possible interactions: Windows "show pointer location when I press Ctrl" and PowerToys Find My Mouse (double-press Left Ctrl); UNVERIFIED whether a *long* hold triggers either. | **Recommended default** |
| Shift | Nothing in most apps, but Sticky Keys prompt after 5 taps; and Shift is the IME toggle on some layouts | Second choice |

**Recommendation.** Default to **hold Ctrl alone for 900 ms**, matching PowerToys. Make the key and duration configurable, with a range of 300–3000 ms. Optionally add a "double-tap Ctrl then hold" mode for users who rest on Ctrl.

### 2.3 Hook algorithm (`WH_KEYBOARD_LL`, .NET Framework 4.8 / WPF)
1. On keydown of the trigger key, when no key is down and nothing is armed: arm the timer at T0. Ignore autorepeat of the same key.
2. On keydown of **any other** key, including a second modifier, or on a mouse button (needs `WH_MOUSE_LL` if you care): disarm, and mark the trigger "used as modifier" until it is released.
3. When the timer fires and the trigger is still down: show the overlay. Do not steal focus (`WS_EX_NOACTIVATE`, topmost).
4. On key-up of the trigger: hide the overlay and disarm.
5. If the overlay is showing and another key goes down: hide it. The user is now using a real shortcut.
6. **Never return non-zero.** Always `CallNextHookEx`, so the app never steals shortcuts. Skip events with `LLKHF_INJECTED` if you inject anything.
7. Use the `KBDLLHOOKSTRUCT` data. `GetAsyncKeyState` is not updated yet inside the callback: "the callback function is called before the asynchronous state of the key is updated". [doc] https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc
8. Keep the callback trivial. Post to the dispatcher with `BeginInvoke` and return. Do no WPF, WMI or IO work inside the hook (see the timeout below).

### 2.4 `WH_KEYBOARD_LL` facts and pitfalls
- **No admin needed and no DLL injection.** "the WH_KEYBOARD_LL hook is not injected into another process. Instead, the context switches back to the process that installed the hook". It is global-only, and "the thread that installed the hook must have a message loop" (the WPF UI thread qualifies). [doc] https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc ; scope table https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowshookexw
- **.NET.** Keep the delegate rooted (static field), otherwise the GC collects it and the app crashes. [doc] SetWindowsHookExW remarks (same URL).
- **LowLevelHooksTimeout.** Value at `HKCU\Control Panel\Desktop\LowLevelHooksTimeout` (ms). "on Windows 7 and later, the hook is silently removed without being called. There is no way for the application to know whether the hook is removed." Windows 10 1709+ caps the timeout at 1000 ms. [doc] LowLevelKeyboardProc.
  - **Mitigation:** return fast, keep GC pauses off the hook thread, and optionally run the hook on a dedicated STA thread with its own message loop (Microsoft's note: "run the hooks on a dedicated thread that passes the work off to a worker thread").
  - Add a watchdog: if no hook callbacks arrive while `GetLastInputInfo` shows keyboard activity, re-install the hook. This heuristic is our own suggestion.
- **UIPI and elevated windows.** UIPI "prevents lower-privilege applications from sending window messages or installing hooks in higher-privilege processes". A process launched with UIAccess "Has read input for all integrity levels using low-level hooks, raw input, GetKeyState…", which implies a normal medium-integrity process does not. [doc] https://learn.microsoft.com/en-us/previous-versions/dotnet/articles/bb625963(v=msdn.10) . Corroborated by PowerToys: remapping "won't work on an app or window if that window is running in administrator (elevated) mode and PowerToys is not running as administrator". [doc] keyboard-manager URL above.
  - **Consequence:** the overlay will not trigger while an elevated window (Task Manager, admin console, installers) has focus, unless the app runs elevated or as signed UIAccess from Program Files. It also never fires on the secure desktop (UAC prompt, Ctrl+Alt+Del, lock screen).
- **Raw Input alternative.** Microsoft recommends Raw Input over LL hooks for monitoring ("raw input can asynchronously monitor ... more effectively"). `RIDEV_INPUTSINK` delivers in the background, and Raw Input cannot time out. It cannot block keys, which is fine here because we never block. It also tells you which device a key came from (internal vs external keyboard), which is useful for "show the laptop map only when the internal keyboard is used". [doc] LowLevelKeyboardProc note; https://learn.microsoft.com/windows/win32/inputdev/raw-input . UNVERIFIED for this app whether Raw Input is also blocked by UIPI for elevated foreground windows; the integrity doc lists raw input among UIAccess privileges, so assume yes.

---

## 3. Model identification

### 3.1 Where Windows exposes SMBIOS
| SMBIOS field | WMI | Registry `HKLM\HARDWARE\DESCRIPTION\System\BIOS` |
|---|---|---|
| Type 1 Manufacturer | `Win32_ComputerSystem.Manufacturer` (MappingStrings "SMBIOS\|Type 1\|System Information\|Manufacturer") | `SystemManufacturer` |
| Type 1 Product Name | `Win32_ComputerSystem.Model` ("SMBIOS\|Type 1\|...\|Product Name"), also `Win32_ComputerSystemProduct.Name` | `SystemProductName` |
| Type 1 Version | `Win32_ComputerSystemProduct.Version` | `SystemVersion` |
| Type 1 Family | `Win32_ComputerSystem.SystemFamily` (Win10+) | `SystemFamily` |
| Type 1 SKU Number | `Win32_ComputerSystem.SystemSKUNumber` (Win10+) | `SystemSKU` |
| Type 2 Baseboard Product | `Win32_BaseBoard.Product` | `BaseBoardProduct` |
| Type 0 BIOS Version | `Win32_BIOS.SMBIOSBIOSVersion` | `BIOSVersion` |

Sources:
- WMI mapping strings: [doc] https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-computersystem
- Microsoft's sample script uses `Win32_BaseBoard.Product`, `Win32_ComputerSystem.SystemFamily` and `.SystemSKUNumber`: [doc] https://learn.microsoft.com/en-us/windows-hardware/drivers/bringup/sample-powershell-script-to-query-smbios-locally
- Registry value names: [local] read on P1503CVA. No official Microsoft page listing these registry values was found, so treat them as observed and stable in practice, but undocumented.
- Recommendation: read the registry first (fast, no WMI start-up cost, no admin) and fall back to WMI.

**Microsoft's intended semantics** ([doc] https://learn.microsoft.com/en-us/windows-hardware/drivers/bringup/smbios):
- **Family** = sub-brand / product line.
- **Product Name** = specific model; the recommendation is to include "the full value of the Family field followed by a single space and then the model name".
- **Baseboard Product** "must change when the baseboard in the device model changes".
- **SKU** = configuration, format OEM-defined.

CHIDs (hashes of these fields, used by Windows Update targeting) and their field tables: [doc] https://learn.microsoft.com/en-us/windows-hardware/drivers/install/computer-hardware-ids

### 3.2 Lenovo convention
- **Lenovo stores the friendly name in Type 1 *Version*.** Win32_ComputerSystem.Model / Product Name holds the machine-type code. [community/doc-archive] Microsoft blog: "Lenovo stores its model in a different class, called Win32_ComputerSystemProduct and the name for the field is 'Version'". https://learn.microsoft.com/en-us/archive/blogs/alex_semi/computer-make-model-with-lenovo-real-models ; Lenovo CDRT blog post (404 to direct fetch; content via search) https://thinkdeploy.blogspot.com/2020/03/lenovo-model-information-in-mdt.html . No current official Lenovo doc page was retrieved; treat this as well-established but not formally cited.
- **Caveat:** some machines return `Version = "Lenovo Computer"` or a wrong string after a bad BIOS flash. https://www.windows-noob.com/forums/topic/8291-wmi-query-returns-lenovo-product-as-thinkcentres-model/
- **Real dmidecode dumps** (linuxhw/DMI, CC-BY-4.0):

| Line | Product Name (=`Model`) | Version (=`CSProduct.Version`) | Family | SKU | Baseboard Product | Source |
|---|---|---|---|---|---|---|
| ThinkPad | `21JKCTO1WW` | `ThinkPad E14 Gen 5` | `ThinkPad E14 Gen 5` | `LENOVO_MT_21JK_BU_Think_FM_ThinkPad E14 Gen 5` | `21JKCTO1WW` | https://github.com/linuxhw/DMI/blob/master/Notebook/Lenovo/ThinkPad/ThinkPad%20E14%20Gen%205%2021JKCTO1WW/BBFF5AF426A5 |
| ThinkBook | `21KH` | `ThinkBook 16 G6 IRL` | same | `LENOVO_MT_21KH_BU_idea_FM_ThinkBook 16 G6 IRL` | `LNVNB161216` | https://github.com/linuxhw/DMI/blob/master/Notebook/Lenovo/ThinkBook/ThinkBook%2016%20G6%20IRL%2021KH/02673ACC585F |
| Legion | `82JU` | `Legion 5 15ACH6H` | same | `LENOVO_MT_82JU_BU_idea_FM_Legion 5 15ACH6H` | `LNVNB161216` | https://github.com/linuxhw/DMI/blob/master/Notebook/Lenovo/Legion/Legion%205%2015ACH6H%2082JU/0BA70260E7C5 |
| Yoga | `83CV` | `Yoga Slim 7 14IMH9` | same | `LENOVO_MT_83CV_BU_idea_FM_Yoga Slim 7 14IMH9` | `LNVNB161216` | https://github.com/linuxhw/DMI/blob/master/Notebook/Lenovo/Yoga/Yoga%20Slim%207%2014IMH9%2083CV/100FC043DF60 |
| IdeaPad | `82XD` | `IdeaPad Slim 5 14IRL8` | same | `LENOVO_MT_82XD_BU_idea_FM_IdeaPad Slim 5 14IRL8` | `LNVNB161216` | https://github.com/linuxhw/DMI/blob/master/Notebook/Lenovo/IdeaPad/IdeaPad%20Slim%205%2014IRL8%2082XD/00DD4705218D |

Patterns observed (n=5 dumps; generalization UNVERIFIED but consistent with the kernel and hwdb matching below):
- ThinkPad Product Name is the full 10-character MTM (`21JKCTO1WW`). Consumer lines (IdeaPad/Legion/Yoga/ThinkBook) use the bare 4-character machine type (`82JU`). Use the first 4 characters as the MT key.
- **Version** is the friendly name and the best key for a Fn-map lookup.
- **SKU** `LENOVO_MT_<MT>_BU_<think|idea>_FM_<friendly>` encodes MT, business unit and friendly name, which makes it a robust parse target.
- Consumer **Baseboard Product** `LNVNB161216` is generic and useless for identification.
- The Linux kernel and systemd match Lenovo the same way: `DMI_MATCH(DMI_PRODUCT_NAME, "82L5")`, `DMI_MATCH(DMI_PRODUCT_VERSION, "IdeaPad 5 15ITL05")`, and in systemd hwdb `svnLENOVO:pn21KH*:pvrThinkBook16G6IRL`. [src] ideapad-laptop.c L1122–1137; 60-keyboard.hwdb L1186–1198.

### 3.3 ASUS convention
- **Product Name pattern:** `<Family/brand> <SKU-model>_<variant model>`. The part after `_` is usually the same model, or a regional or OEM variant such as `Q410VA`, `A1504VA` or `FA577NU`. **Baseboard Product** = the bare model code. **Family** = brand line. Real data:

| Line | Product Name (=`Model`) | Family | Baseboard Product | Source |
|---|---|---|---|---|
| ExpertBook ([local]) | `ASUS EXPERTBOOK P1503CVA` | `ASUS EXPERTBOOK` | `P1503CVA` | this machine; BIOS `P1503CVA.319` |
| ExpertBook | `ExpertBook B9450FAV` | `ExpertBook` | `B9450FAV` | https://github.com/linuxhw/DMI/blob/master/Notebook/ASUSTek%20Computer/ExpertBook/ExpertBook%20B9450FAV/895651CFD6AE |
| Zenbook | `Zenbook UX3404VA_UX3404VA` | `Zenbook` | `UX3404VA` | https://github.com/linuxhw/DMI/blob/master/Notebook/ASUSTek%20Computer/Zenbook/Zenbook%20UX3404VA_UX3404VA/350C9CF3BF0B |
| Vivobook | `Vivobook_ASUSLaptop X1504VA_X1504VA` | `Vivobook` | `X1504VA` | https://github.com/linuxhw/DMI/blob/master/Notebook/ASUSTek%20Computer/VivoBook_ASUSLaptop/VivoBook_ASUSLaptop%20X1504VA_X1504VA/033884308130 |
| ROG | `ROG Zephyrus G14 GA403UV_GA403UV` | `ROG Zephyrus G14` | `GA403UV` | https://github.com/linuxhw/DMI/blob/master/Notebook/ASUSTek%20Computer/ROG/ROG%20Zephyrus%20G14%20GA403UV_GA403UV/73F1558E342F |
| TUF (dmesg) | `ASUS TUF Gaming A15 FA507NU_FA577NU` | – | `FA507NU` | `DMI: ASUSTeK COMPUTER INC. ASUS TUF Gaming A15 FA507NU_FA577NU/FA507NU, BIOS FA507NU.316` https://bugzilla.kernel.org/attachment.cgi?id=307599 |
| TUF (dmesg) | `ASUS TUF Gaming A16 FA617NS_FA617NS` | – | `FA617NS` | https://bugzilla.kernel.org/attachment.cgi?id=304154 |

- **Other variants seen** in the linuxhw folder listing:
  - `Zenbook UX3404VA_Q410VA`
  - `Zenbook UM3402YAR_UM3402YA`
  - `VivoBook_ASUSLaptop X1504ZA_F1504ZA`
  - `ROG Strix G18 G834JZR_G814JZR`
  - older `ZenBook` (capital B) and `VivoBook` spellings

  https://github.com/linuxhw/DMI/tree/master/Notebook/ASUSTek%20Computer
- **The BIOS version string** is `<board>.<rev>` (`GA403UV.303`, `X1504VA.302`), another source of the model code.
- **Recommended ASUS key:** `Win32_BaseBoard.Product` / `BaseBoardProduct` (e.g. `UX3405MA`, `GA403UV`). Then strip the trailing config letter(s) to reach the series (UX3405 → "Zenbook 14 UX3405", the series ASUS uses on its spec pages, e.g. https://www.asus.com/laptops/for-home/zenbook/asus-zenbook-14-oled-ux3405/techspec/). Fall back to parsing Product Name.
- **Matching style:** the kernel uses substring matches like `DMI_MATCH(DMI_PRODUCT_NAME, "ROG Flow Z13")`, `"UX8406MA"`, and `DMI_MATCH(DMI_BOARD_NAME, "RC71L")`. [src] asus-nb-wmi.c L548–584, asus-wmi.c L160–165. Use prefix or substring rules, not exact strings.
- UNVERIFIED: the exact strings for UX3405MA (the user's example `ZenBook UX3405MA_UX3405MA`). No public dump was found in this pass; the pattern above predicts `Zenbook UX3405MA_UX3405MA` with Family `Zenbook`. Capitalization changed from `ZenBook` to `Zenbook` around 2022 per the linuxhw folders, so match case-insensitively.

---

## 4. Existing open data and code (credit / licensing)

There is **no open dataset of per-model Fn-key legends** (what is printed on F1–F12 and what Fn+X does) for ASUS or Lenovo. Closest material:

| Project | What it has | License | Use |
|---|---|---|---|
| systemd `hwdb.d/60-keyboard.hwdb` | Per-DMI scancode → keycode maps for laptop hotkeys, with comments like "Fn+F4 mic mute", "Fn+F8 launch emoji keyboard" | LGPL-2.1-or-later (systemd default; file has no SPDX override) https://github.com/systemd/systemd/blob/main/LICENSES/README.md | DMI-matching syntax and scancode facts; cite if derived |
| Linux kernel `thinkpad_acpi`, `ideapad-laptop`, `asus-wmi`, `asus-nb-wmi`, `hid-asus`, `hid-lenovo` | Event-code ↔ function tables, Fn-lock mechanics, DMI quirk lists | GPL-2.0 | Facts are fine to use; don't copy code into a non-GPL app |
| G-Helper (seerge/g-helper) | Windows C#: ATKACPI IOCTL, `AsusAtkWmiEvent`, ASUS HID 0x5A listener, Fn-key action map, Fn-lock emulation | GPL-3.0 | Best Windows reference for ASUS; credit; GPL means no code copy into a closed app |
| Lenovo Legion Toolkit | Windows C#: `\\.\EnergyDrv` Fn-lock, `LENOVO_UTILITY_EVENT` key codes | GPL-3.0, archived 2025-07 | Same as above |
| asusctl / asus-linux | Linux ROG tooling | MPL-2.0 (https://gitlab.com/asus-linux/asusctl) | Reference |
| PowerToys | LL-hook hold-to-show implementation | MIT | Pattern (2.1); code reuse allowed with notice |
| linuxhw/DMI | Thousands of real dmidecode dumps by vendor/model | CC-BY-4.0 (https://github.com/linuxhw/DMI) | Build or validate model-matching rules; attribution required |
| Vendor manuals (Lenovo HTML user guides, ASUS e-manuals / FAQ 1038312) | The authoritative per-model Fn legend tables | Vendor copyright | Primary source for map data; enter facts by hand, link the source, don't copy text or images |

---

## 5. Suggested verification on real hardware (see §6 for what was measured)

1. Run a tiny LL-hook + Raw Input logger. Press Fn alone, Fn+Esc, and Fn+each F-key on each test model, with Fn-lock on and off. Record VK, scancode, flags, and which device (Raw Input `hDevice`).
2. ASUS: subscribe to `AsusAtkWmiEvent` non-elevated and confirm events arrive. Toggle Fn+Esc and re-read `FnSwitch` and `Customization.ini [FunctionKeyLock]` to confirm their meaning.
3. Lenovo consumer: try `\\.\EnergyDrv` IOCTL 0x831020E8 and `LENOVO_UTILITY_EVENT` without elevation.
4. ThinkPad: dump `Lenovo_BiosSetting` to get the exact Fn-related setting names, and check whether reading them needs admin.

---

## 6. Measured on real hardware

Measured with [FnProbe](../../tools/fn-probe/FnProbe.cs), the read-only diagnostic that ships with each release. It listens on every channel a user-mode Windows app can reach while the user follows prompts: press A (control), touch nothing (idle baseline), hold Fn alone three times, press an Fn combination once. On ASUS it also compares the ATKACPI `DSTS` status values with Fn held and released, then repeats hold/release twice to confirm any candidate. A signal counts only if it is new in the Fn phase or clearly above the idle baseline.

### 6.1 ASUS ExpertBook P1503CVA — no Fn signal on any channel (2026-11-13)

| | |
|---|---|
| Model | ASUS ExpertBook P1 15" P1503CVA, BIOS P1503CVA.319 |
| OS | Windows 11 build 26300 (26H2), non-elevated |
| Keyboard | `ACPI\ATK3001` (PS/2-type, i8042) |
| Other HID | touchpad `ASUF1300` (digitizer 0x0D/0x05, vendor 0xFF00/0x01, 0xFF00/0xC7); Intel `INTC816` (0xFFB3/0x10) |
| Tool | FnProbe v0.1.4, **local console session** at the laptop |
| Raw report | [measurements/asus-expertbook-p1503cva-2026-11-13.txt](measurements/asus-expertbook-p1503cva-2026-11-13.txt) |

| Channel | Control (A key) | Fn held alone (3 × 2 s) | Fn + F |
|---|---|---|---|
| `WH_KEYBOARD_LL` hook | ✅ vk 0x41, scan 0x1E | **nothing** | — |
| Raw Input, keyboard `ATK3001` | ✅ make 0x1E | **nothing** | — |
| Raw Input, vendor HID pages (0xFF00, 0xFFB3) | registered | **nothing** | — |
| ASUS WMI `AsusAtkWmiEvent` | — | **nothing** | ✅ `EventID=0x9D` |
| ATKACPI `DSTS`, 53 answering IDs | answers | **no value changed** | — |

Conclusion: on this model the Fn key is handled entirely inside the keyboard controller. Fn alone produces no scancode, no HID report, no WMI event and no ACPI state change that Windows software can see. Only the *result* of an Fn combination reaches Windows: Fn+F emits ASUS WMI event 0x9D (fan profile). **An app therefore cannot use "hold Fn" as a trigger on this laptop.** Fn Cheatsheet uses Left Ctrl (hold or double tap) instead, because it sits right next to Fn.

Notes from the measurement runs:
- `DSTS` id `0x00110013` (CPU fan) changes by itself every second or so. FnProbe excludes it as drift, and the hold/release confirmation rejects it when it changes by chance during a hold.
- Runs inside a **Remote Desktop** session are only partly valid. The keyboard hook and Raw Input see the RDP client's keyboard, not the laptop's. WMI events and ACPI values are hardware-wide and still count; Fn+F on the laptop keyboard produced the same `0x9D` event there. Since v0.1.4, FnProbe says so in its verdict.
- FnProbe v0.1.3 had a bug: the window receiving `WM_INPUT` was garbage-collected, so its Raw Input channel recorded nothing. Reports from v0.1.3 do not cover the HID channel. The run above used the fixed v0.1.4.

### 6.2 Lenovo — pending

A Lenovo run is planned with the same procedure. Lenovo routes hotkeys differently (`thinkpad_acpi` HKEY events, `LENOVO_UTILITY_EVENT`, `\.\EnergyDrv`), so the result may differ from ASUS.
