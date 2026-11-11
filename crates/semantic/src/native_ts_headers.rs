//! Canonical, untrusted native TypeScript compiler header claims.
//! Authority requires the selected compiler closure witness and exact source/image join.
use crate::ir::{DeclarationFamilyId, DeclarationIdentity, SourceIdentity, VariantFingerprint};
use backend_version::{ContentId, Schema, SourceFactDomain};

pub const MAX_NATIVE_TS_HEADER_AGGREGATE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_NATIVE_TS_HEADERS_PER_FILE: usize = 16_384;
pub const MAX_NATIVE_TS_HEADER_FILES: usize = 65_536;
const ENTRY_BYTES: usize = 52;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeTsHeaderEntry {
    pub entity_id: u32,
    pub identity: DeclarationIdentity,
    pub compiler_extent: (u32, u32),
    pub header_extent: (u32, u32),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeTsHeaderFile {
    pub path: String,
    pub source: SourceIdentity,
    pub image_identity: [u8; 32],
    pub image_byte_len: u64,
    pub producer_recipe: [u8; 32],
    pub entries: Vec<NativeTsHeaderEntry>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeTsHeaderAggregate { pub files: Vec<NativeTsHeaderFile> }
pub struct NativeTsHeaderAggregateSchema;
impl Schema for NativeTsHeaderAggregateSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc00e;
    const VERSION: u8 = 1;
    type Value = NativeTsHeaderAggregate;
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(b"NHA1");
        out.extend_from_slice(&(value.files.len() as u32).to_be_bytes());
        for file in &value.files {
            out.extend_from_slice(&(file.path.len() as u16).to_be_bytes());
            out.extend_from_slice(file.path.as_bytes());
            out.extend_from_slice(file.source.identity.as_ref());
            out.extend_from_slice(&file.source.byte_len.to_be_bytes());
            out.extend_from_slice(&file.image_identity);
            out.extend_from_slice(&file.image_byte_len.to_be_bytes());
            out.extend_from_slice(&file.producer_recipe);
            out.extend_from_slice(&(file.entries.len() as u32).to_be_bytes());
            for e in &file.entries {
                out.extend_from_slice(&e.entity_id.to_be_bytes());
                out.extend_from_slice(e.identity.family.as_bytes());
                out.extend_from_slice(e.identity.variant.as_bytes());
                for n in [e.compiler_extent.0,e.compiler_extent.1,e.header_extent.0,e.header_extent.1] { out.extend_from_slice(&n.to_be_bytes()); }
            }
        }
    }
}
struct Reader<'a> { bytes: &'a [u8], at: usize }
impl<'a> Reader<'a> {
    fn take(&mut self,n: usize) -> Option<&'a [u8]> { let end=self.at.checked_add(n)?; let v=self.bytes.get(self.at..end)?; self.at=end; Some(v) }
    fn u16(&mut self)->Option<u16>{Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))}
    fn u32(&mut self)->Option<u32>{Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))}
    fn u64(&mut self)->Option<u64>{Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))}
}
impl NativeTsHeaderAggregate {
    /// Validates canonical framing and geometry before allocating any file or entry vectors.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, &'static str> {
        fn parse(bytes: &[u8], materialize: bool) -> Option<NativeTsHeaderAggregate> {
            if bytes.len() > MAX_NATIVE_TS_HEADER_AGGREGATE_BYTES { return None; }
            let mut r=Reader {bytes,at:0}; if r.take(4)? != b"NHA1" {return None;}
            let count=r.u32()? as usize;
            if count > MAX_NATIVE_TS_HEADER_FILES || count > bytes.len().saturating_sub(8)/114 {return None;}
            let mut files=if materialize {Vec::with_capacity(count)} else {Vec::new()};
            let mut previous: Option<&str>=None;
            for _ in 0..count {
                let n=r.u16()? as usize; let path=std::str::from_utf8(r.take(n)?).ok()?;
                if n==0 || n>4096 || path.starts_with('/') || path.contains('\\') || path.bytes().any(|b| b.is_ascii_control()) || path.split('/').any(|p| p.is_empty() || p=="." || p=="..") || previous.is_some_and(|p| p>=path) {return None;}
                previous=Some(path);
                let source=SourceIdentity {identity:ContentId::<SourceFactDomain>::try_from(<[u8;32]>::try_from(r.take(32)?).ok()?).ok()?, byte_len:r.u32()?};
                if source.byte_len > 64*1024*1024 {return None;}
                let image_identity=r.take(32)?.try_into().ok()?; let image_byte_len=r.u64()?; let producer_recipe=r.take(32)?.try_into().ok()?;
                let entries_count=r.u32()? as usize;
                if entries_count>MAX_NATIVE_TS_HEADERS_PER_FILE || entries_count.checked_mul(ENTRY_BYTES)? > bytes.len().saturating_sub(r.at) {return None;}
                let mut entries=if materialize {Vec::with_capacity(entries_count)} else {Vec::new()};
                let mut prior=None;
                for _ in 0..entries_count {
                    let entity_id=r.u32()?; if prior.is_some_and(|p| p>=entity_id){return None;} prior=Some(entity_id);
                    let family=DeclarationFamilyId::from_raw(r.take(16)?.try_into().ok()?); let variant=VariantFingerprint::from_raw(r.take(16)?.try_into().ok()?);
                    let start=r.u32()?; let end=r.u32()?; let hs=r.u32()?; let he=r.u32()?;
                    if start>=end || end>source.byte_len || hs>start || start>=he || he>end {return None;}
                    if materialize {entries.push(NativeTsHeaderEntry {entity_id,identity:DeclarationIdentity{family,variant},compiler_extent:(start,end),header_extent:(hs,he)});}
                }
                if materialize {files.push(NativeTsHeaderFile {path:path.to_owned(),source,image_identity,image_byte_len,producer_recipe,entries});}
            }
            if r.at != bytes.len(){return None;} Some(NativeTsHeaderAggregate{files})
        }
        parse(bytes,false).ok_or("invalid native header aggregate")?;
        parse(bytes,true).ok_or("invalid native header aggregate")
    }
    /// Validates a producer value and its exact canonical encoded size.
    pub fn encode_checked(&self) -> Result<Vec<u8>, &'static str> {
        let mut size=8usize;
        for f in &self.files { size=size.checked_add(114).and_then(|n|n.checked_add(f.path.len())).and_then(|n|n.checked_add(f.entries.len().checked_mul(ENTRY_BYTES)?)).ok_or("native header aggregate overflow")?; }
        if size>MAX_NATIVE_TS_HEADER_AGGREGATE_BYTES {return Err("native header aggregate too large");}
        let mut bytes=Vec::with_capacity(size); NativeTsHeaderAggregateSchema::encode(self,&mut bytes);
        Self::decode_canonical(&bytes)?; Ok(bytes)
    }
}
