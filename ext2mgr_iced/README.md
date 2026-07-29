# Ext2Mgr (Iced port)

Rust + [Iced](https://iced.rs/) GUI for managing Ext2/Ext4 volumes on Windows. Modeled on classic **Ext2 Volume Manager** (`Ext2Mgr/`).

## Features

- Dual volume and disk/partition lists with native Win32 menu, tray, and context menus
- Assign / change / remove drive letters with four persistence modes (below)
- Ext2 volume attributes, service management, properties dialog/pane
- Remove Dead Letters (orphans, Explorer-dead DOS siblings, dormant Session Manager registry letters)
- Startup and refresh automount for Ext2Fsd `MountPoint=` and dormant Session Manager letters
- Windows accent theming; View menu for properties pane, SI/binary units, bytes/bits

Source layout: [`docs/SOURCE_LAYOUT.md`](docs/SOURCE_LAYOUT.md) (`disk` / `mount` / `service` / `win` / `ui`).

Compared to classic Ext2Mgr: [`PORT_IMPROVEMENTS.md`](PORT_IMPROVEMENTS.md). Classic Ext2Mgr / Ext2Srv fixes: [`../Ext2Mgr/IMPROVEMENTS.md`](../Ext2Mgr/IMPROVEMENTS.md).

## Persistence (important)

Four distinct options (do not conflate):

| Mode | Survives reboot? | What it is |
|------|------------------|------------|
| Temporary | **No** | `DefineDosDevice` only (Ext2Srv, or local when elevated) |
| Mount Manager | **Yes** | `SetVolumeMountPoint` — same mechanism Windows Disk Management uses. Needs a Win32 volume GUID. Needs Administrator. |
| Session Manager DOS Devices | **Yes** | Registry `HKLM\...\Session Manager\DOS Devices` (`X:` → `\Device\HarddiskVolumeN`). Immediate `DefineDosDevice` bind afterward so the letter appears now — that bind is not the Temporary option. Needs Administrator. |
| Ext2Fsd automount | **Config yes** | `HKLM\...\Ext2Fsd\Volumes\{UUID}` with `MountPoint=X:;`. Remounted on load/refresh. Needs EXT UUID + Administrator. |

Device paths match Ext2Mgr (`Volume->Name` = `\Device\HarddiskVolumeN`). Mount Manager is disabled in the letter picker when the volume has no Win32 GUID.

## Build & run

```bash
cargo build --release
cargo run
cargo run --release
```

From `ext2mgr_iced/`. Run **elevated** for permanent registry / Ext2Fsd Volumes / service parameter writes. Ext2Srv must be running for temporary letter ops (unless elevated local `DefineDosDevice` succeeds).
