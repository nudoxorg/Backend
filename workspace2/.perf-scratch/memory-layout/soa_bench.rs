use std::{hint::black_box, mem::size_of, time::Instant};

#[repr(C)] #[derive(Clone, Copy)] struct AoS { key:u64, object:[u8;48], parent:u32, depth:u32 }
struct SoA { keys:Vec<u64>, objects:Vec<[u8;48]>, parents:Vec<u32>, depths:Vec<u32> }
#[repr(C)] struct Block8 { keys:[u64;8], objects:[[u8;48];8], parents:[u32;8], depths:[u32;8], len:usize }
struct AoSoA { blocks:Vec<Block8> }

fn seed_keys(n:usize)->Vec<u64>{ (0..n as u64).map(|x| x.wrapping_mul(0x9e3779b97f4a7c15)).collect() }
fn build_aos(keys:&[u64])->Vec<AoS>{keys.iter().map(|&k|AoS{key:k,object:[k as u8;48],parent:k as u32,depth:1}).collect()}
fn build_soa(keys:&[u64])->SoA{SoA{keys:keys.to_vec(),objects:keys.iter().map(|&k|[k as u8;48]).collect(),parents:keys.iter().map(|&k|k as u32).collect(),depths:vec![1;keys.len()]}}
fn build_aosoa(keys:&[u64])->AoSoA{let mut blocks=Vec::new(); for chunk in keys.chunks(8){let mut b=Block8{keys:[0;8],objects:[[0;48];8],parents:[0;8],depths:[0;8],len:chunk.len()};for(i,&k)in chunk.iter().enumerate(){b.keys[i]=k;b.objects[i]=[k as u8;48];b.parents[i]=k as u32;b.depths[i]=1;}blocks.push(b)}AoSoA{blocks}}
fn aos_lookup(a:&[AoS], q:&[u64])->u64{q.iter().map(|&x|a.binary_search_by_key(&x,|r|r.key).unwrap_or(0)as u64).sum()}
fn soa_lookup(a:&SoA,q:&[u64])->u64{q.iter().map(|&x|a.keys.binary_search(&x).unwrap_or(0)as u64).sum()}
fn aosoa_lookup(a:&AoSoA,q:&[u64])->u64{q.iter().map(|&x|{let mut lo=0;let mut hi=a.blocks.len();while lo<hi{let m=(lo+hi)/2;if a.blocks[m].keys[0]<=x{lo=m+1}else{hi=m}}let i=lo.saturating_sub(1);let b=&a.blocks[i];b.keys[..b.len].iter().position(|&k|k==x).map_or(0,|j|i as u64*8+j as u64)}).sum()}
fn aos_key(a:&[AoS])->u64{a.iter().map(|x|x.key).sum()}
fn soa_key(a:&SoA)->u64{a.keys.iter().sum()}
fn aosoa_key(a:&AoSoA)->u64{a.blocks.iter().flat_map(|b|b.keys[..b.len].iter()).sum()}
fn aos_full(a:&[AoS])->u64{a.iter().map(|x|x.key+u64::from(x.parent)+u64::from(x.depth)+u64::from(x.object[0])).sum()}
fn soa_full(a:&SoA)->u64{(0..a.keys.len()).map(|i|a.keys[i]+u64::from(a.parents[i])+u64::from(a.depths[i])+u64::from(a.objects[i][0])).sum()}
fn aosoa_full(a:&AoSoA)->u64{a.blocks.iter().map(|b|(0..b.len).map(|i|b.keys[i]+u64::from(b.parents[i])+u64::from(b.depths[i])+u64::from(b.objects[i][0])).sum::<u64>()).sum()}
fn median(v:&mut [u128])->u128{v.sort_unstable();v[v.len()/2]}
fn main(){println!("format\troot-repr-v1\ncolumns\tn\trepresentation\taos_bytes\tsoa_bytes\taosoa_bytes\tlookup_ns\tkey_ns\tfull_ns");for n in [100_000usize,1_000_000]{let keys=seed_keys(n);let mut sorted=keys.clone();sorted.sort_unstable();let queries=sorted.iter().step_by((n/10_000).max(1)).copied().collect::<Vec<_>>();let aos=build_aos(&sorted);let soa=build_soa(&sorted);let aosoa=build_aosoa(&sorted);for name in ["aos","soa","aosoa"]{let mut l=Vec::new();let mut k=Vec::new();let mut f=Vec::new();for _ in 0..7{let t=Instant::now();let x=match name{"aos"=>aos_lookup(&aos,&queries),"soa"=>soa_lookup(&soa,&queries),_=>aosoa_lookup(&aosoa,&queries)};black_box(x);l.push(t.elapsed().as_nanos());let t=Instant::now();let x=match name{"aos"=>aos_key(&aos),"soa"=>soa_key(&soa),_=>aosoa_key(&aosoa)};black_box(x);k.push(t.elapsed().as_nanos());let t=Instant::now();let x=match name{"aos"=>aos_full(&aos),"soa"=>soa_full(&soa),_=>aosoa_full(&aosoa)};black_box(x);f.push(t.elapsed().as_nanos());}println!("result\t{n}\t{name}\t{}\t{}\t{}\t{}\t{}\t{}",n*size_of::<AoS>(),n*(8+48+4+4),((n+7)/8)*size_of::<Block8>(),median(&mut l),median(&mut k),median(&mut f));}}}
