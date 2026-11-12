//! Bounded source line-map claims. Selected closure and exact host bytes must
//! be joined before these offsets can be used as source coordinates.
use crate::ir::SourceIdentity;
use backend_version::{ContentId, Schema, SourceFactDomain};

pub const MAX_NATIVE_SOURCE_LINE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_NATIVE_SOURCE_LINE_FILES: usize = 65_536;
pub const MAX_NATIVE_SOURCE_LINES_PER_FILE: usize = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeSourceLineFile {
    pub path: String,
    pub source: SourceIdentity,
    pub line_starts: Vec<u32>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeSourceLineAggregate { pub files: Vec<NativeSourceLineFile> }
pub struct NativeSourceLineAggregateSchema;
impl Schema for NativeSourceLineAggregateSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc00f;
    const VERSION: u8 = 1;
    type Value = NativeSourceLineAggregate;
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(b"NLM1");
        out.extend_from_slice(&(value.files.len() as u32).to_be_bytes());
        for file in &value.files {
            out.extend_from_slice(&(file.path.len() as u16).to_be_bytes());
            out.extend_from_slice(file.path.as_bytes());
            out.extend_from_slice(file.source.identity.as_ref());
            out.extend_from_slice(&file.source.byte_len.to_be_bytes());
            out.extend_from_slice(&(file.line_starts.len() as u32).to_be_bytes());
            for offset in &file.line_starts { out.extend_from_slice(&offset.to_be_bytes()); }
        }
    }
}
fn valid_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 4096 && !path.starts_with('/') && !path.contains('\\')
        && !(path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic())
        && !path.bytes().any(|b| b.is_ascii_control())
        && !path.split('/').any(|p| p.is_empty() || p == "." || p == "..")
}
struct Reader<'a> { bytes: &'a [u8], at: usize }
impl<'a> Reader<'a> {
    fn take(&mut self,n:usize)->Option<&'a [u8]>{let end=self.at.checked_add(n)?;let b=self.bytes.get(self.at..end)?;self.at=end;Some(b)}
    fn u16(&mut self)->Option<u16>{Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))}
    fn u32(&mut self)->Option<u32>{Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))}
}
impl NativeSourceLineFile {
    /// Checks the complete map against independently supplied exact source bytes.
    /// Selected closure membership and source ownership must be proved separately.
    pub fn matches_source(&self, bytes: &[u8]) -> bool {
        let Some(actual) = SourceIdentity::from_bytes(bytes) else { return false; };
        if actual != self.source || self.line_starts.first() != Some(&0) { return false; }
        let Ok(text) = std::str::from_utf8(bytes) else { return false; };
        if text.contains('\u{2028}') || text.contains('\u{2029}') { return false; }
        let mut index = 1usize;
        for (at, byte) in bytes.iter().enumerate() {
            if *byte == b'\r' && bytes.get(at + 1) != Some(&b'\n') { return false; }
            if *byte == b'\n' {
                let Some(expected) = u32::try_from(at + 1).ok() else { return false; };
                if self.line_starts.get(index) != Some(&expected) { return false; }
                index += 1;
            }
        }
        index == self.line_starts.len()
    }
}
impl NativeSourceLineAggregate {
    pub fn file(&self,path:&str)->Option<&NativeSourceLineFile>{self.files.binary_search_by(|f|f.path.as_str().cmp(path)).ok().map(|i|&self.files[i])}
    /// Precharges decoded arrays before materializing untrusted bytes.
    pub fn decode_budgeted(bytes:&[u8], budget:&mut usize)->Result<Self,&'static str>{
        fn parse(bytes:&[u8], materialize:bool)->Option<(NativeSourceLineAggregate,usize)>{
            if bytes.len()>MAX_NATIVE_SOURCE_LINE_BYTES{return None;}
            let mut r=Reader{bytes,at:0}; if r.take(4)?!=b"NLM1"{return None;}
            let count=r.u32()? as usize;
            if count>MAX_NATIVE_SOURCE_LINE_FILES || count>bytes.len().saturating_sub(8)/47{return None;}
            let mut resident=count.checked_mul(std::mem::size_of::<NativeSourceLineFile>())?.checked_add(32)?;
            let mut files=if materialize{Vec::with_capacity(count)}else{Vec::new()};
            let mut previous:Option<&str>=None;
            for _ in 0..count {
                let n=r.u16()? as usize;let path=std::str::from_utf8(r.take(n)?).ok()?;
                if !valid_path(path) || previous.is_some_and(|p|p>=path){return None;}previous=Some(path);
                let identity=ContentId::<SourceFactDomain>::try_from(<[u8;32]>::try_from(r.take(32)?).ok()?).ok()?;
                let byte_len=r.u32()?; if byte_len>64*1024*1024{return None;}
                let nlines=r.u32()? as usize;
                if nlines==0 || nlines>MAX_NATIVE_SOURCE_LINES_PER_FILE || nlines.checked_mul(4)?>bytes.len().saturating_sub(r.at){return None;}
                resident=resident.checked_add(n)?.checked_add(64)?.checked_add(nlines.checked_mul(4)?)?;
                let mut lines=if materialize{Vec::with_capacity(nlines)}else{Vec::new()};
                let mut last=None;
                for _ in 0..nlines {let offset=r.u32()?;if (last.is_none() && offset!=0) || last.is_some_and(|p|p>=offset) || offset>byte_len{return None;}last=Some(offset);if materialize{lines.push(offset);}}
                if materialize{files.push(NativeSourceLineFile{path:path.to_owned(),source:SourceIdentity{identity,byte_len},line_starts:lines});}
            }
            if r.at!=bytes.len(){return None;}Some((NativeSourceLineAggregate{files},resident))
        }
        let (_,resident)=parse(bytes,false).ok_or("invalid native source line aggregate")?;
        *budget=budget.checked_sub(resident).ok_or("native source line allocation budget exceeded")?;
        parse(bytes,true).map(|(v,_)|v).ok_or("invalid native source line aggregate")
    }
    /// Precharges encoded output before allocating; validates producer values.
    pub fn encode_budgeted(&self,budget:&mut usize)->Result<Vec<u8>,&'static str>{
        if self.files.len()>MAX_NATIVE_SOURCE_LINE_FILES{return Err("too many native source line files");}
        let mut size=8usize;let mut previous:Option<&str>=None;
        for file in &self.files {
            if !valid_path(&file.path) || previous.is_some_and(|p|p>=file.path.as_str()) || file.source.byte_len>64*1024*1024 || file.line_starts.is_empty() || file.line_starts.len()>MAX_NATIVE_SOURCE_LINES_PER_FILE || file.line_starts[0]!=0 || file.line_starts.windows(2).any(|p|p[0]>=p[1]) || file.line_starts.last().is_some_and(|p|*p>file.source.byte_len){return Err("invalid native source line file");}
            previous=Some(&file.path);
            size=size.checked_add(42).and_then(|v|v.checked_add(file.path.len())).and_then(|v|v.checked_add(file.line_starts.len().checked_mul(4)?)).ok_or("native source line size overflow")?;
        }
        if size>MAX_NATIVE_SOURCE_LINE_BYTES{return Err("native source line aggregate too large");}
        *budget=budget.checked_sub(size.checked_add(32).ok_or("native source line size overflow")?).ok_or("native source line allocation budget exceeded")?;
        let mut bytes=Vec::with_capacity(size);NativeSourceLineAggregateSchema::encode(self,&mut bytes);Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_map_requires_exact_source_and_complete_newline_positions() {
        let bytes = "α\r\nsecond\n".as_bytes();
        let mut file = NativeSourceLineFile { path:"a.ts".into(), source:SourceIdentity::from_bytes(bytes).unwrap(), line_starts:vec![0,4,11] };
        assert!(file.matches_source(bytes));
        file.line_starts[1]=3; assert!(!file.matches_source(bytes));
        file.line_starts[1]=4; assert!(!file.matches_source("β\r\nsecond\n".as_bytes()));
    }
    #[test]
    fn million_line_map_roundtrips_with_preallocation_limits() {
        let mut source = SourceIdentity::from_bytes(b"source fixture").unwrap();
        source.byte_len = 1_000_000;
        let value = NativeSourceLineAggregate { files: vec![NativeSourceLineFile {
            path:"src/large.ts".into(), source, line_starts:(0..1_000_000).collect(),
        }] };
        let mut budget = 16 * 1024 * 1024;
        let bytes = value.encode_budgeted(&mut budget).unwrap();
        assert!(bytes.len() > 4_000_000);
        let mut short = 1;
        assert!(NativeSourceLineAggregate::decode_budgeted(&bytes, &mut short).is_err());
        assert_eq!(short, 1);
        let mut credit = 8 * 1024 * 1024;
        assert_eq!(NativeSourceLineAggregate::decode_budgeted(&bytes, &mut credit).unwrap(), value);
        assert!(credit < 8 * 1024 * 1024);
        assert!(NativeSourceLineAggregate::decode_budgeted(&bytes[..bytes.len()-1], &mut credit).is_err());
        let mut bad = bytes.clone(); bad[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(NativeSourceLineAggregate::decode_budgeted(&bad, &mut credit).is_err());
        let mut invalid = value; invalid.files[0].line_starts[1]=0;
        assert!(invalid.encode_budgeted(&mut budget).is_err());
    }
}
