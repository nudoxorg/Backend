use compiler_languages_java::jar::{Jar, JarError};

fn put(v: &mut Vec<u8>, n: u32) {
    v.extend(n.to_le_bytes());
}
fn zip(name: &[u8], body: &[u8], method: u16) -> Vec<u8> {
    let mut z = Vec::new();
    let crc = crc(body);
    let local = 0u32;
    put(&mut z, 0x0403_4b50);
    z.extend([20, 0, 0, 0]);
    z.extend(method.to_le_bytes());
    z.extend([0; 4]);
    put(&mut z, crc);
    put(&mut z, body.len() as u32);
    put(&mut z, body.len() as u32);
    z.extend((name.len() as u16).to_le_bytes());
    z.extend([0, 0]);
    z.extend(name);
    z.extend(body);
    let cd = z.len() as u32;
    put(&mut z, 0x0201_4b50);
    z.extend([20, 0, 20, 0, 0, 0]);
    z.extend(method.to_le_bytes());
    z.extend([0; 4]);
    put(&mut z, crc);
    put(&mut z, body.len() as u32);
    put(&mut z, body.len() as u32);
    z.extend((name.len() as u16).to_le_bytes());
    z.extend([0; 12]);
    put(&mut z, local);
    z.extend(name);
    put(&mut z, 0x0605_4b50);
    z.extend([0; 4]);
    z.extend([1, 0, 1, 0]);
    let directory_size = 46 + name.len() as u32;
    put(&mut z, directory_size);
    put(&mut z, cd);
    z.extend([0, 0]);
    z
}
fn crc(bytes: &[u8]) -> u32 {
    let mut c = !0;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                c >> 1 ^ 0xedb8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

#[test]
fn stored_entry_round_trips_and_safe_paths_are_checked() {
    let bytes = zip(b"META-INF/MANIFEST.MF", b"hello", 0);
    let jar = Jar::parse(&bytes).unwrap();
    let entry = jar.entries().next().unwrap().unwrap();
    let mut out = Vec::new();
    assert_eq!(entry.data(&mut out).unwrap().as_ref(), b"hello");
    assert!(entry.is_safe_relative_path());
    let unsafe_bytes = zip(b"../evil", b"x", 0);
    let unsafe_jar = Jar::parse(&unsafe_bytes).unwrap();
    let unsafe_entry = unsafe_jar.entries().next().unwrap().unwrap();
    assert!(!unsafe_entry.is_safe_relative_path());
}

#[test]
fn missing_eocd_and_unknown_method_are_typed() {
    assert!(matches!(
        Jar::parse(b"not zip"),
        Err(JarError::EocdMissing { .. })
    ));
    let mut bytes = zip(b"x", b"x", 0);
    let cd =
        u32::from_le_bytes(bytes[bytes.len() - 6..bytes.len() - 2].try_into().unwrap()) as usize;
    let central_method = cd + 10;
    bytes[central_method] = 99;
    assert!(matches!(
        Jar::parse(&bytes),
        Err(JarError::UnknownCompression { method: 99, .. })
    ));
}

#[test]
fn crc_corruption_is_rejected() {
    let mut bytes = zip(b"x", b"x", 0);
    let data = 31;
    bytes[data] = b'y';
    let jar = Jar::parse(&bytes).unwrap();
    let entry = jar.entries().next().unwrap().unwrap();
    assert!(matches!(
        entry.data(&mut Vec::new()),
        Err(JarError::CrcMismatch { .. })
    ));
}
