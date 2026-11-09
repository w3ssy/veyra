//! Bettet Manifest, Icons und Versionsinfo in die EXE ein (nur MSVC-Linker, ohne zusätzliche Abhängigkeit).
//!
//! Die Ressourcen werden direkt als `.res`-Datei geschrieben (kein `rc.exe` nötig) und dem Linker übergeben:
//! - Icon 1: Programm-Icon (Explorer, Taskleiste, Autostart-Liste)
//! - Icon 2/3: Tray-Icon für dunkle/helle Taskleiste
//! - Versionsinfo: Anzeigename „Veyra“ (Task-Manager, Eigenschaften → Details)

use std::path::{Path, PathBuf};

/// Ressourcen-IDs – müssen zu `src/platform/windows.rs` passen.
const ICONS: [(u16, &str); 3] = [(1, "assets/veyra.ico"), (2, "assets/tray-light.ico"), (3, "assets/tray-dark.ico")];

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;

fn main() {
    println!("cargo:rerun-if-changed=veyra.manifest");
    for (_, ico) in ICONS {
        println!("cargo:rerun-if-changed={ico}");
    }
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let manifest = root.join("veyra.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());

        let res = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("veyra.res");
        std::fs::write(&res, build_res(&root)).expect("veyra.res schreiben");
        // link.exe nimmt .res-Dateien direkt als Eingabe.
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else if windows {
        println!("cargo:warning=Kein MSVC-Linker: Manifest/Icons werden nicht eingebettet – Windows 11 24H2 braucht das Manifest!");
    }
}

// ───────────────────────────── .res-Format ─────────────────────────────

fn pad4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// Ein Eintrag mit numerischem Typ und Namen (Kopf = 32 Byte), Daten auf 4 Byte aufgefüllt.
fn push_resource(out: &mut Vec<u8>, typ: u16, name: u16, flags: u16, lang: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    out.extend([0xFFFFu16, typ, 0xFFFF, name].iter().flat_map(|v| v.to_le_bytes()));
    out.extend(0u32.to_le_bytes()); // DataVersion
    out.extend(flags.to_le_bytes());
    out.extend(lang.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // Version
    out.extend(0u32.to_le_bytes()); // Characteristics
    out.extend(data);
    pad4(out);
}

fn build_res(root: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    push_resource(&mut out, 0, 0, 0, 0, &[]); // Pflicht-Leereintrag am Anfang

    let mut next_image_id = 1u16;
    for (group_id, path) in ICONS {
        let ico = std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let u16_at = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([ico[o], ico[o + 1], ico[o + 2], ico[o + 3]]);
        assert!(ico.len() >= 6 && u16_at(2) == 1, "{path}: keine ICO-Datei");
        let count = u16_at(4) as usize;

        let mut group = Vec::new();
        group.extend([0u16, 1, count as u16].iter().flat_map(|v| v.to_le_bytes()));
        for i in 0..count {
            let e = 6 + i * 16;
            let (size, offset) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
            push_resource(&mut out, RT_ICON, next_image_id, 0x1010, 0, &ico[offset..offset + size]);
            group.extend(&ico[e..e + 12]); // Breite, Höhe, Farben, reserviert, Ebenen, Bits, Größe
            group.extend(next_image_id.to_le_bytes());
            next_image_id += 1;
        }
        push_resource(&mut out, RT_GROUP_ICON, group_id, 0x1030, 0, &group);
    }

    push_resource(&mut out, RT_VERSION, 1, 0x0030, 0x0409, &version_info());
    out
}

// ───────────────────────────── VERSIONINFO ─────────────────────────────

/// Knoten der VS_VERSIONINFO-Struktur: wLength, wValueLength, wType, Schlüssel, Wert, Kinder.
fn node(key: &str, value: &[u8], value_len: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0u8; 6];
    b.extend(utf16z(key));
    pad4(&mut b);
    b.extend(value);
    for c in children {
        pad4(&mut b);
        b.extend(c);
    }
    let len = b.len() as u16;
    b[0..2].copy_from_slice(&len.to_le_bytes());
    b[2..4].copy_from_slice(&value_len.to_le_bytes());
    b[4..6].copy_from_slice(&(text as u16).to_le_bytes());
    b
}

fn string(key: &str, value: &str) -> Vec<u8> {
    let v = utf16z(value);
    node(key, &v, (v.len() / 2) as u16, true, &[])
}

fn version_info() -> Vec<u8> {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let part = |k: &str| env(k).parse::<u32>().unwrap_or(0);
    let (major, minor, patch) = (part("CARGO_PKG_VERSION_MAJOR"), part("CARGO_PKG_VERSION_MINOR"), part("CARGO_PKG_VERSION_PATCH"));
    let version = env("CARGO_PKG_VERSION");

    let ms = (major << 16) | minor;
    let ls = patch << 16;
    let fixed: Vec<u8> = [
        0xFEEF04BD, // Signatur
        0x0001_0000,
        ms,
        ls, // Dateiversion
        ms,
        ls,         // Produktversion
        0x3F,       // FileFlagsMask
        0,          // FileFlags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,          // VFT_APP
        0,
        0,
        0,
    ]
    .iter()
    .flat_map(|v: &u32| v.to_le_bytes())
    .collect();

    let strings = node(
        "040904b0",
        &[],
        0,
        true,
        &[
            string("CompanyName", "w3ssy"),
            string("FileDescription", "Veyra"),
            string("FileVersion", &version),
            string("InternalName", "veyra"),
            string("LegalCopyright", "© w3ssy – MIT-Lizenz"),
            string("OriginalFilename", "veyra.exe"),
            string("ProductName", "Veyra – Motion for your universe."),
            string("ProductVersion", &version),
        ],
    );
    let string_info = node("StringFileInfo", &[], 0, true, &[strings]);
    let translation = node("Translation", &0x04B0_0409u32.to_le_bytes(), 4, false, &[]);
    let var_info = node("VarFileInfo", &[], 0, true, &[translation]);
    node("VS_VERSION_INFO", &fixed, fixed.len() as u16, false, &[string_info, var_info])
}
