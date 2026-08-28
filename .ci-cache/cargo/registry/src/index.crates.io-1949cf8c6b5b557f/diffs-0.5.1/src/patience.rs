use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Index;
use super::{myers, Diff, Replace};

struct I<'a, S: 'a + Index<usize> + ?Sized> {
    p: &'a S,
    i: usize,
}

impl<'a, A: Index<usize> + 'a> std::fmt::Debug for I<'a, A>
where
    A::Output: std::fmt::Debug,
{
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "{:?}", &self.p[self.i])
    }
}

impl<'a, 'b, A: Index<usize> + 'b + ?Sized, B: Index<usize> + 'b + ?Sized> PartialEq<I<'a, A>>
    for I<'b, B>
where
    B::Output: PartialEq<A::Output>,
{
    fn eq(&self, b: &I<'a, A>) -> bool {
        self.p[self.i] == b.p[b.i]
    }
}

enum AB<A, B> {
    A(A),
    B(B),
}

impl<A: std::hash::Hash, B: std::hash::Hash> std::hash::Hash for AB<A, B> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            AB::A(a) => a.hash(state),
            AB::B(a) => a.hash(state),
        }
    }
}

impl<A: Eq, B: Eq + PartialEq<A>> PartialEq for AB<A, B> {
    fn eq(&self, b: &Self) -> bool {
        match (self, b) {
            (AB::A(a), AB::A(b)) => a.eq(b),
            (AB::A(a), AB::B(b)) => b.eq(a),
            (AB::B(a), AB::A(b)) => a.eq(b),
            (AB::B(a), AB::B(b)) => a.eq(b),
        }
    }
}
impl<A: Eq, B: Eq + PartialEq<A>> Eq for AB<A, B> {}

fn unique<
        'a,
    'b,
        A: Hash + Eq + std::fmt::Debug,
        B: Hash + Eq + PartialEq<A> + std::fmt::Debug,
    S: Index<usize, Output = A> + ?Sized,
    T: Index<usize, Output = B> + ?Sized
        >(
    p: &'a S,
    e0: usize,
    e1: usize,
    q: &'b T,
    f0: usize,
    f1: usize,
) -> (Vec<I<'a, S>>, Vec<I<'b, T>>) {
    let mut aa = HashMap::new();
    for i in e0..e1 {
        match aa.entry(AB::A(&p[i])) {
            Entry::Vacant(e) => {
                e.insert(Some(i));
            }
            Entry::Occupied(mut e) => {
                e.get_mut().take();
            }
        }
    }
    let mut bb = HashMap::new();
    for j in f0..f1 {
        match bb.entry(&q[j]) {
            Entry::Vacant(e) => {
                if let Some(Some(_)) = aa.get(&AB::B(&q[j])) {
                    e.insert(Some(j));
                }
            }
            Entry::Occupied(mut e) => {
                aa.insert(AB::B(&q[j]), None);
                e.get_mut().take();
            }
        }
    }
    let mut v: Vec<_> = aa
        .into_iter()
        .filter_map(|(_, x)| x)
        .map(|i| I { p, i })
        .collect();
    v.sort_by(|a, b| a.i.cmp(&b.i));

    let mut w: Vec<_> = bb
        .into_iter()
        .filter_map(|(_, x)| x)
        .map(|i| I { p: q, i })
        .collect();

    w.sort_by(|a, b| a.i.cmp(&b.i));
    (v, w)
}

/// Patience diff algorithm.
pub fn diff<
    A: Hash + Eq + std::fmt::Debug,
    B: Hash + Eq + PartialEq<A> + std::fmt::Debug,
    S: Index<usize, Output = A> + ?Sized + std::fmt::Debug,
    T: Index<usize, Output = B> + ?Sized + std::fmt::Debug,
    D: Diff,
>(
    d: &mut D,
    e: &S,
    e0: usize,
    e1: usize,
    f: &T,
    f0: usize,
    f1: usize,
) -> Result<(), D::Error> {
    let (au, bu) = unique(e, e0, e1, f, f0, f1);

    /*let auv: Vec<_> = au.iter().map(|i| i.i).collect();
    let buv: Vec<_> = bu.iter().map(|i| i.i).collect();
    println!("auv {:?} buv {:?}", auv, buv);*/

    struct Patience<
        'a,
        'b,
        'd,
        S: 'a + Index<usize> + ?Sized,
        T: 'b + Index<usize> + ?Sized,
        D: Diff + 'd,
    > {
        current_a: usize,
        current_b: usize,
        a1: usize,
        b1: usize,
        a: &'a S,
        b: &'b T,
        d: &'d mut D,
        au: &'a [I<'a, S>],
        bu: &'b [I<'b, T>],
    }
    impl<
            'a,
            'b,
            'd,
            S: 'a + Index<usize> + ?Sized,
            T: 'b + Index<usize> + ?Sized,
            D: Diff + 'd,
        > Diff for Patience<'a, 'b, 'd, S, T, D>
    where
        S::Output: std::fmt::Debug + Sized,
        T::Output: PartialEq<S::Output> + std::fmt::Debug + Sized,
    {
        type Error = D::Error;
        fn equal(&mut self, old: usize, new: usize, len: usize) -> Result<(), D::Error> {
            for (old, new) in (old..old + len).zip(new..new + len) {
                let a1 = self.au[old].i;
                let b1 = self.bu[new].i;
                // println!("{:?} {:?} {:?} {:?}", old, new, a1, b1);
                /*while self.current_a < a1
                    && self.current_b < b1
                    && self.b[b1] == self.a[a1]
                {
                    a1 -= 1;
                    b1 -= 1;
                }*/
                // println!("after {:?} {:?}", a1, b1);
                myers::diff_offsets(
                    self.d,
                    self.a,
                    self.current_a,
                    a1,
                    self.b,
                    self.current_b,
                    b1,
                )?;

                // println!("a1 {:?} {:?}", a1, self.au[old].i);
                if a1 < self.au[old].i {
                    self.d.equal(self.current_a, self.current_b, self.au[old].i - a1)?
                }

                self.current_a = self.au[old].i;
                self.current_b = self.bu[new].i;
            }
            Ok(())
        }

        fn finish(&mut self) -> Result<(), D::Error> {
            myers::diff(
                self.d,
                self.a,
                self.current_a,
                self.a1,
                self.b,
                self.current_b,
                self.b1,
            )
        }
    }
    let mut d = Replace::new(Patience {
        current_a: e0,
        current_b: f0,
        a: e,
        a1: e1,
        b: f,
        b1: f1,
        d,
        au: &au,
        bu: &bu,
    });
    myers::diff(&mut d, &au, 0, au.len(), &bu, 0, bu.len())?;
    Ok(())
}

#[test]
fn patience() {
    let a: &[usize] = &[11, 1, 2, 2, 3, 4, 4, 4, 5, 47, 19];
    let b: &[usize] = &[10, 1, 2, 2, 8, 9, 4, 4, 7, 47, 18];
    struct D(Vec<(usize, usize, usize, usize)>);
    impl Diff for D {
        type Error = ();
        fn delete(&mut self, o: usize, len: usize, new: usize) -> Result<(), ()> {
            self.0.push((o, len, new, 0));
            Ok(())
        }
        fn insert(&mut self, o: usize, n: usize, len: usize) -> Result<(), ()> {
            self.0.push((o, 0, n, len));
            Ok(())
        }
        fn replace(&mut self, o: usize, l: usize, n: usize, nl: usize) -> Result<(), ()> {
            self.0.push((o, l, n, nl));
            Ok(())
        }
    }
    let mut d = Replace::new(D(Vec::new()));
    diff(&mut d, a, 0, a.len(), b, 0, b.len()).unwrap();
    let d: D = d.into_inner();
    assert_eq!(
        d.0.as_slice(),
        &[(0, 1, 0, 1), (4, 2, 4, 2), (8, 1, 8, 1), (10, 1, 10, 1)]
    );
}
