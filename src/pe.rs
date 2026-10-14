//! Windows: which DLLs the plugins and the libraries of a release need, read
//! from the import tables of the images instead of being listed. The DLLs the
//! build produced are followed transitively; what is imported and is not among
//! them is Windows' own (`kernel32`, the `api-ms-win-*` forwarders, ...)
//! except the Visual C++ runtime, which the runtime of a release carries (the
//! app itself links the C runtime statically).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Lower-case file name -> path of every DLL directly in `dir`.
pub fn dlls_in(dir: &Path) -> Result<HashMap<String, PathBuf>> {
    let mut dlls = HashMap::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.to_ascii_lowercase().ends_with(".dll")
        {
            dlls.insert(name.to_ascii_lowercase(), path);
        }
    }
    Ok(dlls)
}

/// The DLL the library `name` belongs to among `available` (as [`dlls_in`]
/// returns them): `name` is what a `.pc` file links with `-l<name>`, the
/// import library `<name>.lib`; meson names its DLL `<name>-<soversion>.dll`
/// (`glib-2.0-0.dll`, `intl-8.dll`, `z-1.dll`) or `<name>.dll`. `None` for a
/// static library, which has no DLL.
pub fn dll_of_lib(available: &HashMap<String, PathBuf>, name: &str) -> Result<Option<PathBuf>> {
    let name = name.to_ascii_lowercase();
    let mut found: Vec<&PathBuf> = available
        .iter()
        .filter(|(file, _)| {
            file.strip_suffix(".dll").is_some_and(|stem| {
                stem == name
                    || stem
                        .strip_prefix(name.as_str())
                        .and_then(|rest| rest.strip_prefix('-'))
                        .is_some_and(|version| {
                            !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
                        })
            })
        })
        .map(|(_, path)| path)
        .collect();
    found.sort();
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some((*one).clone())),
        several => bail!(
            "the library {name} matches several DLLs: {}",
            several
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[derive(Debug, Default)]
pub struct Closure {
    /// File name -> path of the DLLs found among the build's.
    pub bundled: BTreeMap<String, PathBuf>,
    /// Lower-case names of C++ runtime DLLs that are not among them.
    pub runtime: BTreeSet<String>,
}

/// Every DLL from `available` that `roots` import, directly or through each
/// other; `read` gives the bytes of an image. Imports that are not in
/// `available` are Windows' own (kernel32, the `api-ms-win-*` forwarders,
/// ...) except the Visual C++ runtime.
pub fn dll_closure(
    available: &HashMap<String, PathBuf>,
    roots: &[PathBuf],
    read: impl Fn(&Path) -> std::io::Result<Vec<u8>>,
) -> Result<Closure> {
    let mut closure = Closure::default();
    let mut seen = HashSet::new();
    let mut queue: Vec<PathBuf> = roots.to_vec();
    while let Some(path) = queue.pop() {
        let image = read(&path).with_context(|| format!("read {}", path.display()))?;
        let imports =
            pe_imports(&image).with_context(|| format!("{} is not a PE image", path.display()))?;
        for import in imports {
            let lower = import.to_ascii_lowercase();
            if let Some(found) = available.get(&lower) {
                if seen.insert(lower) {
                    if let Some(name) = found.file_name().and_then(|n| n.to_str()) {
                        closure.bundled.insert(name.to_owned(), found.clone());
                    }
                    queue.push(found.clone());
                }
            } else if is_vc_runtime(&lower) {
                closure.runtime.insert(lower);
            }
        }
    }
    Ok(closure)
}

fn is_vc_runtime(lower: &str) -> bool {
    lower.ends_with(".dll")
        && ["vcruntime", "msvcp", "concrt", "vcomp", "vccorlib"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

/// The redistributable runtime: `$VCToolsRedistDir` (set by the Visual
/// Studio developer environment), else the system directory, where the
/// redistributable puts the same files.
pub fn find_vc_runtime(name: &str) -> Result<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(redist) = std::env::var_os("VCToolsRedistDir")
        && let Ok(entries) = fs::read_dir(Path::new(&redist).join("x64"))
    {
        dirs.extend(entries.flatten().map(|e| e.path()).filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("Microsoft.VC") && n.ends_with(".CRT"))
        }));
    }
    if let Some(system) = std::env::var_os("SystemRoot") {
        dirs.push(Path::new(&system).join("System32"));
    }
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .with_context(|| format!("{name} not found; install the Visual C++ redistributable"))
}

fn le_u16(data: &[u8], at: usize) -> Option<usize> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(usize::from(u16::from_le_bytes(bytes.try_into().ok()?)))
}

fn le_u32(data: &[u8], at: usize) -> Option<usize> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    usize::try_from(u32::from_le_bytes(bytes.try_into().ok()?)).ok()
}

/// The DLLs a PE image imports: regular and delay-load import tables.
/// Layout: PE format, https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
pub fn pe_imports(image: &[u8]) -> Option<Vec<String>> {
    /// Data directory indexes.
    const IMPORT: usize = 1;
    const DELAY_IMPORT: usize = 13;
    const MAX_DESCRIPTORS: usize = 4096;

    let pe = le_u32(image, 0x3c)?;
    if image.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let sections = le_u16(image, pe + 6)?;
    let optional_size = le_u16(image, pe + 20)?;
    let optional = pe + 24;
    // PE32 (0x10b) has its data directories at 96, PE32+ (0x20b) at 112.
    let directories = optional
        + match le_u16(image, optional)? {
            0x10b => 96,
            0x20b => 112,
            _ => return None,
        };
    let table = optional + optional_size;

    let rva_to_offset = |rva: usize| -> Option<usize> {
        (0..sections).find_map(|i| {
            let section = table + i * 40;
            let size = le_u32(image, section + 8)?.max(le_u32(image, section + 16)?);
            let virtual_address = le_u32(image, section + 12)?;
            let raw = le_u32(image, section + 20)?;
            let inside = rva.checked_sub(virtual_address).filter(|d| *d < size)?;
            raw.checked_add(inside)
        })
    };
    let name_at = |rva: usize| -> Option<String> {
        let start = rva_to_offset(rva)?;
        let bytes = image.get(start..)?;
        let len = bytes.iter().take(256).position(|b| *b == 0)?;
        Some(String::from_utf8_lossy(bytes.get(..len)?).into_owned())
    };

    let mut imports = Vec::new();
    for (index, size, name_field) in [(IMPORT, 20, 12), (DELAY_IMPORT, 32, 4)] {
        let rva = le_u32(image, directories + index * 8)?;
        if rva == 0 {
            continue;
        }
        let mut descriptor = rva_to_offset(rva)?;
        for _ in 0..MAX_DESCRIPTORS {
            let name = le_u32(image, descriptor + name_field)?;
            if name == 0 {
                break;
            }
            imports.push(name_at(name)?);
            descriptor += size;
        }
    }
    Some(imports)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal PE32+ image whose only import is `names`.
    pub(crate) fn pe_image(names: &[&str]) -> Vec<u8> {
        let mut image = vec![0u8; 0x400];
        let put = |image: &mut Vec<u8>, at: usize, bytes: &[u8]| {
            image[at..at + bytes.len()].copy_from_slice(bytes);
        };
        put(&mut image, 0x3c, &0x80u32.to_le_bytes());
        put(&mut image, 0x80, b"PE\0\0");
        put(&mut image, 0x80 + 6, &1u16.to_le_bytes()); // one section
        put(&mut image, 0x80 + 20, &240u16.to_le_bytes()); // optional header size
        put(&mut image, 0x80 + 24, &0x20bu16.to_le_bytes());
        // Import directory: RVA 0x1000 (file offset 0x200 after the section below).
        put(&mut image, 0x80 + 24 + 112 + 8, &0x1000u32.to_le_bytes());
        let section = 0x80 + 24 + 240;
        put(&mut image, section + 8, &0x200u32.to_le_bytes()); // virtual size
        put(&mut image, section + 12, &0x1000u32.to_le_bytes()); // virtual address
        put(&mut image, section + 16, &0x200u32.to_le_bytes()); // raw size
        put(&mut image, section + 20, &0x200u32.to_le_bytes()); // raw offset
        // Descriptors (20 bytes each, zero terminated) then the names.
        let names_at = 0x200 + 20 * (names.len() + 1);
        let mut cursor = names_at;
        for (i, name) in names.iter().enumerate() {
            let rva = 0x1000 + (cursor - 0x200);
            put(&mut image, 0x200 + i * 20 + 12, &(rva as u32).to_le_bytes());
            put(&mut image, cursor, name.as_bytes());
            cursor += name.len() + 1;
        }
        image
    }

    #[test]
    fn reads_pe_imports() {
        let image = pe_image(&["KERNEL32.dll", "gstreamer-1.0-0.dll"]);
        assert_eq!(
            pe_imports(&image),
            Some(vec!["KERNEL32.dll".into(), "gstreamer-1.0-0.dll".into()])
        );
    }

    #[test]
    fn truncated_or_foreign_images_are_rejected_not_panicked_on() {
        let image = pe_image(&["a.dll"]);
        for len in [0, 2, 0x3c, 0x80, 0x90, 0x150, 0x201, 0x212] {
            // Any prefix is either parsed or rejected.
            let _ = pe_imports(&image[..len]);
        }
        assert_eq!(pe_imports(b"\x7fELF"), None);
    }

    #[test]
    fn follows_imports_through_bundled_dlls_only() {
        let images: HashMap<PathBuf, Vec<u8>> = [
            (
                "bin/app.exe",
                pe_image(&["GStreamer-1.0-0.dll", "KERNEL32.dll"]),
            ),
            (
                "bin/gstreamer-1.0-0.dll",
                pe_image(&["glib-2.0-0.dll", "VCRUNTIME140.dll"]),
            ),
            (
                "bin/glib-2.0-0.dll",
                pe_image(&["api-ms-win-crt-heap-l1-1-0.dll"]),
            ),
            ("bin/unrelated.dll", pe_image(&[])),
        ]
        .into_iter()
        .map(|(path, image)| (PathBuf::from(path), image))
        .collect();
        // The DLLs of the build's `bin` by lower-case name.
        let available: HashMap<String, PathBuf> =
            ["gstreamer-1.0-0.dll", "glib-2.0-0.dll", "unrelated.dll"]
                .into_iter()
                .map(|name| (name.to_owned(), PathBuf::from("bin").join(name)))
                .collect();
        let read = |path: &Path| {
            images
                .get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        };
        let closure = dll_closure(&available, &[PathBuf::from("bin/app.exe")], read).unwrap();
        assert_eq!(
            closure.bundled.keys().collect::<Vec<_>>(),
            ["glib-2.0-0.dll", "gstreamer-1.0-0.dll"]
        );
        assert_eq!(
            closure.runtime.into_iter().collect::<Vec<_>>(),
            ["vcruntime140.dll"]
        );
    }

    #[test]
    fn a_root_that_cannot_be_read_or_is_no_pe_image_is_named_in_the_error() {
        let none = HashMap::new();
        let unreadable = dll_closure(&none, &[PathBuf::from("bin/missing.exe")], |_| {
            Err(std::io::ErrorKind::NotFound.into())
        })
        .unwrap_err();
        assert_eq!(unreadable.to_string(), "read bin/missing.exe");
        let foreign = dll_closure(&none, &[PathBuf::from("bin/app.elf")], |_| {
            Ok(b"\x7fELF".to_vec())
        })
        .unwrap_err();
        assert_eq!(foreign.to_string(), "bin/app.elf is not a PE image");
    }

    #[test]
    fn only_the_visual_cpp_runtime_is_taken_from_outside_the_build() {
        for runtime in [
            "vcruntime140.dll",
            "vcruntime140_1.dll",
            "msvcp140.dll",
            "concrt140.dll",
            "vcomp140.dll",
        ] {
            assert!(is_vc_runtime(runtime), "{runtime}");
        }
        for system in [
            "kernel32.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-heap-l1-1-0.dll",
            "msvcrt.dll",
        ] {
            assert!(!is_vc_runtime(system), "{system}");
        }
        // A file of the same name that is no DLL is none either.
        assert!(!is_vc_runtime("vcruntime140.lib"));
    }

    /// The DLLs of the `bin` of a Windows build, by lower-case name.
    fn build_bin() -> HashMap<String, PathBuf> {
        [
            "glib-2.0-0.dll",
            "gobject-2.0-0.dll",
            "gstreamer-1.0-0.dll",
            "gstapp-1.0-0.dll",
            "intl-8.dll",
            "z-1.dll",
            "orc-0.4-0.dll",
            "opus-0.dll",
            "ffi-7.dll",
            "avcodec-61.dll",
            "pkgconf.dll",
        ]
        .into_iter()
        .map(|name| (name.to_owned(), PathBuf::from("bin").join(name)))
        .collect()
    }

    #[test]
    fn a_library_belongs_to_the_dll_named_after_it_and_its_soversion() {
        let bin = build_bin();
        let dll = |lib: &str| dll_of_lib(&bin, lib).unwrap();
        assert_eq!(dll("glib-2.0"), Some(PathBuf::from("bin/glib-2.0-0.dll")));
        assert_eq!(
            dll("gstreamer-1.0"),
            Some(PathBuf::from("bin/gstreamer-1.0-0.dll"))
        );
        assert_eq!(
            dll("gstapp-1.0"),
            Some(PathBuf::from("bin/gstapp-1.0-0.dll"))
        );
        assert_eq!(dll("intl"), Some(PathBuf::from("bin/intl-8.dll")));
        assert_eq!(dll("z"), Some(PathBuf::from("bin/z-1.dll")));
        assert_eq!(dll("orc-0.4"), Some(PathBuf::from("bin/orc-0.4-0.dll")));
        // No soversion at all.
        assert_eq!(dll("pkgconf"), Some(PathBuf::from("bin/pkgconf.dll")));
        // The case of the name does not matter.
        assert_eq!(dll("GLib-2.0"), Some(PathBuf::from("bin/glib-2.0-0.dll")));
    }

    #[test]
    fn a_static_library_and_a_longer_name_have_no_dll() {
        let bin = build_bin();
        // dav1d is linked statically.
        assert_eq!(dll_of_lib(&bin, "dav1d").unwrap(), None);
        // `gst` is no prefix of `gstreamer-1.0-0.dll` in the sense of a name and a soversion.
        assert_eq!(dll_of_lib(&bin, "gst").unwrap(), None);
        // Neither is `glib` one of `glib-2.0-0.dll`: `2.0-0` is no soversion.
        assert_eq!(dll_of_lib(&bin, "glib").unwrap(), None);
    }

    #[test]
    fn two_dlls_for_one_library_are_an_error() {
        let mut bin = build_bin();
        bin.insert("intl-9.dll".into(), PathBuf::from("bin/intl-9.dll"));
        let error = dll_of_lib(&bin, "intl").unwrap_err().to_string();
        assert!(
            error.contains("intl-8.dll") && error.contains("intl-9.dll"),
            "{error}"
        );
    }
}
