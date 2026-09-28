use super::config::origin_authority;
use super::*;

impl S3ClosurePublisher {
    pub(super) fn presign(
        &self,
        method: &str,
        path: &str,
        date: &str,
        expires_seconds: u64,
        credential: &str,
        scope: &str,
        signed_headers: &[(&str, String)],
        signed_headers_value: &str,
    ) -> Result<String, PublicationError> {
        let authority = origin_authority(&self.endpoint)?;
        let mut query = BTreeMap::from([
            ("X-Amz-Algorithm".to_owned(), "AWS4-HMAC-SHA256".to_owned()),
            ("X-Amz-Credential".to_owned(), credential.to_owned()),
            ("X-Amz-Date".to_owned(), date.to_owned()),
            ("X-Amz-Expires".to_owned(), expires_seconds.to_string()),
            (
                "X-Amz-SignedHeaders".to_owned(),
                signed_headers_value.to_owned(),
            ),
        ]);
        if let Some(token) = &self.session_token {
            query.insert("X-Amz-Security-Token".to_owned(), token.clone());
        }
        let canonical_query_string = canonical_query(&query);
        let canonical_headers = signed_headers
            .iter()
            .map(|(name, value)| {
                if *name == "host" {
                    format!("host:{authority}\n")
                } else {
                    format!("{name}:{}\n", value.trim())
                }
            })
            .collect::<String>();
        let canonical_request = format!(
            "{method}\n{path}\n{canonical_query_string}\n{canonical_headers}\n{signed_headers_value}\nUNSIGNED-PAYLOAD"
        );
        let request_hash = hex(&Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{request_hash}");
        let signing_key = signing_key(&self.secret_key, &date[..8], &self.region)?;
        let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
        let origin = self.endpoint.trim_end_matches('/');
        // The signature is calculated over the sorted unsigned query above.
        // AWS presigned URLs then append it as the final parameter; including
        // it in canonical_query would sort it before SignedHeaders and change
        // the conventional wire representation (though not the signature).
        Ok(format!(
            "{origin}{path}?{}&X-Amz-Signature={signature}",
            canonical_query(&query)
        ))
    }
}

pub(super) fn canonical_query(query: &BTreeMap<String, String>) -> String {
    query
        .iter()
        .map(|(name, value)| format!("{}={}", aws_encode(name), aws_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

pub(super) fn aws_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn signing_key(secret: &str, date: &str, region: &str) -> Result<[u8; 32], PublicationError> {
    let date_key = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes())?;
    let region_key = hmac_sha256(&date_key, region.as_bytes())?;
    let service_key = hmac_sha256(&region_key, b"s3")?;
    hmac_sha256(&service_key, b"aws4_request")
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Result<[u8; 32], PublicationError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| PublicationError::Configuration)?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().into())
}

pub(super) fn aws_timestamp(unix_seconds: u64) -> String {
    let days = i64::try_from(unix_seconds / 86_400).unwrap_or(i64::MAX);
    let seconds = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60,
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch.saturating_add(719_468);
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}
