use std::{hint::black_box, time::Instant};
fn dst(n:usize,i:usize,w:usize)->usize{if w==0{i}else{n+i*7+w-1}}
fn permute(a:&mut[u64],n:usize){let total=8*n;let mut seen=vec![false;total];for start in 0..total{if seen[start]{continue}let mut cur=start;let mut saved=a[cur];loop{seen[cur]=true;let next=dst(n,cur/8,cur%8);std::mem::swap(&mut saved,&mut a[next]);cur=next;if cur==start{break}if seen[cur]{panic!("non-bijective permutation")};}}}
fn roundtrip(n:usize){let mut a=(0..8*n as u64).collect::<Vec<_>>();let original=a.clone();permute(&mut a,n);for i in 0..n{assert_eq!(a[i],original[i*8]);for w in 1..8{assert_eq!(a[n+i*7+w-1],original[i*8+w]);}}}
fn main(){for n in 0..=257{roundtrip(n)};for n in [100_000usize,1_000_000]{let mut samples=Vec::new();for _ in 0..9{let mut a=(0..8*n as u64).collect::<Vec<_>>();let t=Instant::now();permute(&mut a,n);black_box(a);samples.push(t.elapsed().as_nanos());}samples.sort_unstable();println!("n={n}\tmedian_permute_ns={}\tbytes={}",samples[4],n*64);}}
