use nudox_id::ObjectDomain;
use nudox_root::LocalityError;
#[cfg(feature = "locality-simd-size")]
use nudox_root::LocalityValidator;
#[cfg(not(feature = "locality-simd-size"))]
use nudox_root::ValidatedLocality;

const ROOT_DOMAIN_CODE: u8 = 2;
const OBJECT_DOMAIN_CODE: u8 = 1;
const EXCEPTION_COUNT: u32 = 64;
const FIXTURE_BYTES: usize = 353;
const GENERATION_DOMAIN_OFFSET: usize = 0;
const CONTENT_DOMAIN_OFFSET: usize = 32;
const ROOT_COUNT_LOW_BYTE_OFFSET: usize = 36;
const EXCEPTION_COUNT_LOW_BYTE_OFFSET: usize = 40;
const EXCEPTION_ROWS_OFFSET: usize = 49;
const OVERLAY_BASIS_DOMAIN_OFFSET: usize = 321;
const ROW_BYTES: usize = 4;
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
    bytes[GENERATION_DOMAIN_OFFSET] = ROOT_DOMAIN_CODE;
    bytes[CONTENT_DOMAIN_OFFSET] = OBJECT_DOMAIN_CODE;
    let exception_count = EXCEPTION_COUNT.to_be_bytes();
    bytes[ROOT_COUNT_LOW_BYTE_OFFSET] = exception_count[ROW_BYTES - 1];
    bytes[EXCEPTION_COUNT_LOW_BYTE_OFFSET] = exception_count[ROW_BYTES - 1];
    bytes[OVERLAY_BASIS_DOMAIN_OFFSET] = ROOT_DOMAIN_CODE;
    let mut row = 0_u32;
    while row < EXCEPTION_COUNT {
        let offset = EXCEPTION_ROWS_OFFSET + row as usize * ROW_BYTES;
        let encoded = row.to_be_bytes();
        bytes[offset] = encoded[0];
        bytes[offset + 1] = encoded[1];
        bytes[offset + 2] = encoded[2];
        bytes[offset + 3] = encoded[3];
        row += 1;
    }
    bytes
}
