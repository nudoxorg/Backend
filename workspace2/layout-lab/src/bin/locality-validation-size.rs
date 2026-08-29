use nudox_id::ObjectDomain;
use nudox_root::LocalityError;
#[cfg(feature = "locality-simd-size")]
use nudox_root::LocalityValidator;
#[cfg(not(feature = "locality-simd-size"))]
use nudox_root::ValidatedLocality;

const FIXTURE_BYTES: usize = 352;
const FIXTURE: [u8; FIXTURE_BYTES] = fixture();

fn main() -> Result<(), LocalityError> {
    let bytes = std::hint::black_box(&FIXTURE);
    #[cfg(feature = "locality-simd-size")]
    let result = LocalityValidator::new().validate::<ObjectDomain>(bytes);
    #[cfg(not(feature = "locality-simd-size"))]
    let result = ValidatedLocality::<ObjectDomain>::try_from(bytes.as_slice());
    std::hint::black_box(result?.metadata_bytes());
    Ok(())
}

const fn fixture() -> [u8; FIXTURE_BYTES] {
    let mut bytes = [0_u8; FIXTURE_BYTES];
    bytes[35] = 64;
    bytes[39] = 64;
    let mut row = 0_u32;
    while row < 64 {
        let offset = 48 + row as usize * 4;
        let encoded = row.to_be_bytes();
        bytes[offset] = encoded[0];
        bytes[offset + 1] = encoded[1];
        bytes[offset + 2] = encoded[2];
        bytes[offset + 3] = encoded[3];
        row += 1;
    }
    bytes
}
