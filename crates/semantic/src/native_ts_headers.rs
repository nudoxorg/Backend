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
impl NativeTsHeaderFile {
    /// Owned heap and record extent used by capture custody accounting.
    pub fn checked_retained_bytes(&self) -> Option<usize> {
        std::mem::size_of::<Self>().checked_add(self.path.capacity())?
            .checked_add(self.entries.capacity().checked_mul(std::mem::size_of::<NativeTsHeaderEntry>())?)
    }
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
fn parse(bytes: &[u8], materialize: bool) -> Option<(NativeTsHeaderAggregate, usize)> {
    if bytes.len() > MAX_NATIVE_TS_HEADER_AGGREGATE_BYTES { return None; }
    let mut r=Reader {bytes,at:0}; if r.take(4)? != b"NHA1" {return None;}
    let count=r.u32()? as usize;
    if count > MAX_NATIVE_TS_HEADER_FILES || count > bytes.len().saturating_sub(8)/114 {return None;}
    let mut retained = count.checked_mul(std::mem::size_of::<NativeTsHeaderFile>())?.checked_add(32)?;
    let mut files=if materialize {Vec::with_capacity(count)} else {Vec::new()};
    let mut previous: Option<&str>=None;
    for _ in 0..count {
        let n=r.u16()? as usize; let path=std::str::from_utf8(r.take(n)?).ok()?;
        if n==0 || n>4096 || path.starts_with('/') || path.contains('\\') || (path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic()) || path.bytes().any(|b| b.is_ascii_control()) || path.split('/').any(|p| p.is_empty() || p=="." || p=="..") || previous.is_some_and(|p| p>=path) {return None;}
        previous=Some(path);
        let source=SourceIdentity {identity:ContentId::<SourceFactDomain>::try_from(<[u8;32]>::try_from(r.take(32)?).ok()?).ok()?, byte_len:r.u32()?};
        if source.byte_len > 64*1024*1024 {return None;}
        let image_identity=r.take(32)?.try_into().ok()?; let image_byte_len=r.u64()?; let producer_recipe=r.take(32)?.try_into().ok()?;
        let entries_count=r.u32()? as usize;
        if entries_count>MAX_NATIVE_TS_HEADERS_PER_FILE || entries_count.checked_mul(ENTRY_BYTES)? > bytes.len().saturating_sub(r.at) {return None;}
        retained = retained.checked_add(n)?.checked_add(32)?.checked_add(entries_count.checked_mul(std::mem::size_of::<NativeTsHeaderEntry>())?)?.checked_add(32)?;
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
    if r.at != bytes.len(){return None;} Some((NativeTsHeaderAggregate{files}, retained))
}

/// A fully canonical-validated aggregate borrowing its original immutable wire.
/// This is a claim, not selected-closure or compiler-source authority.
#[derive(Clone, Copy)]
pub struct NativeTsHeaderAggregateView<'a> { bytes: &'a [u8], count: usize }

/// One source file's metadata and entry bytes borrowed from the aggregate.
#[derive(Clone, Copy)]
pub struct NativeTsHeaderFileView<'a> {
    pub path: &'a str,
    pub source: SourceIdentity,
    pub image_identity: [u8; 32],
    pub image_byte_len: u64,
    pub producer_recipe: [u8; 32],
    entries: &'a [u8],
}

pub struct NativeTsHeaderFileIter<'a> { reader: Reader<'a>, remaining: usize }
pub struct NativeTsHeaderEntryIter<'a> { chunks: std::slice::ChunksExact<'a, u8> }

impl<'a> NativeTsHeaderAggregateView<'a> {
    /// Validates all records, ordering, extents and framing without allocating.
    /// The caller must reserve work for the complete scan before calling.
    pub fn parse_canonical(bytes: &'a [u8]) -> Result<Self, &'static str> {
        parse(bytes, false).ok_or("invalid native header aggregate")?;
        let count = u32::from_be_bytes(bytes[4..8].try_into().map_err(|_| "invalid native header aggregate")?) as usize;
        Ok(Self { bytes, count })
    }
    pub const fn len(&self) -> usize { self.count }
    pub const fn is_empty(&self) -> bool { self.count == 0 }
    /// Sequential scan; caller accounts for each scan against its work ledger.
    pub fn files(&self) -> NativeTsHeaderFileIter<'a> {
        NativeTsHeaderFileIter { reader: Reader { bytes: self.bytes, at: 8 }, remaining: self.count }
    }
    /// Finds a path without allocating; caller accounts for the scanned bytes.
    pub fn file(&self, path: &str) -> Option<NativeTsHeaderFileView<'a>> {
        self.files().take_while(|file| file.path <= path).find(|file| file.path == path)
    }
}

impl<'a> Iterator for NativeTsHeaderFileIter<'a> {
    type Item = NativeTsHeaderFileView<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 { return None; }
        let r = &mut self.reader;
        let n = r.u16()? as usize;
        let path = std::str::from_utf8(r.take(n)?).ok()?;
        let source = SourceIdentity { identity: ContentId::<SourceFactDomain>::try_from(<[u8;32]>::try_from(r.take(32)?).ok()?).ok()?, byte_len: r.u32()? };
        let image_identity = r.take(32)?.try_into().ok()?;
        let image_byte_len = r.u64()?;
        let producer_recipe = r.take(32)?.try_into().ok()?;
        let count = r.u32()? as usize;
        let entries = r.take(count.checked_mul(ENTRY_BYTES)?)?;
        self.remaining -= 1;
        Some(NativeTsHeaderFileView { path, source, image_identity, image_byte_len, producer_recipe, entries })
    }
    fn size_hint(&self) -> (usize, Option<usize>) { (self.remaining, Some(self.remaining)) }
}
impl ExactSizeIterator for NativeTsHeaderFileIter<'_> {}

impl NativeTsHeaderFileView<'_> {
    pub fn entries(&self) -> NativeTsHeaderEntryIter<'_> { NativeTsHeaderEntryIter { chunks: self.entries.chunks_exact(ENTRY_BYTES) } }
    /// Binary lookup over the canonical sorted entry cells; returns an owned
    /// fixed-size claim without allocating.
    pub fn entry(&self, entity_id: u32) -> Option<NativeTsHeaderEntry> {
        let mut low = 0usize; let mut high = self.entries.len() / ENTRY_BYTES;
        while low < high {
            let mid = low + (high - low) / 2;
            let cell = self.entries.get(mid * ENTRY_BYTES..(mid + 1) * ENTRY_BYTES)?;
            let entry = decode_entry(cell)?;
            match entry.entity_id.cmp(&entity_id) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(entry),
            }
        }
        None
    }
}
fn decode_entry(bytes: &[u8]) -> Option<NativeTsHeaderEntry> {
    let mut r = Reader { bytes, at: 0 };
    let entity_id = r.u32()?;
    let family = DeclarationFamilyId::from_raw(r.take(16)?.try_into().ok()?);
    let variant = VariantFingerprint::from_raw(r.take(16)?.try_into().ok()?);
    let start = r.u32()?; let end = r.u32()?; let hs = r.u32()?; let he = r.u32()?;
    if r.at != bytes.len() { return None; }
    Some(NativeTsHeaderEntry { entity_id, identity: DeclarationIdentity { family, variant }, compiler_extent: (start, end), header_extent: (hs, he) })
}
impl Iterator for NativeTsHeaderEntryIter<'_> {
    type Item = NativeTsHeaderEntry;
    fn next(&mut self) -> Option<Self::Item> { decode_entry(self.chunks.next()?) }
    fn size_hint(&self) -> (usize, Option<usize>) { self.chunks.size_hint() }
}
impl ExactSizeIterator for NativeTsHeaderEntryIter<'_> {}

impl NativeTsHeaderFile {
    /// Finds an entity in a canonically validated file. This is only a claim.
    pub fn entry(&self, entity_id: u32) -> Option<&NativeTsHeaderEntry> {
        self.entries.binary_search_by_key(&entity_id, |entry| entry.entity_id).ok().map(|i| &self.entries[i])
    }
}
impl NativeTsHeaderAggregate {
    /// Finds a path in a canonically validated aggregate; no authority follows.
    pub fn file(&self, path: &str) -> Option<&NativeTsHeaderFile> {
        self.files.binary_search_by(|file| file.path.as_str().cmp(path)).ok().map(|i| &self.files[i])
    }
    /// Validates canonical framing and geometry before allocating any file or entry vectors.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, &'static str> {
        Self::decode_canonical_budgeted(bytes, &mut usize::MAX)
    }
    /// Precharges all retained file, path and entry allocations before decoding.
    pub fn decode_canonical_budgeted(bytes: &[u8], budget: &mut usize) -> Result<Self, &'static str> {
        let (_, retained) = parse(bytes,false).ok_or("invalid native header aggregate")?;
        *budget = budget.checked_sub(retained).ok_or("native header aggregate allocation budget exceeded")?;
        parse(bytes,true).map(|(aggregate, _)| aggregate).ok_or("invalid native header aggregate")
    }
    /// Validates a producer value and its exact canonical encoded size.
    pub fn encode_checked(&self) -> Result<Vec<u8>, &'static str> {
        Self::encode_checked_budgeted(self, &mut usize::MAX)
    }
    /// Reserves encoded output and validation scratch before allocating.
    pub fn encode_checked_budgeted(&self, budget: &mut usize) -> Result<Vec<u8>, &'static str> {
        let mut size=8usize;
        for f in &self.files { size=size.checked_add(114).and_then(|n|n.checked_add(f.path.len())).and_then(|n|n.checked_add(f.entries.len().checked_mul(ENTRY_BYTES)?)).ok_or("native header aggregate overflow")?; }
        if size>MAX_NATIVE_TS_HEADER_AGGREGATE_BYTES {return Err("native header aggregate too large");}
        let scratch = size.checked_add(self.files.len().checked_mul(std::mem::size_of::<NativeTsHeaderFile>() + 64).ok_or("native header aggregate overflow")?).and_then(|n| n.checked_add(64)).ok_or("native header aggregate overflow")?;
        let reservation = size.checked_add(scratch).ok_or("native header aggregate overflow")?;
        *budget = budget.checked_sub(reservation).ok_or("native header aggregate allocation budget exceeded")?;
        let mut bytes=Vec::with_capacity(size); NativeTsHeaderAggregateSchema::encode(self,&mut bytes);
        Self::decode_canonical(&bytes)?;
        *budget = budget.checked_add(scratch).ok_or("native header aggregate budget overflow")?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(count: usize) -> NativeTsHeaderAggregate {
        NativeTsHeaderAggregate { files: vec![NativeTsHeaderFile {
            path: "src/main.ts".into(), source: SourceIdentity::from_bytes(&vec![b'x'; 20_000]).unwrap(),
            image_identity: [7;32], image_byte_len: 123, producer_recipe: [8;32],
            entries: (0..count).map(|i| NativeTsHeaderEntry {
                entity_id:i as u32, identity:DeclarationIdentity{family:DeclarationFamilyId::from_raw([1;16]),variant:VariantFingerprint::from_raw([2;16])},
                compiler_extent:(i as u32, i as u32 + 2), header_extent:(i as u32, i as u32 + 1),
            }).collect(),
        }] }
    }
    #[test]
    fn borrowed_aggregate_matches_owned_without_allocating() {
        let aggregate = sample(5000);
        let bytes = aggregate.encode_checked().unwrap();
        let allocation = allocation_counter::measure(|| {
            let view = NativeTsHeaderAggregateView::parse_canonical(&bytes).unwrap();
            assert_eq!(view.len(), 1);
            let file = view.file("src/main.ts").unwrap();
            assert_eq!(file.source, aggregate.files[0].source);
            assert_eq!(file.image_identity, aggregate.files[0].image_identity);
            assert_eq!(file.image_byte_len, aggregate.files[0].image_byte_len);
            assert_eq!(file.producer_recipe, aggregate.files[0].producer_recipe);
            assert_eq!(file.entries().count(), 5000);
            assert_eq!(file.entry(0).as_ref(), aggregate.files[0].entries.first());
            assert_eq!(file.entry(4999).as_ref(), aggregate.files[0].entries.last());
            assert_eq!(file.entry(5000), None);
            assert!(view.file("src/missing.ts").is_none());
        });
        assert_eq!(allocation.bytes_max, 0);
        for cutoff in [0, 7, bytes.len() - 1] {
            assert!(NativeTsHeaderAggregateView::parse_canonical(&bytes[..cutoff]).is_err());
        }
        let mut bad = bytes.clone(); bad[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(NativeTsHeaderAggregateView::parse_canonical(&bad).is_err());
        let mut bad = aggregate.clone(); bad.files[0].entries[1].entity_id = 0;
        let mut wire = Vec::new(); NativeTsHeaderAggregateSchema::encode(&bad, &mut wire);
        assert!(NativeTsHeaderAggregateView::parse_canonical(&wire).is_err());
    }

    #[test]
    fn aggregate_supports_five_thousand_headers_and_rejects_bad_wire() {
        let aggregate=sample(5000); let bytes=aggregate.encode_checked().unwrap();
        assert!(bytes.len()>64*1024);
        assert_eq!(NativeTsHeaderAggregate::decode_canonical(&bytes).unwrap(), aggregate);
        let mut short = 1;
        assert!(NativeTsHeaderAggregate::decode_canonical_budgeted(&bytes, &mut short).is_err());
        assert_eq!(short, 1);
        let mut budget = 1024 * 1024;
        assert_eq!(NativeTsHeaderAggregate::decode_canonical_budgeted(&bytes, &mut budget).unwrap(), aggregate);
        assert!(budget < 1024 * 1024);
        assert!(NativeTsHeaderAggregate::decode_canonical(&bytes[..bytes.len()-1]).is_err());
        let mut bad=bytes.clone(); bad[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(NativeTsHeaderAggregate::decode_canonical(&bad).is_err());
        let mut bad=aggregate.clone(); bad.files[0].entries[1].entity_id=0;
        assert!(bad.encode_checked().is_err());
        let mut bad=aggregate.clone(); bad.files[0].entries[0].header_extent=(1,2);
        assert!(bad.encode_checked().is_err());
        let mut bad=sample(MAX_NATIVE_TS_HEADERS_PER_FILE+1); bad.files[0].source.byte_len=40_000;
        assert!(bad.encode_checked().is_err());
    }
}
