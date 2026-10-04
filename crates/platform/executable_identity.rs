//! Open the executable image that this process actually started from.
//!
//! The desktop uses this only while its launch snapshot reader is running.
//! Linux exposes the executing file through the kernel-owned `/proc/self/exe`
//! link. macOS reopens the launch path through a no-follow directory
//! capability and proves that the file's architecture and Mach-O `LC_UUID`
//! match the loaded image containing a caller-supplied function pointer. The
//! caller must pass an address in its main executable; `dladdr` identifies the
//! image containing that address, so injected libraries and dyld image order
//! cannot make a library's identity stand in for the main executable.
//!
//! Windows asks the kernel which file the loader mapped as the main image
//! (`GetMappedFileNameW`, which names the file object itself and so follows it
//! if it was renamed after launch), opens that file, and requires its PE header
//! block to match the loaded one byte for byte except the `ImageBase` the loader
//! rewrites when it relocates. A different file placed at the launch path is
//! never read.
//!
//! Other platforms deliberately have no implementation yet. Callers must
//! treat that as an unknown identity and revalidate their cache.

use std::fs::File;
use std::io;
use std::time::SystemTime;

/// Opens the running executable image, or refuses when it cannot establish
/// the relationship between this process and the bytes being read.
///
/// On macOS and Windows, `main_image_anchor` must be a function compiled into the main
/// executable whose bytes the caller wants to identify. The returned opaque
/// handle retains the no-follow file descriptor and the identity stamp taken
/// before Mach-O verification; reads through it are checked against that same
/// stamp after completion.
///
/// # Errors
/// Returns an I/O error if the current image cannot be proven or opened.
pub fn open_running_executable(main_image_anchor: fn()) -> io::Result<RunningExecutable> {
    #[cfg(target_os = "linux")]
    {
        let _ = main_image_anchor;
        let file = File::open("/proc/self/exe")?;
        RunningExecutable::new(file)
    }

    #[cfg(target_os = "macos")]
    {
        macos::open_running_executable(main_image_anchor)
    }

    #[cfg(windows)]
    {
        windows::open_running_executable(main_image_anchor)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = main_image_anchor;
        Err(unsupported())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// A held executable file whose identity was captured before platform image
/// verification. Its file descriptor is never reopened by pathname.
pub struct RunningExecutable {
    file: File,
    stamp: FileStamp,
}

impl RunningExecutable {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn new(file: File) -> io::Result<Self> {
        let stamp = FileStamp::capture(&file)?;
        Ok(Self { file, stamp })
    }

    /// Runs a bounded read against the held executable and rejects the result
    /// if the same file changed since platform identity verification.
    pub fn with_verified_read<T>(
        &mut self,
        read: impl FnOnce(&mut File) -> io::Result<T>,
    ) -> io::Result<T> {
        self.stamp.verify(&self.file)?;
        use std::io::Seek as _;
        self.file.seek(io::SeekFrom::Start(0))?;
        let value = read(&mut self.file)?;
        self.stamp.verify(&self.file)?;
        Ok(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileStamp {
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    unix_identity: (u64, u64, i64, i64, i64, i64),
}

impl FileStamp {
    fn capture(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("running executable is not a regular file"));
        }
        #[cfg(unix)]
        let unix_identity = {
            use std::os::unix::fs::MetadataExt as _;
            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            )
        };
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            unix_identity,
        })
    }

    fn verify(&self, file: &File) -> io::Result<()> {
        let current = Self::capture(file)?;
        if self == &current {
            Ok(())
        } else {
            Err(invalid("running executable changed during identity read"))
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "running executable identity is not proven on this platform",
    )
}

#[cfg(windows)]
mod windows {
    use super::{FileStamp, RunningExecutable, invalid};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::FileExt as _;
    use std::path::PathBuf;
    use std::ptr;
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleHandleExW, GetModuleHandleW,
    };
    use windows_sys::Win32::System::ProcessStatus::GetMappedFileNameW;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// The longest header block the comparison reads from either side. Real executables keep
    /// their headers in the first page or two; the bound stops a hostile `SizeOfHeaders`.
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    /// Longest NT path of a mapped file that is read back (in UTF-16 units).
    const MAX_MAPPED_NAME_UNITS: usize = 32 * 1024;

    pub(super) fn open_running_executable(anchor: fn()) -> io::Result<RunningExecutable> {
        let base = main_image_containing(anchor)?;
        let path = mapped_image_path(base)?;
        let file = OpenOptions::new().read(true).open(&path)?;
        let stamp = FileStamp::capture(&file)?;

        // SAFETY: `base` is the base of the main executable image, which stays mapped for the
        // life of the process, and its first `MAX_HEADER_BYTES` bytes lie inside the headers
        // region only as far as the loaded header claims (`loaded_headers` bounds it).
        let loaded = unsafe { loaded_headers(base)? };
        let on_disk = disk_headers(&file, loaded.len())?;
        if !headers_match(&loaded, &on_disk)? {
            return Err(invalid(
                "the file at the mapped image's path is not the loaded main image",
            ));
        }
        stamp.verify(&file)?;
        Ok(RunningExecutable { file, stamp })
    }

    /// The base address of the main executable, provided `anchor` lies inside it. A function in
    /// an injected library, or in a DLL the executable loaded, is refused: its identity must not
    /// stand in for the executable's.
    fn main_image_containing(anchor: fn()) -> io::Result<*const u8> {
        let mut containing: HMODULE = ptr::null_mut();
        // SAFETY: the address is a live function pointer; with the FROM_ADDRESS flag the second
        // argument is read as an address and not as a string. UNCHANGED_REFCOUNT leaves the
        // module's reference count alone, so there is no handle to release. `containing` is a
        // writable local.
        let found = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                anchor as usize as *const u16,
                &raw mut containing,
            )
        };
        if found == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a null module name asks for the handle of the calling process's executable.
        let main = unsafe { GetModuleHandleW(ptr::null()) };
        if containing.is_null() || containing != main {
            return Err(invalid(
                "the anchor is not inside the main executable image",
            ));
        }
        Ok(containing.cast::<u8>().cast_const())
    }

    /// The file the loader mapped as the image at `base`, named by the kernel from the file
    /// object itself. Unlike the load-time path in the loader's module list, it follows the file
    /// if it was renamed after launch, so replacing the launch path with another file does not
    /// redirect the read.
    fn mapped_image_path(base: *const u8) -> io::Result<PathBuf> {
        let mut name = vec![0_u16; 1024];
        loop {
            let capacity =
                u32::try_from(name.len()).map_err(|_| invalid("mapped name too long"))?;
            // SAFETY: the current process pseudo-handle is always valid; `base` is a mapped
            // address; `name` is writable for `capacity` units.
            let written = unsafe {
                GetMappedFileNameW(
                    GetCurrentProcess(),
                    base.cast(),
                    name.as_mut_ptr(),
                    capacity,
                )
            };
            if written == 0 {
                return Err(io::Error::last_os_error());
            }
            let written = written as usize;
            if written < name.len() {
                name.truncate(written);
                break;
            }
            // The name was truncated to the buffer: grow, within a bound.
            if name.len() >= MAX_MAPPED_NAME_UNITS {
                return Err(invalid("the mapped image's name exceeds its bound"));
            }
            name.resize(name.len() * 2, 0);
        }
        let device_path = String::from_utf16(&name)
            .map_err(|_| invalid("the mapped image's name is not valid UTF-16"))?;
        if !device_path.starts_with(r"\Device\") {
            return Err(invalid("the mapped image's name is not an NT device path"));
        }
        // `\\?\GLOBALROOT` followed by the NT name is a path CreateFileW opens as given.
        let mut path = String::from(r"\\?\GLOBALROOT");
        path.push_str(&device_path);
        Ok(PathBuf::from(std::ffi::OsString::from(path)))
    }

    /// The header block of the loaded main image, as the loader mapped it.
    ///
    /// # Safety
    /// `base` must be the base address of a mapped PE image that stays mapped for the duration
    /// of the call.
    unsafe fn loaded_headers(base: *const u8) -> io::Result<Vec<u8>> {
        // SAFETY: the DOS header and the bytes the loader validated behind `e_lfanew` are
        // mapped; the first read is the fixed prefix every PE image has, and the second is the
        // `SizeOfHeaders` the loader itself accepted, bounded again here.
        let prefix = unsafe { std::slice::from_raw_parts(base, PE_PREFIX_BYTES) };
        let layout = parse_pe_layout(prefix, true)?;
        // SAFETY: as above; `layout.size_of_headers` is bounded by `MAX_HEADER_BYTES`.
        let headers = unsafe { std::slice::from_raw_parts(base, layout.size_of_headers) };
        Ok(headers.to_vec())
    }

    /// The first `length` bytes of the file, after checking it begins with a plausible PE header
    /// block of that length.
    fn disk_headers(file: &File, length: usize) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0_u8; length];
        let mut filled = 0;
        while filled < bytes.len() {
            let read = file.seek_read(&mut bytes[filled..], filled as u64)?;
            if read == 0 {
                return Err(invalid("the executable ends inside its headers"));
            }
            filled += read;
        }
        parse_pe_layout(&bytes, false)?;
        Ok(bytes)
    }

    /// Bytes every PE image has at its start that the layout parse reads: the DOS header, the
    /// signature, the file header and the fixed part of the optional header.
    const PE_PREFIX_BYTES: usize = 4096;

    /// Where the pieces of a PE header block that matter for identity lie.
    #[derive(Debug, Eq, PartialEq)]
    pub(super) struct PeLayout {
        /// Bytes of the whole header block (`SizeOfHeaders`).
        pub(super) size_of_headers: usize,
        /// The `ImageBase` field, which the loader rewrites in the mapped copy when it relocates
        /// the image.
        pub(super) image_base: std::ops::Range<usize>,
    }

    /// Parses the layout of the PE header block in `bytes`, refusing anything malformed. With
    /// `prefix_only`, `bytes` is just the fixed prefix and the block length is not checked
    /// against it.
    pub(super) fn parse_pe_layout(bytes: &[u8], prefix_only: bool) -> io::Result<PeLayout> {
        const DOS_MAGIC: &[u8; 2] = b"MZ";
        const NT_SIGNATURE: &[u8; 4] = b"PE\0\0";
        const FILE_HEADER_BYTES: usize = 20;
        const OPTIONAL_PE32: u16 = 0x10b;
        const OPTIONAL_PE32_PLUS: u16 = 0x20b;
        // Offsets inside the optional header.
        const IMAGE_BASE_PE32: usize = 28;
        const IMAGE_BASE_PE32_PLUS: usize = 24;
        const SIZE_OF_IMAGE: usize = 56;
        const SIZE_OF_HEADERS: usize = 60;

        let u16_at = |offset: usize| -> io::Result<u16> {
            bytes
                .get(offset..offset + 2)
                .and_then(|slice| slice.try_into().ok())
                .map(u16::from_le_bytes)
                .ok_or_else(|| invalid("truncated PE header"))
        };
        let u32_at = |offset: usize| -> io::Result<u32> {
            bytes
                .get(offset..offset + 4)
                .and_then(|slice| slice.try_into().ok())
                .map(u32::from_le_bytes)
                .ok_or_else(|| invalid("truncated PE header"))
        };
        if bytes.get(..2) != Some(DOS_MAGIC) {
            return Err(invalid("missing DOS header"));
        }
        let nt = usize::try_from(u32_at(0x3c)?).map_err(|_| invalid("PE header offset"))?;
        let signature_end = nt
            .checked_add(4)
            .ok_or_else(|| invalid("PE header offset"))?;
        if bytes.get(nt..signature_end) != Some(NT_SIGNATURE) {
            return Err(invalid("missing PE signature"));
        }
        let optional = signature_end
            .checked_add(FILE_HEADER_BYTES)
            .ok_or_else(|| invalid("PE header offset"))?;
        let optional_size = usize::from(u16_at(signature_end + 16)?);
        let image_base_offset = match u16_at(optional)? {
            OPTIONAL_PE32 => IMAGE_BASE_PE32,
            OPTIONAL_PE32_PLUS => IMAGE_BASE_PE32_PLUS,
            _ => return Err(invalid("unsupported optional header")),
        };
        let image_base_width = if image_base_offset == IMAGE_BASE_PE32 {
            4
        } else {
            8
        };
        if optional_size < SIZE_OF_HEADERS + 4 {
            return Err(invalid("optional header too small"));
        }
        let size_of_image = u32_at(optional + SIZE_OF_IMAGE)? as usize;
        let size_of_headers = u32_at(optional + SIZE_OF_HEADERS)? as usize;
        let header_end = optional
            .checked_add(optional_size)
            .ok_or_else(|| invalid("PE header offset"))?;
        if size_of_headers < header_end
            || size_of_headers > size_of_image
            || size_of_headers > MAX_HEADER_BYTES
            || (!prefix_only && size_of_headers > bytes.len())
        {
            return Err(invalid("SizeOfHeaders is outside the image"));
        }
        let image_base =
            optional + image_base_offset..optional + image_base_offset + image_base_width;
        Ok(PeLayout {
            size_of_headers,
            image_base,
        })
    }

    /// Whether the loaded header block and the file's are the same executable.
    ///
    /// Every byte must agree except the `ImageBase` field: the loader rewrites it in its copy
    /// when it relocates the image. The block holds the timestamp, checksum, entry point, image
    /// size and section table, so a different build of the same program does not match.
    pub(super) fn headers_match(loaded: &[u8], on_disk: &[u8]) -> io::Result<bool> {
        let layout = parse_pe_layout(loaded, false)?;
        if loaded.len() != layout.size_of_headers || on_disk.len() != loaded.len() {
            return Ok(false);
        }
        let masked = layout.image_base;
        Ok(loaded[..masked.start] == on_disk[..masked.start]
            && loaded[masked.end..] == on_disk[masked.end..])
    }

    #[cfg(test)]
    mod tests {
        use super::{PeLayout, headers_match, parse_pe_layout};

        use super::super::open_running_executable;
        use std::io::Read as _;
        use std::process::Command;

        const CHILD_MODE: &str = "NUDOX_TEST_PE_CHILD";
        const PRISTINE: &str = "NUDOX_TEST_PE_PRISTINE";

        fn anchor() {}

        fn read_all(executable: &mut super::super::RunningExecutable) -> Vec<u8> {
            executable
                .with_verified_read(|file| {
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes)?;
                    Ok(bytes)
                })
                .expect("verified read")
        }

        #[test]
        fn the_running_test_binary_is_opened_and_read_back_exactly() {
            if std::env::var_os(CHILD_MODE).is_some() {
                return;
            }
            let mut executable = open_running_executable(anchor).expect("open the running image");
            let expected = std::fs::read(std::env::current_exe().expect("test executable"))
                .expect("read the test executable by path");
            assert_eq!(read_all(&mut executable), expected);
        }

        #[test]
        fn an_anchor_inside_a_loaded_library_is_refused() {
            if std::env::var_os(CHILD_MODE).is_some() {
                return;
            }
            // SAFETY: the address of a kernel32 export is only ever compared against the main
            // module's range, never called, so its real signature does not matter.
            let library_function: fn() = unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> u32, fn()>(
                    windows_sys::Win32::System::Threading::GetCurrentThreadId,
                )
            };
            let error = open_running_executable(library_function)
                .err()
                .expect("a library's identity must not stand in for the executable's");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        }

        /// A copy of the test binary renames its own file away after launch and puts an imposter
        /// at the launch path, as a self-updater (or an attacker with write access to the
        /// install directory) would. The image it is running is still the renamed file, and that
        /// is the file it must open: never the imposter, and not a refusal either.
        #[test]
        fn a_file_swapped_into_the_launch_path_is_never_read() {
            if std::env::var_os(CHILD_MODE).is_some() {
                return;
            }
            let directory = std::env::temp_dir().join(format!(
                "nudox-pe-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
            std::fs::create_dir(&directory).expect("test directory");
            let source = std::env::current_exe().expect("test executable");
            let launched = directory.join("launched.exe");
            let pristine = directory.join("pristine.bin");
            std::fs::copy(&source, &launched).expect("copy the test executable");
            std::fs::copy(&source, &pristine).expect("keep the original bytes");

            let status = Command::new(&launched)
                .args([
                    "--exact",
                    "executable_identity::windows::tests::swapped_launch_path_child",
                    "--nocapture",
                ])
                .env(CHILD_MODE, "child")
                .env(PRISTINE, &pristine)
                .status()
                .expect("start the copied process");
            let _ = std::fs::remove_dir_all(&directory);
            assert!(
                status.success(),
                "the child opened the wrong file: {status}"
            );
        }

        #[test]
        fn swapped_launch_path_child() {
            if std::env::var_os(CHILD_MODE).is_none() {
                return;
            }
            let pristine = std::fs::read(std::env::var_os(PRISTINE).expect("pristine path"))
                .expect("read the original bytes");
            let launched = std::env::current_exe().expect("launch path");
            let moved = launched.with_file_name("moved.exe");
            std::fs::rename(&launched, &moved).expect("a running image can be renamed");
            std::fs::write(&launched, b"imposter: not the running image")
                .expect("place an imposter at the launch path");

            let mut executable = open_running_executable(anchor)
                .expect("the running image is still found, by what is mapped");
            let bytes = read_all(&mut executable);
            assert_ne!(bytes, b"imposter: not the running image");
            assert_eq!(bytes, pristine, "the bytes are the running image's own");
        }

        /// A minimal PE32+ header block: DOS header, `PE` signature, file header, and an
        /// optional header with the given image base and size fields.
        fn forged_header(image_base: u64, size_of_headers: u32, time_stamp: u32) -> Vec<u8> {
            let nt = 0x80_usize;
            let optional_size = 0xf0_u16;
            let mut bytes = vec![0_u8; size_of_headers as usize];
            bytes[..2].copy_from_slice(b"MZ");
            bytes[0x3c..0x40].copy_from_slice(&(nt as u32).to_le_bytes());
            bytes[nt..nt + 4].copy_from_slice(b"PE\0\0");
            bytes[nt + 8..nt + 12].copy_from_slice(&time_stamp.to_le_bytes());
            bytes[nt + 20..nt + 22].copy_from_slice(&optional_size.to_le_bytes());
            let optional = nt + 24;
            bytes[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
            bytes[optional + 24..optional + 32].copy_from_slice(&image_base.to_le_bytes());
            bytes[optional + 56..optional + 60].copy_from_slice(&0x2000_u32.to_le_bytes());
            bytes[optional + 60..optional + 64].copy_from_slice(&size_of_headers.to_le_bytes());
            bytes
        }

        #[test]
        fn a_relocated_image_matches_its_file_but_a_different_build_does_not() {
            let on_disk = forged_header(0x1_4000_0000, 0x400, 7);
            // The loader rewrote ImageBase when it relocated the image.
            let relocated = forged_header(0x7ff6_1234_0000, 0x400, 7);
            assert!(headers_match(&relocated, &on_disk).expect("well-formed"));
            // A rebuilt executable has another timestamp (or checksum, entry point, sections).
            let rebuilt = forged_header(0x7ff6_1234_0000, 0x400, 8);
            assert!(!headers_match(&rebuilt, &on_disk).expect("well-formed"));
            // A header block of another length is another executable.
            let longer = forged_header(0x1_4000_0000, 0x600, 7);
            assert!(!headers_match(&longer, &on_disk).expect("well-formed"));
        }

        #[test]
        fn the_layout_names_the_image_base_and_the_header_block() {
            let layout = parse_pe_layout(&forged_header(1, 0x400, 0), false).expect("layout");
            assert_eq!(
                layout,
                PeLayout {
                    size_of_headers: 0x400,
                    image_base: 0x80 + 24 + 24..0x80 + 24 + 32,
                }
            );
        }

        #[test]
        fn malformed_header_blocks_are_refused() {
            let good = forged_header(1, 0x400, 0);
            let mut cases = Vec::<(&str, Vec<u8>)>::new();
            cases.push(("empty", Vec::new()));
            cases.push(("not a DOS header", vec![0_u8; 0x400]));
            let mut bad = good.clone();
            bad[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
            cases.push(("e_lfanew past the block", bad));
            let mut bad = good.clone();
            bad[0x80..0x84].copy_from_slice(b"NE\0\0");
            cases.push(("not a PE signature", bad));
            let mut bad = good.clone();
            bad[0x80 + 24..0x80 + 26].copy_from_slice(&0x107_u16.to_le_bytes());
            cases.push(("a ROM image", bad));
            let mut bad = good.clone();
            bad[0x80 + 24 + 60..0x80 + 24 + 64].copy_from_slice(&0x10_u32.to_le_bytes());
            cases.push(("SizeOfHeaders inside the headers", bad));
            let mut bad = good.clone();
            bad[0x80 + 24 + 60..0x80 + 24 + 64].copy_from_slice(&0x4000_u32.to_le_bytes());
            cases.push(("SizeOfHeaders beyond SizeOfImage", bad));
            let mut bad = good.clone();
            bad[0x80 + 24 + 56..0x80 + 24 + 60].copy_from_slice(&0x10_0000_u32.to_le_bytes());
            bad[0x80 + 24 + 60..0x80 + 24 + 64].copy_from_slice(&0x2_0000_u32.to_le_bytes());
            cases.push(("SizeOfHeaders beyond the read bound", bad));
            cases.push(("block shorter than SizeOfHeaders", good[..0x200].to_vec()));
            for (label, bytes) in cases {
                assert!(parse_pe_layout(&bytes, false).is_err(), "{label}");
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{FileStamp, RunningExecutable, invalid};
    use crate::directory::DirectoryCapability;
    use std::ffi::{c_char, c_int, c_void};
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::FileExt as _;
    use std::ptr;

    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const MH_EXECUTE: u32 = 2;
    const LC_UUID: u32 = 0x1b;
    const FAT_MAGIC: u32 = 0xcafe_babe;
    const FAT_CIGAM: u32 = 0xbeba_feca;
    const FAT_MAGIC_64: u32 = 0xcafe_babf;
    const FAT_CIGAM_64: u32 = 0xbfba_feca;
    const CPU_SUBTYPE_MASK: u32 = 0xff00_0000;
    const MAX_FAT_ARCHES: u32 = 128;
    const MAX_LOAD_COMMANDS: u32 = 16_384;
    const MAX_LOAD_COMMAND_BYTES: usize = 1 << 20;

    #[repr(C)]
    struct DlInfo {
        file_name: *const c_char,
        file_base: *mut c_void,
        symbol_name: *const c_char,
        symbol_address: *mut c_void,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MachHeader64 {
        magic: u32,
        cpu_type: i32,
        cpu_subtype: i32,
        file_type: u32,
        command_count: u32,
        command_bytes: u32,
        flags: u32,
        reserved: u32,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ImageIdentity {
        cpu_type: u32,
        cpu_subtype: u32,
        uuid: [u8; 16],
    }

    #[link(name = "System")]
    unsafe extern "C" {
        fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
    }

    pub(super) fn open_running_executable(anchor: fn()) -> io::Result<RunningExecutable> {
        let path = std::env::current_exe()?;
        let parent = path
            .parent()
            .ok_or_else(|| invalid("running executable has no parent"))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("running executable has no Unicode name"))?;
        let directory = DirectoryCapability::open_read_only_source(parent)?;
        let file = directory.open_file_read(name)?;
        let stamp = FileStamp::capture(&file)?;
        if !loaded_main_image_matches(&file, anchor)? {
            return Err(invalid(
                "current executable path does not identify the loaded main image",
            ));
        }
        stamp.verify(&file)?;
        Ok(RunningExecutable { file, stamp })
    }

    fn loaded_main_image_matches(file: &File, anchor: fn()) -> io::Result<bool> {
        let mut info = DlInfo {
            file_name: ptr::null(),
            file_base: ptr::null_mut(),
            symbol_name: ptr::null(),
            symbol_address: ptr::null_mut(),
        };
        let address = anchor as *const () as *const c_void;
        // SAFETY: `address` is a live function pointer supplied by the caller;
        // `info` is a valid writable `DlInfo`. The returned image base is used
        // only to read the Mach-O header dyld mapped for that address.
        if unsafe { dladdr(address, &mut info) } == 0 || info.file_base.is_null() {
            return Err(invalid("dyld could not identify the main-image anchor"));
        }
        // SAFETY: `dladdr` returned the mapped Mach-O image base containing
        // the live anchor. Its load commands are loader-validated; our parser
        // still bounds command count, byte count, and each command size.
        let loaded = unsafe { loaded_image_identity(info.file_base.cast())? };
        let on_disk = disk_image_identity(file, loaded)?;
        Ok(loaded == on_disk)
    }

    unsafe fn loaded_image_identity(base: *const u8) -> io::Result<ImageIdentity> {
        // SAFETY: caller provides dyld's base for the image containing its
        // live function pointer. A complete Mach-O header is mapped there.
        let header = unsafe { ptr::read_unaligned(base.cast::<MachHeader64>()) };
        if header.magic != MH_MAGIC_64 || header.file_type != MH_EXECUTE {
            return Err(invalid("anchor is not in a 64-bit main executable image"));
        }
        let command_bytes = usize::try_from(header.command_bytes)
            .map_err(|_| invalid("loaded Mach-O command size overflow"))?;
        if command_bytes > MAX_LOAD_COMMAND_BYTES {
            return Err(invalid("loaded Mach-O command table exceeds its bound"));
        }
        // SAFETY: `base` is dyld's loaded main-image header, and the header's
        // bounded `sizeofcmds` region is the loader-validated command table.
        // The shared parser treats the resulting bytes as untrusted anyway.
        let commands = unsafe {
            std::slice::from_raw_parts(base.add(std::mem::size_of::<MachHeader64>()), command_bytes)
        };
        parse_load_commands(
            header.cpu_type as u32,
            header.cpu_subtype as u32,
            header.file_type,
            header.command_count,
            commands,
        )
    }

    fn disk_image_identity(file: &File, loaded: ImageIdentity) -> io::Result<ImageIdentity> {
        let length = file.metadata()?.len();
        let mut magic_bytes = [0_u8; 4];
        read_exact_at(file, &mut magic_bytes, 0)?;
        let big_endian_magic = u32::from_be_bytes(magic_bytes);
        match big_endian_magic {
            FAT_MAGIC => disk_fat_identity(file, length, loaded, true, false),
            FAT_CIGAM => disk_fat_identity(file, length, loaded, false, false),
            FAT_MAGIC_64 => disk_fat_identity(file, length, loaded, true, true),
            FAT_CIGAM_64 => disk_fat_identity(file, length, loaded, false, true),
            _ if u32::from_le_bytes(magic_bytes) == MH_MAGIC_64 => {
                disk_thin_identity(file, 0, length, loaded)
            }
            _ => Err(invalid("current executable is not a supported Mach-O")),
        }
    }

    fn disk_fat_identity(
        file: &File,
        file_length: u64,
        loaded: ImageIdentity,
        big_endian: bool,
        wide: bool,
    ) -> io::Result<ImageIdentity> {
        let mut header = [0_u8; 8];
        read_exact_at(file, &mut header, 0)?;
        let architecture_count = word(&header[4..8], big_endian);
        if architecture_count == 0 || architecture_count > MAX_FAT_ARCHES {
            return Err(invalid("fat Mach-O architecture count exceeds its bound"));
        }
        let entry_size = if wide { 32_u64 } else { 20_u64 };
        let table_end = 8_u64
            .checked_add(
                u64::from(architecture_count)
                    .checked_mul(entry_size)
                    .ok_or_else(|| invalid("fat Mach-O table overflow"))?,
            )
            .ok_or_else(|| invalid("fat Mach-O table overflow"))?;
        if table_end > file_length {
            return Err(invalid("truncated fat Mach-O architecture table"));
        }
        let mut selected = None;
        let mut entry = [0_u8; 32];
        for index in 0..architecture_count {
            let offset = 8 + u64::from(index) * entry_size;
            let bytes = &mut entry[..entry_size as usize];
            read_exact_at(file, bytes, offset)?;
            let cpu_type = word(&bytes[..4], big_endian);
            let cpu_subtype = word(&bytes[4..8], big_endian);
            let (slice_offset, slice_length) = if wide {
                (
                    quad(&bytes[8..16], big_endian),
                    quad(&bytes[16..24], big_endian),
                )
            } else {
                (
                    u64::from(word(&bytes[8..12], big_endian)),
                    u64::from(word(&bytes[12..16], big_endian)),
                )
            };
            let end = slice_offset
                .checked_add(slice_length)
                .ok_or_else(|| invalid("fat Mach-O slice range overflow"))?;
            if slice_offset < table_end || end > file_length || slice_length < 32 {
                return Err(invalid("fat Mach-O slice is outside the executable"));
            }
            if same_cpu(cpu_type, cpu_subtype, loaded.cpu_type, loaded.cpu_subtype) {
                if selected.replace((slice_offset, slice_length)).is_some() {
                    return Err(invalid("fat Mach-O has duplicate matching slices"));
                }
            }
        }
        let (offset, length) =
            selected.ok_or_else(|| invalid("fat Mach-O lacks the loaded architecture"))?;
        disk_thin_identity(file, offset, length, loaded)
    }

    fn disk_thin_identity(
        file: &File,
        offset: u64,
        slice_length: u64,
        loaded: ImageIdentity,
    ) -> io::Result<ImageIdentity> {
        let mut header = [0_u8; 32];
        read_exact_at(file, &mut header, offset)?;
        if u32::from_le_bytes(header[..4].try_into().expect("four-byte word")) != MH_MAGIC_64 {
            return Err(invalid(
                "loaded executable slice is not little-endian 64-bit Mach-O",
            ));
        }
        let cpu_type = u32::from_le_bytes(header[4..8].try_into().expect("four-byte word"));
        let cpu_subtype = u32::from_le_bytes(header[8..12].try_into().expect("four-byte word"));
        let file_type = u32::from_le_bytes(header[12..16].try_into().expect("four-byte word"));
        let command_count = u32::from_le_bytes(header[16..20].try_into().expect("four-byte word"));
        let command_bytes =
            u32::from_le_bytes(header[20..24].try_into().expect("four-byte word")) as usize;
        if file_type != MH_EXECUTE
            || !same_cpu(cpu_type, cpu_subtype, loaded.cpu_type, loaded.cpu_subtype)
        {
            return Err(invalid(
                "on-disk Mach-O architecture does not match loaded main image",
            ));
        }
        if command_bytes > MAX_LOAD_COMMAND_BYTES
            || command_count > MAX_LOAD_COMMANDS
            || command_count as usize > command_bytes / 8
            || 32_u64 + command_bytes as u64 > slice_length
        {
            return Err(invalid("on-disk Mach-O command table exceeds its bound"));
        }
        let mut commands = vec![0_u8; command_bytes];
        read_exact_at(file, &mut commands, offset + 32)?;
        parse_load_commands(cpu_type, cpu_subtype, file_type, command_count, &commands)
    }

    fn parse_load_commands(
        cpu_type: u32,
        cpu_subtype: u32,
        file_type: u32,
        command_count: u32,
        commands: &[u8],
    ) -> io::Result<ImageIdentity> {
        if file_type != MH_EXECUTE
            || commands.len() > MAX_LOAD_COMMAND_BYTES
            || command_count > MAX_LOAD_COMMANDS
            || command_count as usize > commands.len() / 8
        {
            return Err(invalid("Mach-O command table exceeds its bound"));
        }
        let mut cursor = 0_usize;
        let mut uuid = None;
        for _ in 0..command_count {
            let prefix = commands
                .get(cursor..cursor.saturating_add(8))
                .ok_or_else(|| invalid("truncated Mach-O load command"))?;
            let command = u32::from_le_bytes(prefix[..4].try_into().expect("four-byte word"));
            let size =
                u32::from_le_bytes(prefix[4..8].try_into().expect("four-byte word")) as usize;
            let end = cursor
                .checked_add(size)
                .ok_or_else(|| invalid("Mach-O command range overflow"))?;
            if size < 8 || end > commands.len() {
                return Err(invalid("invalid Mach-O load command size"));
            }
            if command == LC_UUID {
                if size != 24 || uuid.is_some() {
                    return Err(invalid("invalid or duplicate Mach-O LC_UUID"));
                }
                let mut bytes = [0_u8; 16];
                bytes.copy_from_slice(&commands[cursor + 8..cursor + 24]);
                uuid = Some(bytes);
            }
            cursor = end;
        }
        if cursor != commands.len() {
            return Err(invalid("Mach-O command table has trailing bytes"));
        }
        Ok(ImageIdentity {
            cpu_type,
            cpu_subtype,
            uuid: uuid.ok_or_else(|| invalid("main image has no LC_UUID"))?,
        })
    }

    fn same_cpu(left_type: u32, left_subtype: u32, right_type: u32, right_subtype: u32) -> bool {
        left_type == right_type
            && (left_subtype & !CPU_SUBTYPE_MASK) == (right_subtype & !CPU_SUBTYPE_MASK)
    }

    fn word(bytes: &[u8], big_endian: bool) -> u32 {
        let bytes: [u8; 4] = bytes.try_into().expect("four-byte word");
        if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        }
    }

    fn quad(bytes: &[u8], big_endian: bool) -> u64 {
        let bytes: [u8; 8] = bytes.try_into().expect("eight-byte word");
        if big_endian {
            u64::from_be_bytes(bytes)
        } else {
            u64::from_le_bytes(bytes)
        }
    }

    fn read_exact_at(file: &File, mut destination: &mut [u8], mut offset: u64) -> io::Result<()> {
        while !destination.is_empty() {
            let count = file.read_at(destination, offset)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated Mach-O image",
                ));
            }
            offset = offset
                .checked_add(count as u64)
                .ok_or_else(|| invalid("Mach-O file offset overflow"))?;
            destination = &mut destination[count..];
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::{Read as _, Write as _};
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        fn test_main_image_anchor() {}

        const CHILD_MODE: &str = "NUDOX_TEST_REPLACEMENT_AFTER_EXEC";

        #[test]
        fn current_executable_file_matches_the_loaded_main_image() {
            let mut executable =
                open_running_executable(test_main_image_anchor).expect("verified running image");
            executable
                .with_verified_read(|file| file.metadata().map(|metadata| metadata.len()))
                .expect("stable executable read");
        }

        #[test]
        fn held_image_stamp_rejects_change_before_the_digest_read_starts() {
            let directory = test_directory("stamp");
            let path = directory.join("image");
            std::fs::write(&path, b"original image").expect("write initial image");
            let file = File::open(&path).expect("open held image");
            let mut running = RunningExecutable::new(file).expect("capture initial stamp");

            // This is the race window between loaded-image verification and
            // the caller's first digest metadata read. The held descriptor
            // stays the same, but its in-place contents and ctime change.
            std::fs::write(&path, b"replacement image").expect("replace held contents");
            let error = running
                .with_verified_read(|file| file.metadata().map(|metadata| metadata.len()))
                .expect_err("changed held image must not be fingerprinted");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }

        #[test]
        fn replacement_after_process_start_refuses_the_new_path_image() {
            if std::env::var_os(CHILD_MODE).is_some() {
                return;
            }
            let source = std::env::current_exe().expect("test executable");
            let directory = test_directory("replacement");
            let _directory_guard = DirectoryGuard(directory.clone());
            let running_path = directory.join("running-test");
            std::fs::copy(&source, &running_path).expect("copy test executable");
            let ready_file = directory.join("ready");
            let child = Command::new(&running_path)
                .args([
                    "--exact",
                    "executable_identity::macos::tests::replacement_child",
                    "--nocapture",
                ])
                .env(CHILD_MODE, "child")
                .env("NUDOX_TEST_REPLACEMENT_READY", &ready_file)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start copied process");
            let mut child_guard = ChildGuard(Some(child));
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if ready_file.exists() {
                    break;
                }
                if let Some(status) = child_guard
                    .0
                    .as_mut()
                    .expect("child")
                    .try_wait()
                    .expect("check child")
                {
                    panic!("child exited before ready marker: {status}");
                }
                assert!(Instant::now() < deadline, "child ready wait timed out");
                std::thread::sleep(Duration::from_millis(10));
            }

            // `/usr/bin/true` is a system Mach-O with a different main-image
            // UUID. Copy then rename on the same volume to model app-bundle
            // replacement while the child still has the original image mapped.
            let replacement = directory.join("replacement");
            std::fs::copy("/usr/bin/true", &replacement).expect("copy replacement Mach-O");
            std::fs::rename(&replacement, &running_path).expect("atomically replace launch path");
            child_guard
                .0
                .as_mut()
                .expect("child")
                .stdin
                .as_mut()
                .expect("child stdin")
                .write_all(b"go")
                .expect("release child");
            let status = child_guard.wait_timeout(Duration::from_secs(10));
            assert!(status.success(), "child rejected replaced current_exe path");
        }

        #[test]
        fn replacement_child() {
            if std::env::var_os(CHILD_MODE).is_none() {
                return;
            }
            let ready_file = std::env::var_os("NUDOX_TEST_REPLACEMENT_READY")
                .expect("parent supplies ready marker path");
            std::fs::write(ready_file, b"ready").expect("write ready marker");
            let mut release = [0_u8; 2];
            std::io::stdin()
                .read_exact(&mut release)
                .expect("parent replacement signal");
            assert!(
                open_running_executable(replacement_child).is_err(),
                "the mapped test process must not adopt the replacement file's LC_UUID"
            );
        }

        struct ChildGuard(Option<std::process::Child>);

        impl ChildGuard {
            fn wait_timeout(&mut self, timeout: Duration) -> std::process::ExitStatus {
                let deadline = Instant::now() + timeout;
                loop {
                    if let Some(status) = self
                        .0
                        .as_mut()
                        .expect("child")
                        .try_wait()
                        .expect("check child")
                    {
                        self.0.take();
                        return status;
                    }
                    assert!(Instant::now() < deadline, "child exit wait timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }

        impl Drop for ChildGuard {
            fn drop(&mut self) {
                if let Some(mut child) = self.0.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }

        struct DirectoryGuard(std::path::PathBuf);

        impl Drop for DirectoryGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn test_directory(name: &str) -> std::path::PathBuf {
            let unique = format!(
                "nudox-image-{}-{name}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            );
            let directory = std::env::temp_dir().join(unique);
            std::fs::create_dir(&directory).expect("private test directory");
            directory
        }

        fn uuid_command(bytes: [u8; 16]) -> [u8; 24] {
            let mut command = [0_u8; 24];
            command[..4].copy_from_slice(&LC_UUID.to_le_bytes());
            command[4..8].copy_from_slice(&24_u32.to_le_bytes());
            command[8..].copy_from_slice(&bytes);
            command
        }

        #[test]
        fn parser_accepts_one_well_formed_uuid_command() {
            let command = uuid_command([7; 16]);
            let identity =
                parse_load_commands(1, 2, MH_EXECUTE, 1, &command).expect("valid load command");
            assert_eq!(identity.uuid, [7; 16]);
        }

        #[test]
        fn parser_rejects_truncated_load_command_prefix() {
            let error =
                parse_load_commands(1, 2, MH_EXECUTE, 1, &[0; 7]).expect_err("truncated prefix");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }

        #[test]
        fn parser_rejects_duplicate_uuid_commands() {
            let mut commands = Vec::from(uuid_command([1; 16]));
            commands.extend_from_slice(&uuid_command([2; 16]));
            let error =
                parse_load_commands(1, 2, MH_EXECUTE, 2, &commands).expect_err("duplicate UUID");
            assert!(error.to_string().contains("duplicate"));
        }

        #[test]
        fn parser_rejects_invalid_and_trailing_command_bytes() {
            let mut invalid_size = uuid_command([3; 16]);
            invalid_size[4..8].copy_from_slice(&4_u32.to_le_bytes());
            assert!(parse_load_commands(1, 2, MH_EXECUTE, 1, &invalid_size).is_err());

            let mut trailing = Vec::from(uuid_command([4; 16]));
            trailing.push(0);
            assert!(parse_load_commands(1, 2, MH_EXECUTE, 1, &trailing).is_err());
        }
    }
}
