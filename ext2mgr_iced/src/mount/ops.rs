//! Mount / unmount helpers.
//!
//! Persistence modes (same idea as Ext2Mgr’s letter picker):
//! - **Temporary** — `DefineDosDevice` only (Ext2Srv, or local when elevated). Lost after reboot.
//! - **Mount Manager** — `SetVolumeMountPoint` (Windows MountMgr). Survives reboot; Explorer-native.
//! - **Session Manager DOS Devices** — registry under
//!   `HKLM\...\Session Manager\DOS Devices`. Survives reboot. Classic also calls
//!   `DefineDosDevice` afterward so the letter appears immediately; that assign is
//!   not the same option as Temporary.
//! - **Ext2Fsd automount** — `Volumes\{UUID}` registry; remounted on refresh.
//!
//! Device path for registry / DefineDosDevice is `\Device\HarddiskVolumeN`
//! (Ext2Mgr `Volume->Name`).


use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMode {
    /// Option: Mount via DefineDosDevice only (lost after reboot).
    Temporary,
    /// Option: Windows Mount Manager (`SetVolumeMountPoint`).
    MountManager,
    /// Option: Session Manager DOS Devices registry (survives reboot).
    PermanentRegistry,
    /// Ext2Fsd `Volumes\{UUID}` automount — config persists; remounted by manager/driver.
    Ext2Automount,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MountRequest {
    pub letter: char,
    pub mode: MountMode,
    /// NT path written to Session Manager and/or passed to DefineDosDevice
    /// (`\Device\HarddiskVolumeN`).
    pub symlink: String,
    pub win32_volume_name: String,
    /// Required for Ext2Automount.
    pub uuid: Option<[u8; 16]>,
    pub codepage: String,
    pub readonly: bool,
}

pub fn free_drive_letters() -> Vec<char> {
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    let mut letters = Vec::new();
    for letter_index in 2u32..26 {
        if mask & (1 << letter_index) == 0 {
            letters.push(char::from(b'A' + letter_index as u8));
        }
    }
    letters
}

pub fn first_free_drive_letter() -> Option<char> {
    free_drive_letters().into_iter().next()
}

/// Prefer `physical_object` when it is `\Device\...` (Ext2Mgr `Volume->Name`).
pub fn dos_device_target(physical_object: &str, symlink: &str) -> String {
    if physical_object.starts_with(r"\Device\") {
        physical_object.to_string()
    } else if symlink.starts_with(r"\Device\") {
        symlink.to_string()
    } else if !physical_object.is_empty() {
        physical_object.to_string()
    } else {
        symlink.to_string()
    }
}

pub fn mount_volume(request: &MountRequest) -> Result<String, String> {
    match request.mode {
        MountMode::Temporary => {
            define_temporary(request.letter, &request.symlink)?;
            Ok(format!(
                "Mounted {}: temporary via DefineDosDevice (lost after reboot)",
                request.letter
            ))
        }
        MountMode::MountManager => {
            assign_via_mount_manager(
                request.letter,
                &request.symlink,
                &request.win32_volume_name,
            )?;
            Ok(format!(
                "Mounted {}: permanent via Mount Manager (SetVolumeMountPoint)",
                request.letter
            ))
        }
        MountMode::PermanentRegistry => {
            // Registry is the Session Manager option; DefineDosDevice is only the
            // immediate bind (same sequence as Ext2Mgr AddMountPoint + bRegistry).
            super::persist::set_registry_mount_point(request.letter, &request.symlink)?;
            define_temporary(request.letter, &request.symlink)?;
            Ok(format!(
                "Mounted {}: Session Manager DOS Devices (registry; immediate DefineDosDevice bind)",
                request.letter
            ))
        }
        MountMode::Ext2Automount => {
            let uuid = request.uuid.ok_or_else(|| {
                "Ext2Fsd automount needs an EXT volume UUID (select an EXT2/3/4 volume)".to_string()
            })?;
            super::persist::store_ext2_automount(
                &uuid,
                request.letter,
                &request.codepage,
                request.readonly,
            )?;
            define_temporary(request.letter, &request.symlink)?;
            Ok(format!(
                "Mounted {}: Ext2Fsd automount saved under Volumes\\{} (persists; remounted on refresh)",
                request.letter,
                super::persist::format_volume_uuid(&uuid)
            ))
        }
    }
}

pub fn unmount_letter(letter: char, symlink_hint: &str) -> Result<(), String> {
    let _ = super::persist::clear_registry_mount_point(letter);

    let mount_root = format!(r"{letter}:\");
    let _ = delete_volume_mount_point(&mount_root);

    if is_process_elevated() {
        let symlink = if !symlink_hint.is_empty() {
            symlink_hint.to_string()
        } else {
            query_dos_device(letter).unwrap_or_default()
        };
        if !symlink.is_empty() && define_dos_device_local_remove(letter, &symlink) {
            if query_dos_device(letter).is_none() {
                return Ok(());
            }
        }
    }

    let symlink = super::pipe::with_shared_client(|client| {
        match client.query_drive(letter as u8) {
            Ok(query) if query.result && !query.symlink.is_empty() => Ok(query.symlink),
            _ if !symlink_hint.is_empty() => Ok(symlink_hint.to_string()),
            _ => Ok(query_dos_device(letter).unwrap_or_default()),
        }
    })
    .map_err(|err| format!("Failed to remove drive letter {letter}:\nExt2Srv pipe: {err}"))?;
    if symlink.is_empty() {
        return Err(format!("Failed to remove drive letter {letter}:"));
    }
    let ok = super::pipe::with_shared_client(|client| client.remove_drive(letter as u8, &symlink))
        .map_err(|err| format!("Failed to remove drive letter {letter}:\n{err}"))?;
    if ok {
        Ok(())
    } else if query_dos_device(letter).is_none() {
        Ok(())
    } else {
        Err(format!("Failed to remove drive letter {letter}:"))
    }
}

pub fn pipe_status_str() -> String {
    match super::pipe::health_check() {
        Ok(()) => "Ext2Srv pipe: connected.".to_string(),
        Err(err) => format!("Ext2Srv pipe: {err} (is Ext2Srv running?)"),
    }
}

fn define_temporary(letter: char, symlink: &str) -> Result<(), String> {
    // Same order as Ext2Mgr `Ext2DefineDosDevice`: local when elevated, else Ext2Srv.
    if is_process_elevated() && define_dos_device_local(letter, symlink) {
        return Ok(());
    }
    let ok = super::pipe::with_shared_client(|client| client.define_drive(letter as u8, symlink)).map_err(
        |err| format!("Ext2Fsd service is not started.\n(Ext2Srv pipe: {err})"),
    )?;
    if ok {
        Ok(())
    } else {
        Err(format!("Failed to assign new drive letter {letter}:"))
    }
}

/// Ext2Mgr `Ext2AssignDrvLetter(..., bMountMgr=TRUE)` / `Ext2InsertMountPoint`.
fn assign_via_mount_manager(
    letter: char,
    symlink: &str,
    win32_volume_name: &str,
) -> Result<(), String> {
    let mount_root = format!(r"{letter}:\");
    let volume_name = if !win32_volume_name.is_empty() {
        let mut name = win32_volume_name.to_string();
        if !name.ends_with('\\') {
            name.push('\\');
        }
        name
    } else {
        // Classic: temporary DOS bind → query GUID name → remove DOS → SetVolumeMountPoint.
        define_temporary(letter, symlink)?;
        let probed = match volume_name_for_mount_point(&mount_root) {
            Ok(name) => name,
            Err(err) => {
                let _ = define_dos_device_local_remove(letter, symlink);
                return Err(err);
            }
        };
        let _ = define_dos_device_local_remove(letter, symlink);
        if query_dos_device(letter).is_some() {
            // Loose remove if exact match failed.
            let _ = define_dos_device_local_remove(letter, symlink);
        }
        probed
    };
    set_volume_mount_point(&mount_root, &volume_name)
}

fn volume_name_for_mount_point(mount_root: &str) -> Result<String, String> {
    let wide = to_wide(mount_root);
    let mut name = vec![0u16; 256];
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW(
            wide.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if ok == 0 {
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        return Err(format!(
            "GetVolumeNameForVolumeMountPoint({mount_root}) failed ({code})"
        ));
    }
    let end = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name.len());
    Ok(String::from_utf16_lossy(&name[..end]))
}

fn set_volume_mount_point(mount_root: &str, volume_name: &str) -> Result<(), String> {
    let root_w = to_wide(mount_root);
    let volume_w = to_wide(volume_name);
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::SetVolumeMountPointW(
            root_w.as_ptr(),
            volume_w.as_ptr(),
        )
    };
    if ok == 0 {
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        Err(format!(
            "SetVolumeMountPoint({mount_root}) failed ({code})"
        ))
    } else {
        Ok(())
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn is_process_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

fn define_dos_device_local(letter: char, symlink: &str) -> bool {
    let dos = format!("{letter}:");
    let dos_wide = to_wide(&dos);
    let target_wide = to_wide(symlink);
    unsafe {
        windows_sys::Win32::Storage::FileSystem::DefineDosDeviceW(
            super::pipe::DDD_RAW_TARGET_PATH,
            dos_wide.as_ptr(),
            target_wide.as_ptr(),
        ) != 0
    }
}

fn define_dos_device_local_remove(letter: char, symlink: &str) -> bool {
    let dos = format!("{letter}:");
    let dos_wide = to_wide(&dos);
    let target_wide = to_wide(symlink);
    let flags = super::pipe::DDD_RAW_TARGET_PATH
        | super::pipe::DDD_REMOVE_DEFINITION
        | super::pipe::DDD_EXACT_MATCH_ON_REMOVE;
    unsafe {
        windows_sys::Win32::Storage::FileSystem::DefineDosDeviceW(
            flags,
            dos_wide.as_ptr(),
            target_wide.as_ptr(),
        ) != 0
    }
}

fn query_dos_device(letter: char) -> Option<String> {
    let dos = format!("{letter}:");
    let wide = to_wide(&dos);
    let mut target = vec![0u16; 512];
    let written = unsafe {
        windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW(
            wide.as_ptr(),
            target.as_mut_ptr(),
            target.len() as u32,
        )
    };
    if written == 0 {
        None
    } else {
        let end = target.iter().position(|&unit| unit == 0).unwrap_or(target.len());
        Some(String::from_utf16_lossy(&target[..end]))
    }
}

fn delete_volume_mount_point(mount_root: &str) -> Result<(), String> {
    let wide = to_wide(mount_root);
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::DeleteVolumeMountPointW(wide.as_ptr())
    };
    if ok == 0 {
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        Err(format!("DeleteVolumeMountPoint failed ({code})"))
    } else {
        Ok(())
    }
}

pub fn is_ext_family(filesystem: &str) -> bool {
    let upper = filesystem.to_ascii_uppercase();
    upper.starts_with("EXT2") || upper.starts_with("EXT3") || upper.starts_with("EXT4")
}

#[derive(Debug, Clone, Default)]
pub struct AutomountReport {
    pub mounted: Vec<String>,
    pub already_mounted: usize,
    pub errors: Vec<String>,
}

impl AutomountReport {
    pub fn summary(&self) -> Option<String> {
        if self.mounted.is_empty() && self.errors.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.mounted.is_empty() {
            parts.push(format!(
                "Auto-mounted {} volume(s): {}",
                self.mounted.len(),
                self.mounted.join("; ")
            ));
        }
        if self.already_mounted > 0 {
            parts.push(format!(
                "{} already mounted (skipped)",
                self.already_mounted
            ));
        }
        if !self.errors.is_empty() {
            parts.push(format!("Auto-mount errors: {}", self.errors.join("; ")));
        }
        Some(parts.join(". "))
    }

    pub fn did_mount(&self) -> bool {
        !self.mounted.is_empty()
    }
}

/// Ext2Mgr `Ext2ProcessExt2Volumes` (+ dormant Session Manager re-bind).
///
/// For each volume that *should* have a letter (Ext2Fsd `MountPoint=` or Session
/// Manager DOS Devices), skip when already lettered / MountMgr-present / live DOS,
/// otherwise assign via DefineDosDevice (same as classic `Ext2MountVolumeAs`).
pub fn process_pending_automounts(
    volumes: &[crate::disk::enum_disk::VolumeEntry],
) -> AutomountReport {
    let mut report = AutomountReport::default();
    let free = free_drive_letters();

    for volume in volumes {
        let device = if volume.physical_object.starts_with(r"\Device\") {
            volume.physical_object.as_str()
        } else if volume.symlink.starts_with(r"\Device\") {
            volume.symlink.as_str()
        } else {
            ""
        };
        let already_live = !volume.letters.is_empty()
            || (!volume.win32_volume_name.is_empty()
                && !crate::mount::dead_letters::mountmgr_letters_for_volume(
                    &volume.win32_volume_name,
                )
                .is_empty());

        // Ext2Fsd Volumes\{UUID} MountPoint=X:  (Ext2ProcessExt2Property)
        if is_ext_family(&volume.filesystem) {
            if let Some(uuid) = volume.uuid {
                if let Some(preferred) = super::persist::query_ext2_automount_letter(&uuid) {
                    if already_live {
                        report.already_mounted += 1;
                    } else {
                        let letter = if free.contains(&preferred)
                            && query_dos_device(preferred).is_none()
                        {
                            preferred
                        } else if let Some(fallback) = free.iter().copied().find(|candidate| {
                            query_dos_device(*candidate).is_none()
                        }) {
                            fallback
                        } else {
                            report.errors.push(format!(
                                "{}: no free letter for Ext2Fsd automount",
                                super::persist::format_volume_uuid(&uuid)
                            ));
                            continue;
                        };
                        let symlink = dos_device_target(&volume.physical_object, &volume.symlink);
                        match define_temporary(letter, &symlink) {
                            Ok(()) => report.mounted.push(format!(
                                "{letter}: (Ext2Fsd automount → {symlink})"
                            )),
                            Err(err) => report.errors.push(err),
                        }
                    }
                    continue;
                }
            }
        }

        // Session Manager DOS Devices: activate registry letter if nothing live yet.
        if device.is_empty() {
            continue;
        }
        let session_letters = super::persist::registry_letters_for_device(device);
        if session_letters.is_empty() {
            continue;
        }
        if already_live {
            report.already_mounted += 1;
            continue;
        }
        for letter in session_letters {
            if query_dos_device(letter).is_some() {
                report.already_mounted += 1;
                continue;
            }
            match define_temporary(letter, device) {
                Ok(()) => report
                    .mounted
                    .push(format!("{letter}: (Session Manager → {device})")),
                Err(err) => report.errors.push(err),
            }
        }
    }

    report
}

/// Ext2Mgr OnFlush → Ext2FlushVolume: FlushFileBuffers on the volume handle.
pub fn flush_volume(
    letters: &[char],
    win32_volume_name: &str,
    physical_object: &str,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;

    let mut paths = Vec::new();
    if let Some(letter) = letters.first() {
        paths.push(format!(r"\\.\{letter}:"));
    }
    let trimmed = win32_volume_name.trim_end_matches('\\');
    if !trimmed.is_empty() {
        paths.push(trimmed.to_string());
    }
    if !physical_object.is_empty() {
        let rest = physical_object.trim_start_matches('\\');
        paths.push(format!(r"\\?\GLOBALROOT\{rest}"));
    }
    if paths.is_empty() {
        return Err("No path to flush".to_string());
    }

    let mut last_error = String::from("Flush failed");
    for path in paths {
        let wide: Vec<u16> = std::ffi::OsStr::new(&path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            windows_sys::Win32::Storage::FileSystem::CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
                std::ptr::null(),
                windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
                0,
                0,
            )
        };
        if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            last_error = format!("Open {path} failed ({code})");
            continue;
        }
        let ok = unsafe { windows_sys::Win32::Storage::FileSystem::FlushFileBuffers(handle) };
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        if ok != 0 {
            return Ok(());
        }
        last_error = format!("FlushFileBuffers({path}) failed ({code})");
    }
    Err(last_error)
}
