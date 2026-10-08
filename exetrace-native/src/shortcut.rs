//! 桌面快捷方式：创建 / 读回（COM：`IShellLinkW` + `IPersistFile`，手写 vtable）。
//!
//! 为什么手写 vtable：windows-sys 只导出函数/常量/结构，不导出 COM 接口定义；
//! 而 ShellLink 的方法表是稳定 ABI，按索引调用即可（Python 版走的也是这条路子）。
//!
//! vtable 索引（**已与 Python 版 shortcut.py 对账**，改动前对照 shobjidl_core.h）：
//!   `IShellLinkW : IUnknown` —— 3 GetPath, 7 SetDescription, 8 GetWorkingDirectory,
//!   9 SetWorkingDirectory, 17 SetIconLocation, 20 SetPath
//!   `IPersistFile : IPersist` —— 5 Load, 6 Save

use std::ffi::c_void;

use windows_sys::core::GUID;
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};

use crate::winx;

// CLSID_ShellLink {00021401-0000-0000-C000-000000000046}
const CLSID_SHELL_LINK: GUID = GUID {
    data1: 0x0002_1401,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};
// IID_IShellLinkW {000214F9-0000-0000-C000-000000000046}
const IID_ISHELL_LINK_W: GUID = GUID {
    data1: 0x0002_14F9,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};
// IID_IPersistFile {0000010B-0000-0000-C000-000000000046}
const IID_IPERSIST_FILE: GUID = GUID {
    data1: 0x0000_010B,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};

const VT_QUERY_INTERFACE: usize = 0;
const VT_RELEASE: usize = 2;
// IShellLinkW 继承的是 IUnknown（**不是** IPersist；IPersistFile 需 QI 另取），
// 所以 GetPath 从索引 3 开始。以下与 Python 版 shortcut.py 完全一致：
//   3 GetPath / 7 SetDescription / 8 GetWorkingDirectory / 9 SetWorkingDirectory /
//   17 SetIconLocation / 20 SetPath；IPersistFile: 5 Load / 6 Save
const VT_GET_PATH: usize = 3;
const VT_GET_WORKING_DIRECTORY: usize = 8;
const VT_SET_DESCRIPTION: usize = 7;
const VT_SET_ICON_LOCATION: usize = 17;
const VT_SET_PATH: usize = 20;
const VT_SET_WORKING_DIRECTORY: usize = 9;
const VT_PF_LOAD: usize = 5;
const VT_PF_SAVE: usize = 6;

type QueryInterfaceFn = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32;
type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
type SetPathFn = unsafe extern "system" fn(*mut c_void, *const u16) -> i32;
type SetDescriptionFn = unsafe extern "system" fn(*mut c_void, *const u16) -> i32;
type SetIconLocationFn = unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32;
type SetWorkingDirectoryFn = unsafe extern "system" fn(*mut c_void, *const u16) -> i32;
type GetPathFn = unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut c_void, u32) -> i32;
type GetWorkingDirectoryFn = unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32;
type PersistSaveFn = unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32;
type PersistLoadFn = unsafe extern "system" fn(*mut c_void, *const u16, u32) -> i32;

unsafe fn method(ptr: *mut c_void, index: usize) -> *const c_void {
    let vtbl = *(ptr as *const *const *const c_void);
    *vtbl.add(index)
}

unsafe fn release(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    let f: ReleaseFn = std::mem::transmute(method(ptr, VT_RELEASE));
    f(ptr);
}

unsafe fn query_interface(ptr: *mut c_void, iid: &GUID) -> Option<*mut c_void> {
    let mut out: *mut c_void = std::ptr::null_mut();
    let f: QueryInterfaceFn = std::mem::transmute(method(ptr, VT_QUERY_INTERFACE));
    let hr = f(ptr, iid, &mut out);
    if hr < 0 || out.is_null() {
        None
    } else {
        Some(out)
    }
}

/// 确保当前线程 COM 已初始化（幂等；`RPC_E_CHANGED_MODE` 也视为可用）。
pub fn com_initialize() -> bool {
    unsafe {
        let hr = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        hr >= 0 || hr as u32 == 0x8001_0106
    }
}

pub struct ShortcutInfo {
    pub target: String,
    pub workdir: String,
}

/// 创建 .lnk（调用方负责唯一化路径，绝不覆盖已有文件）。
pub fn create_shortcut(
    lnk_path: &str,
    target: &str,
    workdir: &str,
    icon: &str,
    description: &str,
) -> Result<(), String> {
    com_initialize();
    unsafe {
        let mut psl: *mut c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_SHELL_LINK,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ISHELL_LINK_W,
            &mut psl,
        );
        if hr < 0 || psl.is_null() {
            return Err(format!("CoCreateInstance(ShellLink) 失败 hr=0x{hr:08X}"));
        }

        let target_w = winx::wide(target);
        let workdir_w = winx::wide(workdir);
        let icon_w = winx::wide(icon);
        let desc_w = winx::wide(description);
        let lnk_w = winx::wide(lnk_path);

        let set_path: SetPathFn = std::mem::transmute(method(psl, VT_SET_PATH));
        let hr = set_path(psl, target_w.as_ptr());
        if hr < 0 {
            release(psl);
            return Err(format!("IShellLinkW::SetPath 失败 hr=0x{hr:08X}"));
        }
        if !workdir.is_empty() {
            let f: SetWorkingDirectoryFn =
                std::mem::transmute(method(psl, VT_SET_WORKING_DIRECTORY));
            f(psl, workdir_w.as_ptr());
        }
        if !icon.is_empty() {
            let f: SetIconLocationFn = std::mem::transmute(method(psl, VT_SET_ICON_LOCATION));
            f(psl, icon_w.as_ptr(), 0);
        }
        if !description.is_empty() {
            let f: SetDescriptionFn = std::mem::transmute(method(psl, VT_SET_DESCRIPTION));
            f(psl, desc_w.as_ptr());
        }

        let Some(ppf) = query_interface(psl, &IID_IPERSIST_FILE) else {
            release(psl);
            return Err("QueryInterface(IPersistFile) 失败".into());
        };
        let save: PersistSaveFn = std::mem::transmute(method(ppf, VT_PF_SAVE));
        let hr = save(ppf, lnk_w.as_ptr(), 1);
        release(ppf);
        release(psl);
        if hr < 0 {
            return Err(format!("IPersistFile::Save 失败 hr=0x{hr:08X}"));
        }
        Ok(())
    }
}

/// 读回 .lnk 的目标与起始位置（创建后自校验用）。
pub fn read_shortcut(lnk_path: &str) -> Result<ShortcutInfo, String> {
    com_initialize();
    unsafe {
        let mut psl: *mut c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_SHELL_LINK,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ISHELL_LINK_W,
            &mut psl,
        );
        if hr < 0 || psl.is_null() {
            return Err(format!("CoCreateInstance 失败 hr=0x{hr:08X}"));
        }
        let Some(ppf) = query_interface(psl, &IID_IPERSIST_FILE) else {
            release(psl);
            return Err("QueryInterface(IPersistFile) 失败".into());
        };
        let lnk_w = winx::wide(lnk_path);
        let load: PersistLoadFn = std::mem::transmute(method(ppf, VT_PF_LOAD));
        let hr = load(ppf, lnk_w.as_ptr(), 0 /* STGM_READ */);
        release(ppf);
        if hr < 0 {
            release(psl);
            return Err(format!("IPersistFile::Load 失败 hr=0x{hr:08X}"));
        }

        let mut buf = [0u16; 1024];
        let get_path: GetPathFn = std::mem::transmute(method(psl, VT_GET_PATH));
        // 第 4 参数 WIN32_FIND_DATAW* 传 NULL；SLGP_RAWPATH(4) = 返回原始路径
        let hr = get_path(psl, buf.as_mut_ptr(), buf.len() as i32, std::ptr::null_mut(), 4);
        if hr < 0 {
            release(psl);
            return Err(format!("IShellLinkW::GetPath 失败 hr=0x{hr:08X}"));
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let target = String::from_utf16_lossy(&buf[..n]);

        let mut wbuf = [0u16; 1024];
        let get_wd: GetWorkingDirectoryFn =
            std::mem::transmute(method(psl, VT_GET_WORKING_DIRECTORY));
        get_wd(psl, wbuf.as_mut_ptr(), wbuf.len() as i32);
        let wn = wbuf.iter().position(|&c| c == 0).unwrap_or(0);
        let workdir = String::from_utf16_lossy(&wbuf[..wn]);
        release(psl);

        Ok(ShortcutInfo { target, workdir })
    }
}

/// 清理 Windows 文件名非法字符（快捷方式名称用；对齐 Python 版）。
pub fn sanitize_name(text: &str) -> String {
    let cleaned: String = text
        .trim()
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    cleaned
        .trim_end_matches(|c| c == ' ' || c == '.')
        .trim_start_matches(|c| c == ' ' || c == '.')
        .chars()
        .take(120)
        .collect()
}

/// 同名自动编号，绝不覆盖桌面已有文件。
pub fn unique_lnk_path(dir: &str, base: &str) -> String {
    let first = format!("{dir}\\{base}.lnk");
    if !std::path::Path::new(&first).exists() {
        return first;
    }
    for i in 2..1000 {
        let candidate = format!("{dir}\\{base} ({i}).lnk");
        if !std::path::Path::new(&candidate).exists() {
            return candidate;
        }
    }
    first
}

/// 一步到位：桌面创建快捷方式 + 立刻读回校验。返回 (lnk 路径, 校验是否通过)。
pub fn pin_to_desktop(target: &str, name: &str) -> Result<(String, bool), String> {
    let desktop = winx::desktop_dir();
    if desktop.is_empty() {
        return Err("无法定位桌面目录".into());
    }
    let base = {
        let s = sanitize_name(name);
        if s.is_empty() {
            sanitize_name(&crate::store::file_stem(target))
        } else {
            s
        }
    };
    let base = if base.is_empty() { "快捷方式".to_string() } else { base };
    let lnk = unique_lnk_path(&desktop, &base);
    let workdir = std::path::Path::new(target)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    create_shortcut(&lnk, target, &workdir, target, &crate::store::file_stem(target))?;
    let verified = match read_shortcut(&lnk) {
        Ok(info) => info.target.eq_ignore_ascii_case(target),
        Err(_) => false,
    };
    Ok((lnk, verified))
}
