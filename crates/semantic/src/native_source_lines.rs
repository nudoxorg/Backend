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
/// Invalid wire/source or a caller work/cancellation refusal.
#[derive(Debug)]
pub enum BorrowedSourceLinesError<E> { Invalid, Stopped(E) }

/// Canonically validated source-line aggregate borrowing immutable wire bytes.
/// It does not itself prove selected-closure membership or source ownership.
#[derive(Clone, Copy)]
pub struct NativeSourceLineAggregateView<'a> { bytes: &'a [u8], count: usize }
#[derive(Clone, Copy)]
pub struct NativeSourceLineFileView<'a> { pub path: &'a str, pub source: SourceIdentity, offsets: &'a [u8] }
pub struct NativeSourceLineFileIter<'a> { reader: Reader<'a>, remaining: usize }

impl<'a> NativeSourceLineAggregateView<'a> {
    /// Validates complete framing, sorted paths and all line positions without
    /// allocating; checks caller work/cancellation before each record/offset.
    pub fn parse<E>(bytes: &'a [u8], checkpoint: &mut impl FnMut(usize)->Result<(),E>) -> Result<Self,BorrowedSourceLinesError<E>> {
        if bytes.len()>MAX_NATIVE_SOURCE_LINE_BYTES {return Err(BorrowedSourceLinesError::Invalid);}
        let mut r=Reader {bytes,at:0};
        if r.take(4)!=Some(b"NLM1".as_slice()) {return Err(BorrowedSourceLinesError::Invalid);}
        let count=r.u32().ok_or(BorrowedSourceLinesError::Invalid)? as usize;
        if count>MAX_NATIVE_SOURCE_LINE_FILES || count>bytes.len().saturating_sub(8)/47 {return Err(BorrowedSourceLinesError::Invalid);}
        let mut previous:Option<&str>=None;
        for _ in 0..count {
            checkpoint(0).map_err(BorrowedSourceLinesError::Stopped)?;
            let n=r.u16().ok_or(BorrowedSourceLinesError::Invalid)? as usize;
            checkpoint(n).map_err(BorrowedSourceLinesError::Stopped)?;
            let path=std::str::from_utf8(r.take(n).ok_or(BorrowedSourceLinesError::Invalid)?).map_err(|_|BorrowedSourceLinesError::Invalid)?;
            if !valid_path(path) || previous.is_some_and(|p|p>=path) {return Err(BorrowedSourceLinesError::Invalid);} previous=Some(path);
            let identity: [u8;32]=r.take(32).ok_or(BorrowedSourceLinesError::Invalid)?.try_into().map_err(|_|BorrowedSourceLinesError::Invalid)?;
            ContentId::<SourceFactDomain>::try_from(identity).map_err(|_|BorrowedSourceLinesError::Invalid)?;
            let length=r.u32().ok_or(BorrowedSourceLinesError::Invalid)?;
            if length>64*1024*1024 {return Err(BorrowedSourceLinesError::Invalid);}
            let lines=r.u32().ok_or(BorrowedSourceLinesError::Invalid)? as usize;
            if lines==0 || lines>MAX_NATIVE_SOURCE_LINES_PER_FILE || lines.checked_mul(4).is_none_or(|n|n>bytes.len().saturating_sub(r.at)) {return Err(BorrowedSourceLinesError::Invalid);}
            let mut prior=None;
            for _ in 0..lines {
                checkpoint(4).map_err(BorrowedSourceLinesError::Stopped)?;
                let offset=r.u32().ok_or(BorrowedSourceLinesError::Invalid)?;
                if (prior.is_none() && offset!=0) || prior.is_some_and(|p|p>=offset) || offset>length {return Err(BorrowedSourceLinesError::Invalid);}
                prior=Some(offset);
            }
        }
        if r.at!=bytes.len(){return Err(BorrowedSourceLinesError::Invalid);}
        Ok(Self{bytes,count})
    }
    pub const fn len(&self)->usize{self.count}
    pub const fn is_empty(&self)->bool{self.count==0}
    pub fn files(&self)->NativeSourceLineFileIter<'a>{NativeSourceLineFileIter{reader:Reader{bytes:self.bytes,at:8},remaining:self.count}}
    /// Fills exact caller-owned slots with canonical file offsets in path order.
    pub fn index_files<E>(&self, slots: &mut [u32], checkpoint: &mut impl FnMut(usize)->Result<(),E>) -> Result<(),BorrowedSourceLinesError<E>> {
        if slots.len()!=self.count {return Err(BorrowedSourceLinesError::Invalid);}
        let mut files=self.files();
        for slot in slots {
            checkpoint(4096+42).map_err(BorrowedSourceLinesError::Stopped)?;
            *slot=u32::try_from(files.reader.at).map_err(|_|BorrowedSourceLinesError::Invalid)?;
            files.next().ok_or(BorrowedSourceLinesError::Invalid)?;
        }
        Ok(())
    }
    /// Binary-searches an index produced by `index_files` for this validated view.
    pub fn find_file_indexed<E>(&self, slots:&[u32], path:&str, checkpoint:&mut impl FnMut(usize)->Result<(),E>) -> Result<Option<NativeSourceLineFileView<'a>>,BorrowedSourceLinesError<E>> {
        if slots.len()!=self.count {return Err(BorrowedSourceLinesError::Invalid);}
        let (mut low,mut high)=(0,slots.len());
        while low<high {
            checkpoint(4096+42).map_err(BorrowedSourceLinesError::Stopped)?;
            let mid=low+(high-low)/2;
            let mut iter=NativeSourceLineFileIter{reader:Reader{bytes:self.bytes,at:slots[mid] as usize},remaining:1};
            let file=iter.next().ok_or(BorrowedSourceLinesError::Invalid)?;
            match file.path.cmp(path) {std::cmp::Ordering::Less=>low=mid+1,std::cmp::Ordering::Greater=>high=mid,std::cmp::Ordering::Equal=>return Ok(Some(file))}
        }
        Ok(None)
    }
    /// Debit the maximum path/header work before each file decode/compare.
    pub fn find_file<E>(&self,path:&str,checkpoint:&mut impl FnMut(usize)->Result<(),E>)->Result<Option<NativeSourceLineFileView<'a>>,E>{
        let mut files=self.files();
        while files.remaining!=0 {checkpoint(4096+42)?; let Some(file)=files.next() else {break;}; if file.path==path{return Ok(Some(file));} if file.path>path{break;} }
        Ok(None)
    }
}
impl<'a> Iterator for NativeSourceLineFileIter<'a> {
    type Item=NativeSourceLineFileView<'a>;
    fn next(&mut self)->Option<Self::Item>{
        if self.remaining==0{return None;} let r=&mut self.reader;
        let n=r.u16()? as usize;let path=std::str::from_utf8(r.take(n)?).ok()?;
        let identity=ContentId::<SourceFactDomain>::try_from(<[u8;32]>::try_from(r.take(32)?).ok()?).ok()?;
        let byte_len=r.u32()?;let lines=r.u32()? as usize;let offsets=r.take(lines.checked_mul(4)?)?;
        self.remaining-=1;Some(NativeSourceLineFileView{path,source:SourceIdentity{identity,byte_len},offsets})
    }
    fn size_hint(&self)->(usize,Option<usize>){(self.remaining,Some(self.remaining))}
}
impl ExactSizeIterator for NativeSourceLineFileIter<'_>{}
impl NativeSourceLineFileView<'_> {
    pub fn line_count(&self)->usize{self.offsets.len()/4}
    pub fn line_start(&self,index:usize)->Option<u32>{let at=index.checked_mul(4)?;Some(u32::from_be_bytes(self.offsets.get(at..at.checked_add(4)?)?.try_into().ok()?))}
    /// Joins against exact UTF-8 host bytes and every newline position. The
    /// selected closure/source ownership must be independently revalidated.
    pub fn matches_source<E>(&self,bytes:&[u8],checkpoint:&mut impl FnMut(usize)->Result<(),E>)->Result<bool,E>{
        checkpoint(0)?;
        if usize::try_from(self.source.byte_len).ok()!=Some(bytes.len()) {return Ok(false);}
        let mut hash=backend_version::ContentPayloadHasher::<SourceFactDomain>::new(bytes.len() as u64);
        for chunk in bytes.chunks(64*1024) {checkpoint(chunk.len())?; if hash.push_chunk(chunk).is_err(){return Ok(false);}}
        let Ok(identity)=hash.finish() else {return Ok(false);}; if identity!=self.source.identity{return Ok(false);}
        // Validate UTF-8 in bounded chunks while preserving incomplete codepoints.
        let mut pos=0usize;
        while pos<bytes.len() {
            let end=(pos+64*1024).min(bytes.len()); checkpoint(end-pos)?;
            match std::str::from_utf8(&bytes[pos..end]) {
                Ok(_)=>pos=end,
                Err(error) if error.error_len().is_none() && end<bytes.len() && error.valid_up_to()>0 => pos+=error.valid_up_to(),
                Err(_)=>return Ok(false),
            }
        }
        if self.line_start(0)!=Some(0){return Ok(false);}
        let mut index=1usize;
        for (chunk_index,chunk) in bytes.chunks(64*1024).enumerate(){
            checkpoint(chunk.len())?; let base=chunk_index*64*1024;
            for (i,byte) in chunk.iter().enumerate(){let at=base+i;
                if *byte==b'\r' && bytes.get(at+1)!=Some(&b'\n'){return Ok(false);}
                if *byte==0xe2 && bytes.get(at+1)==Some(&0x80) && matches!(bytes.get(at+2).copied(),Some(0xa8|0xa9)){return Ok(false);}
                if *byte==b'\n' {let Some(expected)=u32::try_from(at+1).ok() else{return Ok(false);}; if self.line_start(index)!=Some(expected){return Ok(false);} index+=1;}
            }
        }
        checkpoint(0)?;
        Ok(index==self.line_count())
    }
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
    /// Validates producer fields and computes canonical size without allocation.
    pub fn encoded_len_checkpointed<E>(&self,checkpoint:&mut impl FnMut(usize)->Result<(),E>)->Result<usize,BorrowedSourceLinesError<E>>{
        encoded_len_files(self.files.iter(),checkpoint).map(|(size,_)|size)
    }
    /// Writes canonical wire into an exact caller-owned buffer.
    pub fn encode_into<E>(&self,out:&mut [u8],checkpoint:&mut impl FnMut(usize)->Result<(),E>)->Result<(),BorrowedSourceLinesError<E>>{
        encode_files_into(self.files.iter(),out,checkpoint)
    }
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

/// Validates a sorted stream of borrowed producer files and returns exact
/// canonical wire length and count. It retains no paths or line arrays.
pub fn encoded_len_files<'a,E>(files:impl Iterator<Item=&'a NativeSourceLineFile>,checkpoint:&mut impl FnMut(usize)->Result<(),E>)->Result<(usize,usize),BorrowedSourceLinesError<E>>{
    let mut size=8usize;let mut count=0usize;let mut previous:Option<&str>=None;
    for file in files {
        checkpoint(file.path.len()).map_err(BorrowedSourceLinesError::Stopped)?;
        count=count.checked_add(1).ok_or(BorrowedSourceLinesError::Invalid)?;
        if count>MAX_NATIVE_SOURCE_LINE_FILES || !valid_path(&file.path) || previous.is_some_and(|p|p>=file.path.as_str()) || file.source.byte_len>64*1024*1024 || file.line_starts.is_empty() || file.line_starts.len()>MAX_NATIVE_SOURCE_LINES_PER_FILE{return Err(BorrowedSourceLinesError::Invalid);}
        previous=Some(&file.path);let mut prior=None;
        for &offset in &file.line_starts {checkpoint(4).map_err(BorrowedSourceLinesError::Stopped)?;if (prior.is_none() && offset!=0) || prior.is_some_and(|p|p>=offset) || offset>file.source.byte_len{return Err(BorrowedSourceLinesError::Invalid);}prior=Some(offset);}
        size=size.checked_add(42).and_then(|n|n.checked_add(file.path.len())).and_then(|n|n.checked_add(file.line_starts.len().checked_mul(4)?)).ok_or(BorrowedSourceLinesError::Invalid)?;
    }
    if size>MAX_NATIVE_SOURCE_LINE_BYTES{return Err(BorrowedSourceLinesError::Invalid);}Ok((size,count))
}

/// Writes sorted borrowed files into exactly sized caller-owned storage. A
/// stopped/failed operation may leave partial bytes and must be discarded.
pub fn encode_files_into<'a,E,I>(files:I,out:&mut [u8],checkpoint:&mut impl FnMut(usize)->Result<(),E>)->Result<(),BorrowedSourceLinesError<E>>
where I:Iterator<Item=&'a NativeSourceLineFile>+Clone {
    let (size,count)=encoded_len_files(files.clone(),checkpoint)?;
    if out.len()!=size{return Err(BorrowedSourceLinesError::Invalid);}
    struct Writer<'a>{bytes:&'a mut [u8],at:usize}
    impl Writer<'_>{fn put(&mut self,b:&[u8])->bool{let Some(end)=self.at.checked_add(b.len()) else{return false;};let Some(target)=self.bytes.get_mut(self.at..end) else{return false;};target.copy_from_slice(b);self.at=end;true}}
    let mut w=Writer{bytes:out,at:0};
    macro_rules! put {($value:expr)=>{if !w.put($value){return Err(BorrowedSourceLinesError::Invalid);}}}
    put!(b"NLM1");put!(&(count as u32).to_be_bytes());
    let mut written=0usize;let mut previous:Option<&str>=None;
    for file in files {
        checkpoint(file.path.len()).map_err(BorrowedSourceLinesError::Stopped)?;
        written=written.checked_add(1).ok_or(BorrowedSourceLinesError::Invalid)?;
        if written>count || !valid_path(&file.path) || previous.is_some_and(|p|p>=file.path.as_str()) || file.source.byte_len>64*1024*1024 || file.line_starts.is_empty() || file.line_starts.len()>MAX_NATIVE_SOURCE_LINES_PER_FILE{return Err(BorrowedSourceLinesError::Invalid);}
        previous=Some(&file.path);
        put!(&(file.path.len() as u16).to_be_bytes());put!(file.path.as_bytes());put!(file.source.identity.as_ref());put!(&file.source.byte_len.to_be_bytes());put!(&(file.line_starts.len() as u32).to_be_bytes());
        let mut prior=None;
        for &offset in &file.line_starts {checkpoint(4).map_err(BorrowedSourceLinesError::Stopped)?;if (prior.is_none() && offset!=0) || prior.is_some_and(|p|p>=offset) || offset>file.source.byte_len{return Err(BorrowedSourceLinesError::Invalid);}prior=Some(offset);put!(&offset.to_be_bytes());}
    }
    if written!=count || w.at!=size{return Err(BorrowedSourceLinesError::Invalid);}Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn indexed_file_lookup_scales_and_honors_stop() {
        use super::*;
        let source = backend_semantic_test_source();
        let files = (0..5000).map(|i| NativeSourceLineFile { path: format!("src/{i:05}.ts"), source, line_starts: vec![0] }).collect();
        let aggregate = NativeSourceLineAggregate { files };
        let size = aggregate.encoded_len_checkpointed(&mut |_| Ok::<(), ()>(())).unwrap();
        let mut bytes = vec![0; size];
        aggregate.encode_into(&mut bytes, &mut |_| Ok::<(), ()>(())).unwrap();
        let view = NativeSourceLineAggregateView::parse(&bytes, &mut |_| Ok::<(), ()>(())).unwrap();
        let mut index = vec![0; view.len()];
        let mut work = 0usize;
        view.index_files(&mut index, &mut |_| { work += 1; Ok::<(), ()>(()) }).unwrap();
        assert_eq!(work, 5000);
        work = 0;
        for i in 0..5000 {
            let path = format!("src/{i:05}.ts");
            assert_eq!(view.find_file_indexed(&index, &path, &mut |_| { work += 1; Ok::<(), ()>(()) }).unwrap().unwrap().path, path);
        }
        assert!(work < 70_000);
        assert!(matches!(view.find_file_indexed(&index, "src/04999.ts", &mut |_| Err::<(), _>(())), Err(BorrowedSourceLinesError::Stopped(()))));
        assert!(matches!(view.index_files(&mut index[..4999], &mut |_| Ok::<(), ()>(())), Err(BorrowedSourceLinesError::Invalid)));
    }

    fn backend_semantic_test_source() -> crate::ir::SourceIdentity {
        crate::ir::SourceIdentity::from_bytes(b"x").unwrap()
    }
    use super::*;
    #[test]
    fn borrowed_iterator_encoder_rejects_inconsistent_clones() {
        struct Changing<'a>{current:Option<&'a NativeSourceLineFile>,other:&'a NativeSourceLineFile}
        impl<'a> Clone for Changing<'a>{fn clone(&self)->Self{Self{current:Some(self.other),other:self.current.unwrap_or(self.other)}}}
        impl<'a> Iterator for Changing<'a>{type Item=&'a NativeSourceLineFile;fn next(&mut self)->Option<Self::Item>{self.current.take()}}
        let source=SourceIdentity::from_bytes(b"x").unwrap();
        let short=NativeSourceLineFile{path:"a".into(),source,line_starts:vec![0]};
        let long=NativeSourceLineFile{path:"longer".into(),source,line_starts:vec![0]};
        let (size,_)=encoded_len_files(std::iter::once(&short),&mut |_|Ok::<(),()>(())).unwrap();
        let mut wire=vec![0;size];
        let iter=Changing{current:Some(&long),other:&short};
        assert!(matches!(encode_files_into(iter,&mut wire,&mut |_|Ok::<(),()>(())),Err(BorrowedSourceLinesError::Invalid)));
    }

    #[test]
    fn borrowed_lines_match_exact_source_without_allocating_and_stop() {
        let mut source=vec![b'a';65_535]; source.extend_from_slice("α\n".as_bytes());
        let value=NativeSourceLineAggregate{files:vec![NativeSourceLineFile{path:"src/a.ts".into(),source:SourceIdentity::from_bytes(&source).unwrap(),line_starts:vec![0,65_538]}]};
        let mut owned_budget=1024*1024; let expected=value.encode_budgeted(&mut owned_budget).unwrap();
        let mut wire=vec![0u8;expected.len()];
        let measured=allocation_counter::measure(|| {
            value.encode_into(&mut wire,&mut |_|Ok::<(),()>(())).unwrap();
            let view=NativeSourceLineAggregateView::parse(&wire,&mut |_|Ok::<(),()>(())).unwrap();
            let file=view.find_file("src/a.ts",&mut |_|Ok::<(),()>(())).unwrap().unwrap();
            assert_eq!(file.line_count(),2); assert_eq!(file.line_start(1),Some(65_538));
            assert!(file.matches_source(&source,&mut |_|Ok::<(),()>(())).unwrap());
        });
        assert_eq!(measured.bytes_max,0);assert_eq!(wire,expected);
        let mut wrong=source.clone();wrong[0]=b'b';
        let view=NativeSourceLineAggregateView::parse(&wire,&mut |_|Ok::<(),()>(())).unwrap();
        assert!(!view.find_file("src/a.ts",&mut |_|Ok::<(),()>(())).unwrap().unwrap().matches_source(&wrong,&mut |_|Ok::<(),()>(())).unwrap());
        let mut bad=wire.clone();let last=bad.len()-4;bad[last..].copy_from_slice(&0u32.to_be_bytes());
        assert!(matches!(NativeSourceLineAggregateView::parse(&bad,&mut |_|Ok::<(),()>(())),Err(BorrowedSourceLinesError::Invalid)));
        assert!(NativeSourceLineAggregateView::parse(&wire[..wire.len()-1],&mut |_|Ok::<(),()>(())).is_err());
        let mut calls=0;let stopped=NativeSourceLineAggregateView::parse(&wire,&mut |_|{calls+=1;if calls==3{Err(())}else{Ok(())}});
        assert!(matches!(stopped,Err(BorrowedSourceLinesError::Stopped(()))));
        let mut short=vec![0u8;expected.len()-1];assert!(value.encode_into(&mut short,&mut |_|Ok::<(),()>(())).is_err());
    }

    #[test]
    fn borrowed_line_matching_refuses_unsupported_terminators_and_checks_empty_stop() {
        for text in ["a\rb", "a\u{2028}b", "a\u{2029}b", ""] {
            let source = SourceIdentity::from_bytes(text.as_bytes()).unwrap();
            let value = NativeSourceLineAggregate { files: vec![NativeSourceLineFile { path: "a.ts".into(), source, line_starts: vec![0] }] };
            let mut budget = 4096;
            let wire = value.encode_budgeted(&mut budget).unwrap();
            let view = NativeSourceLineAggregateView::parse(&wire, &mut |_| Ok::<(), ()>(())).unwrap();
            let file = view.find_file("a.ts", &mut |_| Ok::<(), ()>(())).unwrap().unwrap();
            assert_eq!(file.matches_source(text.as_bytes(), &mut |_| Ok::<(), ()>(())).unwrap(), text.is_empty());
            if text.is_empty() {
                assert!(file.matches_source(text.as_bytes(), &mut |_| Err::<(), ()>(())).is_err());
                let mut calls = 0;
                assert!(file.matches_source(text.as_bytes(), &mut |_| { calls += 1; if calls == 2 { Err(()) } else { Ok(()) } }).is_err());
            }
        }
    }

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
        let view=NativeSourceLineAggregateView::parse(&bytes,&mut |_|Ok::<(),()>(())).unwrap();
        let file=view.find_file("src/large.ts",&mut |_|Ok::<(),()>(())).unwrap().unwrap();
        assert_eq!(file.line_count(),1_000_000);
        assert_eq!(file.line_start(999_999),Some(999_999));
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
