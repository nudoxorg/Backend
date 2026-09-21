use super::*;

#[derive(Clone)]
pub(super) struct ArchiveFile {
    pub(super) path: Arc<str>,
    pub(super) bytes: Vec<u8>,
    pub(super) bytes_len: u64,
    pub(super) mode: u32,
}

pub(super) fn extract_archive<R: Read + Seek>(
    reader: &mut R,
    format: ForgeArchiveFormat,
    root_prefix: Option<&str>,
    budget: &ArchiveBudget,
) -> Result<Vec<ArchiveFile>, ForgeRejectReason> {
    match format {
        ForgeArchiveFormat::Tar => parse_tar_stream(reader, root_prefix, budget),
        ForgeArchiveFormat::TarGzip => {
            let mut decoded = GzDecoder::new(reader);
            parse_tar_stream(&mut decoded, root_prefix, budget)
        }
        ForgeArchiveFormat::Zip => parse_zip(reader, root_prefix, budget),
    }
}

fn normalize_path(
    path: &str,
    root_prefix: Option<&str>,
) -> Result<Option<Arc<str>>, ForgeRejectReason> {
    let path = path.trim_start_matches("./").trim_end_matches('/');
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ForgeRejectReason::Archive);
    }
    let path = if let Some(prefix) = root_prefix {
        let prefix = prefix.trim_end_matches('/');
        if prefix.is_empty()
            || prefix.starts_with('/')
            || prefix.contains('\\')
            || prefix
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(ForgeRejectReason::Archive);
        }
        let Some(path) = path.strip_prefix(prefix) else {
            return Ok(None);
        };
        let path = path.strip_prefix('/').ok_or(ForgeRejectReason::Archive)?;
        path
    } else {
        path
    };
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(Arc::from(path)))
}

fn parse_tar_stream<R: Read>(
    reader: &mut R,
    root_prefix: Option<&str>,
    budget: &ArchiveBudget,
) -> Result<Vec<ArchiveFile>, ForgeRejectReason> {
    let mut files = Vec::new();
    let mut total = 0u64;
    loop {
        let mut header = [0_u8; 512];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(ForgeRejectReason::Archive);
            }
            Err(_) => return Err(ForgeRejectReason::Archive),
        }
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let name = tar_field(&header[..100])?;
        let mode = tar_octal(&header[100..108])
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        let size = tar_octal(&header[124..136]).ok_or(ForgeRejectReason::Archive)?;
        let kind = header[156];
        if size > budget.max_entry_bytes {
            return Err(ForgeRejectReason::Bounds);
        }
        let selected_path = normalize_path(&name, root_prefix)?;
        let mut content = Vec::new();
        if kind == b'0' || kind == 0 {
            if selected_path.is_some() {
                total = total.checked_add(size).ok_or(ForgeRejectReason::Bounds)?;
                if total > budget.max_bytes || files.len() >= budget.max_entries {
                    return Err(ForgeRejectReason::Bounds);
                }
                content.reserve(usize::try_from(size).map_err(|_| ForgeRejectReason::Bounds)?);
            }
        } else if !matches!(kind, b'5' | b'x' | b'g') {
            return Err(ForgeRejectReason::Archive);
        }
        if kind == b'0' || kind == 0 {
            reader
                .take(size)
                .read_to_end(&mut content)
                .map_err(|_| ForgeRejectReason::Archive)?;
            if u64::try_from(content.len()).unwrap_or(u64::MAX) != size {
                return Err(ForgeRejectReason::Archive);
            }
            if let Some(path) = selected_path {
                files.push(ArchiveFile {
                    path,
                    bytes: content,
                    bytes_len: size,
                    mode,
                });
            }
        } else {
            io::copy(&mut reader.take(size), &mut io::sink())
                .map_err(|_| ForgeRejectReason::Archive)?;
        }
        let padding = (512 - (usize::try_from(size).unwrap_or(0) % 512)) % 512;
        io::copy(
            &mut reader.take(u64::try_from(padding).unwrap_or(0)),
            &mut io::sink(),
        )
        .map_err(|_| ForgeRejectReason::Archive)?;
    }
    if files.is_empty() {
        return Err(ForgeRejectReason::Archive);
    }
    Ok(files)
}

fn tar_field(bytes: &[u8]) -> Result<String, ForgeRejectReason> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8(bytes[..end].to_vec()).map_err(|_| ForgeRejectReason::Archive)
}

fn tar_octal(bytes: &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    let mut digits = 0_u8;
    for byte in bytes.iter().copied() {
        if byte == 0 || (byte == b' ' && digits > 0) {
            break;
        }
        if byte == b' ' {
            continue;
        }
        if !(b'0'..=b'7').contains(&byte) {
            return None;
        }
        value = value.checked_mul(8)?.checked_add(u64::from(byte - b'0'))?;
        digits = digits.saturating_add(1);
    }
    if digits == 0 { Some(0) } else { Some(value) }
}

fn parse_zip<R: Read + Seek>(
    reader: &mut R,
    root_prefix: Option<&str>,
    budget: &ArchiveBudget,
) -> Result<Vec<ArchiveFile>, ForgeRejectReason> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| ForgeRejectReason::Archive)?;
    let mut archive = zip::ZipArchive::new(reader).map_err(|_| ForgeRejectReason::Archive)?;
    if archive.len() > budget.max_entries {
        return Err(ForgeRejectReason::Bounds);
    }
    let mut files = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| ForgeRejectReason::Archive)?;
        if file.is_dir() {
            continue;
        }
        let Some(path) = normalize_path(file.name(), root_prefix)? else {
            continue;
        };
        let size = file.size();
        if size > budget.max_entry_bytes {
            return Err(ForgeRejectReason::Bounds);
        }
        total = total.checked_add(size).ok_or(ForgeRejectReason::Bounds)?;
        if total > budget.max_bytes {
            return Err(ForgeRejectReason::Bounds);
        }
        let mut content = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        file.take(size.saturating_add(1))
            .read_to_end(&mut content)
            .map_err(|_| ForgeRejectReason::Archive)?;
        if u64::try_from(content.len()).unwrap_or(u64::MAX) != size {
            return Err(ForgeRejectReason::Archive);
        }
        files.push(ArchiveFile {
            path,
            bytes: content,
            bytes_len: size,
            mode: 0,
        });
    }
    if files.is_empty() {
        return Err(ForgeRejectReason::Archive);
    }
    Ok(files)
}
