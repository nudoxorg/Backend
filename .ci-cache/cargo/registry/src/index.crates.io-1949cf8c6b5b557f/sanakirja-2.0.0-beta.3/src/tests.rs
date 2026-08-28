use log::*;
use std::collections::BTreeMap;
use std::path::Path;

use crate::debug::*;
use crate::environment::*;
use sanakirja_core::btree::*;
use sanakirja_core::*;

fn add_refs<T: LoadPage, C: Check>(
    txn: &T,
    c: &C,
    refs: &mut BTreeMap<u64, usize>,
) -> Result<(), T::Error>
where
    T::Error: std::fmt::Debug,
{
    c.add_refs(txn, refs)
}

impl Check for A {}
impl Check for U {}
impl Check for V {}
impl Check for [u8] {}

#[derive(Eq, PartialEq, PartialOrd, Ord)]
struct A([u64; 100]);

impl std::fmt::Debug for A {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "A(…)")?;
        Ok(())
    }
}

direct_repr!(A);

#[test]
pub fn put_growth() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let path = tempfile::tempdir().unwrap();
        let path = path.path().join("db");
        let l0 = 1 << 13; // 2 pages
        let env = Env::new(&path, l0, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db::<MutTxn<&Env>, u64, u64>(&mut txn).unwrap();
        let n = 100_000u64;
        for i in 0..n {
            put(&mut txn, &mut db, &i, &i).unwrap();
        }
        println!("{:?}", env.mmaps);
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(len, (l0 << 9) - l0);
        txn.set_root(0, db.db.into());
        txn.commit().unwrap();

        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        let db: Db<u64, u64> = txn.root_db(0).unwrap();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_free_mut(&mut txn, &refs);
        check_refs(&txn, &refs);
    }
}

#[test]
pub fn subtxn() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let path = tempfile::tempdir().unwrap();
        let path = path.path().join("db");
        let l0 = 1 << 13; // 2 pages
        let env = Env::new(&path, l0, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db::<MutTxn<&Env>, u64, u64>(&mut txn).unwrap();
        let n = 100u64;
        for i in 0..n {
            put(&mut txn, &mut db, &i, &i).unwrap();
        }
        txn.set_root(0, db.db.into());
        let txn = {
            let mut subtxn = txn.mut_txn_begin();
            let mut db = subtxn.root_db::<u64, u64, page::Page<u64, u64>>(0).unwrap();
            for i in 0..n {
                put(&mut subtxn, &mut db, &(n + i), &i).unwrap();
            }
            assert!(get(&subtxn, &db, &(n + 10), None).unwrap().is_some());
            subtxn.abort()
        };
        assert_eq!(get(&txn, &db, &(n + 10), None).unwrap(), None)
    }
}

#[derive(Eq, PartialEq, PartialOrd, Ord, Hash, Clone, Copy)]
struct U([u64; 3]);
direct_repr!(U);

#[derive(Eq, PartialEq, PartialOrd, Ord, Hash, Clone, Copy)]
struct V([u64; 5]);
direct_repr!(V);

impl std::fmt::Debug for U {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "U({})", self.0[0])
    }
}

impl std::fmt::Debug for V {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "V")
    }
}

#[test]
pub fn random_scenario_sized_fork() {
    unsafe {
        let path = "/tmp/sanakirja1";
        std::fs::remove_dir_all(path).unwrap_or(());
        std::fs::create_dir_all(path).unwrap();
        let path = Path::new(path).join("db");
        let l0 = 1 << 20;
        let env = Env::new(&path, l0, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db::<MutTxn<&Env>, U, V>(&mut txn).unwrap();
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::SmallRng::from_seed(*b"rc',.snjcg'sthomw,.vbw,.p84fcxjw");
        let mut ve: Vec<(U, V)> = Vec::with_capacity(100_000);
        use std::collections::HashMap;
        let mut h: HashMap<U, V> = HashMap::with_capacity(100_000);

        let mut refs = BTreeMap::new();
        let mut dbs = Vec::new();

        env_logger::try_init().unwrap_or(());
        for i in 0..200_000 {
            let do_debug = false;
            if i % 100_000 == 0 {
                info!("========== i = {:?} {:?}", i, db.db);
            }
            dbs.push(fork_db(&mut txn, &db).unwrap());
            if rng.random_range(0..4) == 3 {
                if let Some((k, v)) = ve.pop() {
                    if do_debug {
                        debug!("del {:?} {:?}", k.0[0], v.0[0]);
                    }
                    assert!(del(&mut txn, &mut db, &k, Some(&v)).unwrap())
                }
            } else {
                let k = U([rng.random(), rng.random(), rng.random()]);
                let v = V([
                    rng.random(),
                    rng.random(),
                    rng.random(),
                    rng.random(),
                    rng.random(),
                ]);
                if do_debug {
                    debug!("put {:?} {:?}", k.0[0], v.0[0]);
                }
                put(&mut txn, &mut db, &k, &v).unwrap();
                ve.push((k, v));
                h.insert(k, v);
            }
            if do_debug {
                debug(&txn, &[&db], format!("debug_{}", i), true);
                for (i, (k, v)) in ve.iter().enumerate() {
                    if get(&txn, &db, k, None).unwrap() != Some((k, v)) {
                        panic!("test {:?} {:?} {:?}", i, k.0[0], v.0[0]);
                    }
                }
                for x in iter(&txn, &db, None).unwrap() {
                    let (k, v) = x.unwrap();
                    if h.get(k) != Some(v) {
                        panic!("test {:?}", k);
                    }
                }
                refs.clear();
                add_refs(&txn, &db, &mut refs).unwrap();
                for db in dbs.iter() {
                    add_refs(&txn, db, &mut refs).unwrap();
                }
                add_free_refs_mut(&txn, &mut refs).unwrap();
                check_free_mut(&mut txn, &refs);
                if let Some(ref rc) = txn.rc {
                    let mut last = 0;
                    for r in iter(&txn, rc, None).unwrap() {
                        let (r, _) = r.unwrap();
                        let r = r.as_u64();
                        if last > 0 && last == (r & !0xfff) {
                            panic!("r = {:?} last = {:?}", r, last);
                        }
                        last = r & !0xfff;
                    }
                }
                let mut n = Vec::new();
                for (p, r) in refs.iter() {
                    if *r >= 2 {
                        let rc = txn.rc(*p).unwrap();
                        if rc != *r as u64 {
                            n.push((p, *r, rc))
                        }
                    } else {
                        assert_eq!(txn.rc(*p).unwrap(), 0);
                    }
                }
                if !n.is_empty() {
                    panic!("n = {:?} {:?}", n, n.len());
                }
            }
        }
    }
}

#[test]
pub fn random_scenario_sized_test() {
    unsafe {
        let path = "/tmp/sanakirja2";
        std::fs::remove_dir_all(path).unwrap_or(());
        std::fs::create_dir_all(path).unwrap();
        let path = Path::new(path).join("db");
        let l0 = 1 << 20;
        let env = Env::new(&path, l0, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db::<MutTxn<&Env>, U, V>(&mut txn).unwrap();
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::SmallRng::from_seed(*b"rc',.snjcg'sthomw,.vbw,.p84fcxjw");
        let mut ve: Vec<(U, V)> = Vec::with_capacity(100_000);
        use std::collections::HashMap;
        let mut h: HashMap<U, V> = HashMap::with_capacity(100_000);

        let mut refs = BTreeMap::new();

        for i in 0..1_000_000 {
            let do_debug = false; // i % 10_000_000 == 0;
            if do_debug {
                env_logger::try_init().unwrap_or(());
                info!("========== i = {:?} {:?}", i, db.db);
            }
            if rng.random_range(0..4) == 3 {
                if let Some((k, v)) = ve.pop() {
                    if do_debug {
                        debug!("del {:?} {:?}", k.0[0], v.0[0]);
                    }
                    assert!(del(&mut txn, &mut db, &k, Some(&v)).unwrap())
                }
            } else {
                let k = U([rng.random(), rng.random(), rng.random()]);
                let v = V([
                    rng.random(),
                    rng.random(),
                    rng.random(),
                    rng.random(),
                    rng.random(),
                ]);
                if do_debug {
                    debug!("put {:?} {:?}", k.0[0], v.0[0]);
                }
                put(&mut txn, &mut db, &k, &v).unwrap();
                ve.push((k, v));
                h.insert(k, v);
            }
            if do_debug {
                for (i, (k, v)) in ve.iter().enumerate() {
                    if get(&txn, &db, k, None).unwrap() != Some((k, v)) {
                        panic!("test {:?} {:?} {:?}", i, k.0[0], v.0[0]);
                    }
                }
                for x in iter(&txn, &db, None).unwrap() {
                    let (k, v) = x.unwrap();
                    if h.get(k) != Some(v) {
                        panic!("test {:?}", k);
                    }
                }
                refs.clear();
                add_refs(&txn, &db, &mut refs).unwrap();
                check_free_mut(&mut txn, &refs);
                for (p, r) in refs.iter() {
                    if *r >= 2 {
                        let rc = txn.rc(*p).unwrap();
                        if rc != *r as u64 {
                            panic!("p {:?} r {:?} {:?}", p, r, rc);
                        }
                    } else {
                        assert_eq!(txn.rc(*p).unwrap(), 0);
                    }
                }
            }
        }
    }
}

// Ici, problèmes:
// - On a une suppression dans la feuille, ça ne devrait pas causer un split juste au-dessus.
// - Et en plus le split est mal géré.
#[test]
pub fn random_scenario_unsized_test() {
    unsafe {
        let path = "/tmp/sanakirja3";
        std::fs::remove_dir_all(path).unwrap_or(());
        std::fs::create_dir_all(path).unwrap();
        let path = Path::new(path).join("db");
        let l0 = 1 << 20;
        let env = Env::new(&path, l0, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db_::<MutTxn<&Env>, U, V, page_unsized::Page<U, V>>(&mut txn).unwrap();
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::SmallRng::from_seed(*b"rc',.snjcg'sthomw,.vbw,.p84fcxjw");
        let mut ve: Vec<(U, V)> = Vec::with_capacity(100_000);
        use std::collections::HashMap;
        let mut h: HashMap<U, V> = HashMap::with_capacity(100_000);

        let mut refs = BTreeMap::new();

        for i in 0..448696 {
            let do_debug = i >= 448_693;
            if do_debug {
                env_logger::try_init().unwrap_or(());
                info!("========== i = {:?} {:?}", i, db.db);
            }
            if rng.random_range(0..4) == 3 {
                if let Some((k, v)) = ve.pop() {
                    if do_debug {
                        debug!("del {:?} {:?}", k.0[0], v.0[0]);
                        debug(&txn, &[&db], "debug0", true);
                    }
                    if !del(&mut txn, &mut db, &k, Some(&v)).unwrap() {
                        panic!("del {}", i);
                    }
                    if do_debug {
                        debug(&txn, &[&db], "debug1", true);
                    }
                }
            } else {
                let k = U([rng.random(), rng.random(), rng.random()]);
                let v = V([
                    rng.random(),
                    rng.random(),
                    rng.random(),
                    rng.random(),
                    rng.random(),
                ]);
                if do_debug {
                    debug!("put {:?} {:?}", k.0[0], v.0[0]);
                }
                put(&mut txn, &mut db, &k, &v).unwrap();
                ve.push((k, v));
                h.insert(k, v);
            }
            if do_debug {
                for (i_, (k, v)) in ve.iter().enumerate() {
                    if get(&txn, &db, k, None).unwrap() != Some((k, v)) {
                        debug(&txn, &[&db], "debug1", true);
                        panic!("test {:?} {:?} {:?} {:?}", i, i_, k.0[0], v.0[0]);
                    }
                }
                for x in iter(&txn, &db, None).unwrap() {
                    let (k, v) = x.unwrap();
                    if h.get(k) != Some(v) {
                        panic!("test {:?}", k);
                    }
                }
                refs.clear();
                add_refs(&txn, &db, &mut refs).unwrap();
                check_free_mut(&mut txn, &refs);
                for (p, r) in refs.iter() {
                    if *r >= 2 {
                        let rc = txn.rc(*p).unwrap();
                        if rc != *r as u64 {
                            panic!("p {:?} r {:?} {:?}", p, r, rc);
                        }
                    } else {
                        assert_eq!(txn.rc(*p).unwrap(), 0);
                    }
                }
            }
        }
    }
}

#[test]
pub fn main() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db::<MutTxn<&Env>, u64, A>(&mut txn).unwrap();
        let n = 1_000u64;
        let m = 1000;
        let mut values = Vec::with_capacity(n as usize);
        let i0 = 500;
        for i in 0..n {
            if i != i0 && (i * i) % m == (i0 * i0) % m {
                continue;
            }
            let a = A([i * i * i; 100]);
            if put(&mut txn, &mut db, &((i * i) % m), &a).unwrap() {
                values.push((i * i) % m);
            }
        }
        values.sort();
        debug(&txn, &[&db], "debug0", true);
        let mut curs = Cursor::new(&txn, &db).unwrap();
        let mut nn = 0;
        while let Some((k, v)) = curs.next(&mut txn).unwrap() {
            debug!("{:?} {:?}", k, v.0[0]);
            assert_eq!(*k, values[nn]);
            nn += 1;
        }
        assert_eq!(nn, values.len());

        let db2 = fork_db(&mut txn, &db).unwrap();
        let a = A([0; 100]);
        put(&mut txn, &mut db, &(m / 2), &a).unwrap();
        let mut curs = Cursor::new(&txn, &db).unwrap();
        let (k, v) = curs.set(&txn, &((i0 * i0) % m), None).unwrap().unwrap();
        assert_eq!((i0 * i0) % m, *k);
        assert_eq!(i0 * i0 * i0, (v.0)[0]);

        let mut curs = Cursor::new(&txn, &db).unwrap();
        let a = A([i0 * i0 * i0; 100]);
        let (k, v) = curs.set(&txn, &((i0 * i0) % m), Some(&a)).unwrap().unwrap();
        assert_eq!((i0 * i0) % m, *k);
        assert_eq!(i0 * i0 * i0, (v.0)[0]);

        debug(&txn, &[&db], "debug0", true);
        let mut curs = Cursor::new(&txn, &db).unwrap();
        curs.set_last(&txn).unwrap();
        let (k, _) = curs.prev(&txn).unwrap().unwrap();
        assert_eq!(k, values.last().unwrap());
        txn.commit().unwrap();

        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_refs(&txn, &db2, &mut refs).unwrap();
        check_free_mut(&mut txn, &refs);
        check_refs(&txn, &refs);
    }
}

#[test]
pub fn u64_unit() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, ()> = create_db(&mut txn).unwrap();
        let n = 10_000_000u64;
        for i in 0..n {
            put(&mut txn, &mut db, &((i * i) % 1_000), &()).unwrap();
        }
        let d = 10;
        for i in 0..d {
            del(&mut txn, &mut db, &((i * i) % 1_000), None).unwrap();
        }
        txn.set_root(0, db.db.into());
        txn.commit().unwrap();

        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        add_refs(
            &txn,
            &txn.root_db::<u64, (), page::Page<u64, ()>>(0).unwrap(),
            &mut refs,
        )
        .unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_free_mut(&mut txn, &refs);
        check_refs(&txn, &refs);
    }
}

#[test]
pub fn u64_u64() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, u64> = create_db(&mut txn).unwrap();
        let n = 1_000_000u64;
        for i in 0..n {
            put(&mut txn, &mut db, &((i * i) % 1_000), &i).unwrap();
        }
        txn.set_root(0, db.db.into());
        txn.commit().unwrap();

        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_free_mut(&mut txn, &refs);
        check_refs(&txn, &refs);
    }
}

#[test]
pub fn last_cursor() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, ()> = create_db(&mut txn).unwrap();
        let n = 100_000u64;
        let m = 10_000;
        let mut max = 0;
        for i in 0..n {
            put(&mut txn, &mut db, &((i * i) % m), &()).unwrap();
            max = max.max((i * i) % m);
        }
        let db2 = fork_db(&mut txn, &db).unwrap();
        let mut curs = Cursor::new(&txn, &db).unwrap();
        curs.set_last(&txn).unwrap();
        let (&nn, _) = curs.prev(&txn).unwrap().unwrap();
        assert_eq!(max, nn);
        let mut refs = BTreeMap::new();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_refs(&txn, &db2, &mut refs).unwrap();
        for (p, r) in refs.iter() {
            if *r >= 2 {
                assert_eq!(txn.rc(*p).unwrap(), *r as u64);
            } else {
                assert_eq!(txn.rc(*p).unwrap(), 0);
            }
        }
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_free_mut(&mut txn, &refs);
        check_refs(&txn, &refs);
        txn.commit().unwrap();
    }
}

#[test]
pub fn empty_last_cursor() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let db: Db<u64, ()> = create_db(&mut txn).unwrap();
        let mut curs = Cursor::new(&txn, &db).unwrap();
        curs.set_last(&txn).unwrap();
        assert!(curs.next(&txn).unwrap().is_none());
        txn.set_root(0, db.db.into());
        txn.commit().unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        let db: Db<u64, u64> = txn.root_db(0).unwrap();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
    }
}

#[test]
pub fn del_mid() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, ()> = create_db(&mut txn).unwrap();
        let n = 2_000u64;
        let mut values = Vec::with_capacity(n as usize);
        for i in 0..n - 1 {
            if put(&mut txn, &mut db, &i, &()).unwrap() {
                values.push(i);
            }
        }
        del(&mut txn, &mut db, &1274, None).unwrap();
        del(&mut txn, &mut db, &1529, None).unwrap();
        assert!(!del(&mut txn, &mut db, &(n + 1), None).unwrap());
        txn.set_root(0, db.db.into());
        txn.commit().unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        let db: Db<u64, ()> = txn.root_db(0).unwrap();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
    }
}

#[test]
pub fn del_leaf() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, A> = create_db(&mut txn).unwrap();
        let n = 200u64;
        let i0 = 10u64;
        let mut values = Vec::with_capacity(n as usize);
        for i in 0..n {
            let a = A([i; 100]);
            put(&mut txn, &mut db, &i, &a).unwrap();
            if i != i0 {
                values.push(i);
            }
        }
        let db2 = fork_db(&mut txn, &db).unwrap();
        del(&mut txn, &mut db, &i0, None).unwrap();
        debug(&txn, &[&db, &db2], "debug0", true);
        assert_eq!(
            iter(&txn, &db, None)
                .unwrap()
                .map(|kv| *kv.unwrap().0)
                .collect::<Vec<_>>(),
            values
        );
        txn.set_root(0, db.db.into());
        txn.set_root(1, db2.db.into());

        txn.commit().unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        let db: Db<u64, A> = txn.root_db(0).unwrap();
        let db2: Db<u64, A> = txn.root_db(1).unwrap();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_refs(&txn, &db2, &mut refs).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
    }
}

#[test]
pub fn del_internal() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, u64> = create_db(&mut txn).unwrap();
        let n = 256u64;
        let i0 = 127u64;
        let mut values = Vec::with_capacity(n as usize);
        for i in 0..n {
            put(&mut txn, &mut db, &i, &i).unwrap();
            if i != i0 {
                values.push(i);
            }
        }
        debug!("===============");
        debug(&txn, &[&db], "debug", true);
        let db2 = fork_db(&mut txn, &db).unwrap();
        del(&mut txn, &mut db, &i0, None).unwrap();
        debug(&txn, &[&db, &db2], "debug1", true);
        let db3: Db<u64, u64> = Db {
            db: core::num::NonZeroU64::new_unchecked(20480),
            k: std::marker::PhantomData,
            v: std::marker::PhantomData,
            p: std::marker::PhantomData,
        };
        debug(&txn, &[&db, &db2, &db3], "debug2", true);
        assert_eq!(
            iter(&txn, &db, None)
                .unwrap()
                .map(|kv| *kv.unwrap().0)
                .collect::<Vec<_>>(),
            values
        );
        txn.set_root(0, db.db.into());
        txn.set_root(1, db2.db.into());

        txn.commit().unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_refs(&txn, &db2, &mut refs).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
    }
}

#[test]
pub fn fork_test() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, A> = create_db(&mut txn).unwrap();
        let n = 19u64;
        let mut values = Vec::with_capacity(n as usize);
        let a = A([0; 100]);
        for i in 0..n - 1 {
            if put(&mut txn, &mut db, &i, &a).unwrap() {
                values.push(i);
            }
        }

        let db2 = fork_db(&mut txn, &db).unwrap();
        let mut values2 = values.clone();
        values2.sort();
        debug(&txn, &[&db, &db2], "debug0", true);
        debug!(">>>>>>");
        for i in n - 1..n {
            if put(&mut txn, &mut db, &i, &a).unwrap() {
                values.push(i);
            }
        }
        debug!("<<<<<<<<<");
        values.sort();
        debug(&txn, &[&db, &db2], "debug1", true);

        let mut curs = Cursor::new(&txn, &db).unwrap();
        let mut nn = 0;
        while let Some((k, _)) = curs.next(&mut txn).unwrap() {
            assert_eq!(*k, values[nn]);
            nn += 1;
        }

        let mut curs = Cursor::new(&txn, &db).unwrap();
        curs.set(&txn, &500, Some(&a)).unwrap();

        assert_eq!(nn, values.len());
        let mut curs = Cursor::new(&txn, &db2).unwrap();
        let mut nn = 0;
        while let Some((k, _)) = curs.next(&mut txn).unwrap() {
            debug!("{:?}", *k);
            assert_eq!(*k, values2[nn]);
            nn += 1;
        }
        assert_eq!(nn, values2.len());

        debug!("{:?}", txn.free_owned_pages);
        debug(&txn, &[&db, &db2], "debug0", true);

        debug!("==============");
        del(&mut txn, &mut db, &11, None).unwrap();
        debug!("free_owned_pages = {:?}", txn.free_owned_pages);
        debug(&txn, &[&db, &db2], "debug1", true);
        debug!("=============");
        for i in 0..15 {
            debug(&txn, &[&db, &db2], &format!("debug-{}", i), true);
            debug!("deleting {:?}", i);
            del(&mut txn, &mut db, &i, None).unwrap();
        }
        for i in n / 2..n {
            del(&mut txn, &mut db, &i, None).unwrap();
        }
        debug!("{:?} {:?}", db, db2);
        debug(&txn, &[&db, &db2], "debug3", true);

        let mut refs = BTreeMap::new();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_refs(&txn, &db2, &mut refs).unwrap();
        // add_refs(&txn, &db3, &mut refs).unwrap();
        let mut err = 0;
        for (p, r) in refs.iter() {
            println!("{:?} {:?}", p, r);
            if *r >= 2 {
                if txn.rc(*p).unwrap() != *r as u64 {
                    error!("{:?} {:?} {:?}", p, txn.rc(*p).unwrap(), *r);
                    err += 1;
                }
            } else {
                if txn.rc(*p).unwrap() != 0 {
                    error!("{:?} {:?} 0", p, txn.rc(*p).unwrap());
                    err += 1;
                }
            }
        }
        assert_eq!(err, 0);
        txn.commit().unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
        debug!("{:?}", refs);
    }
}

#[cfg(target_family = "unix")]
#[test]
fn multi_txn() {
    unsafe {
        std::fs::remove_dir_all("/tmp/sanakirja_multi_txn").unwrap_or(());
        std::fs::create_dir_all("/tmp/sanakirja_multi_txn").unwrap();

        env_logger::try_init().unwrap_or(());
        let child = libc::fork();
        if child == 0 {
            // child

            let env = Env::new("/tmp/sanakirja_multi_txn/db", 4096 * 20, 2).unwrap();

            // Mutable txn
            let mut txn = Env::mut_txn_begin(&env).unwrap();
            info!("started child mutable txn {:?}", txn.root);

            assert_eq!(txn.root, 0);

            let db = create_db::<MutTxn<&Env>, u64, ()>(&mut txn).unwrap();
            debug!("db = {:?}", db.db);
            txn.set_root(0, db.db.into());
            std::thread::sleep(std::time::Duration::from_millis(200));
            txn.commit().unwrap();
            info!("committed");
            let t = std::time::SystemTime::now();
            let mut txn = Env::mut_txn_begin(&env).unwrap();

            assert_eq!(txn.root, 1);

            // Since the parent has an immutable transaction started, we
            // need to wait for at least some time (1s - 100ms of
            // synchronisation margin).
            assert!(t.elapsed().unwrap() >= std::time::Duration::from_millis(90));
            info!("started child mutable txn {:?}", txn.root);
            let db = create_db::<MutTxn<&Env>, u64, ()>(&mut txn).unwrap();
            debug!("db = {:?}", db.db);
            txn.set_root(1, db.db.into());
            std::thread::sleep(std::time::Duration::from_millis(100));
            txn.commit().unwrap();

            /*
            let mut txn = Env::mut_txn_begin(&env).unwrap();
            let mut refs = BTreeMap::new();
            add_free_refs_mut(&txn, &mut refs).unwrap();
            check_refs(&txn, &refs);
            check_free_mut(&mut txn, &refs);
            */
            debug!("child exit");
            libc::exit(0)
        } else {
            // parent
            std::thread::sleep(std::time::Duration::from_millis(100));

            let env = Env::new("/tmp/sanakirja_multi_txn/db", 4096 * 20, 2).unwrap();

            // Immutable
            let txn = Env::txn_begin(&env).unwrap();
            info!("started parent txn {:?}", txn.root);

            // The child didn't commit yet.
            assert_eq!(txn.root, 1);

            std::thread::sleep(std::time::Duration::from_millis(300));
            std::mem::drop(txn);

            std::thread::sleep(std::time::Duration::from_millis(100));
            let txn = Env::txn_begin(&env).unwrap();
            info!("started parent txn {:?}", txn.root);

            // The parent committed, this is a new transaction.
            assert_eq!(txn.root, 0);

            let mut status = 1;
            libc::wait(&mut status);
            assert_eq!(status, 0);
            std::mem::drop(txn);
            let mut txn = Env::mut_txn_begin(&env).unwrap();
            let mut refs = BTreeMap::new();
            add_refs(
                &txn,
                &txn.root_db::<u64, (), page::Page<u64, ()>>(0).unwrap(),
                &mut refs,
            )
            .unwrap();
            add_refs(
                &txn,
                &txn.root_db::<u64, (), page::Page<u64, ()>>(1).unwrap(),
                &mut refs,
            )
            .unwrap();
            add_free_refs_mut(&txn, &mut refs).unwrap();
            check_refs(&txn, &refs);
            check_free_mut(&mut txn, &refs);
        }
    }
}

type UP<K, V> = page_unsized::Page<K, V>;
type P<K, V> = page::Page<K, V>;

#[test]
fn slice() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db_::<MutTxn<&Env>, u64, [u8], UP<u64, [u8]>>(&mut txn).unwrap();
        let n = 10_000u64;
        let m = 1000;
        let mut values = Vec::with_capacity(n as usize);
        for i in 0..n {
            debug!("=============== putting {:?}", i);
            let alpha = b"abcdefgihjklmnopqrstuvwxyz";
            let a = &alpha[..((i as usize * 7) % 25) + 1];
            if put(&mut txn, &mut db, &i, &a[..]).unwrap() {
                values.push((i * i) % m);
            }
        }
        values.sort();
    }
}

#[test]
fn more_than_two_versions() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let n = 5;
        let env = Env::new_anon(40960, n).unwrap();

        let mut txn = Env::mut_txn_begin(&env).unwrap();

        // Allocate two pages.
        for i in 0..n {
            let page = txn.alloc_page().unwrap();
            debug!("page = {:?}", page);
            txn.set_root(i, page.0.offset);
        }
        txn.commit().unwrap();

        for i in 0..n {
            let mut txn = Env::mut_txn_begin(&env).unwrap();
            // Free one of the pages.
            debug!("root(0) = {:?}", txn.root(i));
            txn.decr_rc(txn.root(i).unwrap()).unwrap();
            txn.remove_root(i);
            txn.commit().unwrap();
        }

        let mut txn = Env::mut_txn_begin(&env).unwrap();
        {
            let p = &*(env.mmaps.lock()[0].ptr.add(txn.root * PAGE_SIZE) as *const GlobalHeader);
            debug!("free page: 0x{:x}", u64::from_le(p.free_db));
            let db: Db<u64, ()> = Db {
                db: core::num::NonZeroU64::new_unchecked(u64::from_le(p.free_db)),
                k: std::marker::PhantomData,
                v: std::marker::PhantomData,
                p: std::marker::PhantomData,
            };
            for x in iter(&txn, &db, None).unwrap() {
                debug!("0x{:x}", x.unwrap().0);
            }
        }

        let page = txn.alloc_page().unwrap();
        debug!("page = {:?}", page);
    }
}

#[test]
fn sized_vs_unsized() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409_600_000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();

        let mut db = create_db_::<MutTxn<&Env>, u64, u64, P<u64, u64>>(&mut txn).unwrap();
        let now = std::time::SystemTime::now();
        let n = 1_000u64;
        for i in 0..n {
            debug!("=================== {:?}", i);
            assert!(put(&mut txn, &mut db, &i, &i).unwrap());
        }
        println!("sized put: {:?}", now.elapsed());
        let now = std::time::SystemTime::now();
        for i in 0..n {
            debug!("=================== {:?}", i);
            get(&txn, &db, &i, None).unwrap();
        }
        println!("sized lookup: {:?}", now.elapsed());

        let mut refs = BTreeMap::new();
        add_refs(&txn, &db, &mut refs).unwrap();
        let mut err = 0;
        for (p, r) in refs.iter() {
            if *r >= 2 {
                error!("{:?} referenced twice", p);
                err += 1
            }
        }
        debug!("{:?}", txn.free);
        add_free_refs_mut(&mut txn, &mut refs).unwrap();
        check_free_mut(&mut txn, &refs);
        assert_eq!(err, 0);
        let len = txn.length >> 12;
        println!("sized length = {:?}", len);

        let env = Env::new_anon(409_600_000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db2 = create_db_::<MutTxn<&Env>, u64, u64, UP<u64, u64>>(&mut txn).unwrap();
        let now = std::time::SystemTime::now();
        let n = 1_000u64;
        for i in 0..n {
            assert!(put(&mut txn, &mut db2, &i, &i).unwrap());
        }
        println!("unsized put: {:?}", now.elapsed());
        let now = std::time::SystemTime::now();
        for i in 0..n {
            get(&txn, &db2, &i, None).unwrap();
        }
        println!("unsized lookup: {:?}", now.elapsed());
        refs.clear();
        add_refs(&txn, &db2, &mut refs).unwrap();
        add_free_refs_mut(&mut txn, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
    }
}

#[test]
#[ignore]
fn lmdb() {
    unsafe {
        use lmdb_rs::*;
        env_logger::try_init().unwrap_or(());
        for i in 1..2 {
            let n = i * 5000;
            let mut times = [0f64; 12];
            let mut test = Vec::with_capacity(n);
            let mut rng = rand::rng();
            for _ in 0..n {
                use rand::Rng;
                test.push((rng.random(), rng.random()))
            }

            std::fs::remove_dir_all("/tmp/sanakirja0").unwrap_or(());
            std::fs::create_dir_all("/tmp/sanakirja0").unwrap();
            std::fs::remove_file("/tmp/sanakirja0/db").unwrap_or(());

            let env = Env::new("/tmp/sanakirja0/db", 409_600_000, 2).unwrap();
            let mut txn = Env::mut_txn_begin(&env).unwrap();

            let mut db = create_db_::<MutTxn<&Env>, u64, u64, P<u64, u64>>(&mut txn).unwrap();

            let now = std::time::SystemTime::now();
            for (k, v) in test.iter() {
                assert!(put(&mut txn, &mut db, k, v).unwrap());
            }
            times[0] = now.elapsed().unwrap().as_secs_f64();
            let now = std::time::SystemTime::now();
            debug(&txn, &[&db], "debug", true);
            for (k, v) in test.iter() {
                assert_eq!(get(&txn, &db, &k, None).unwrap(), Some((k, v)))
            }
            times[1] = now.elapsed().unwrap().as_secs_f64();

            let env = Env::new_anon(409_600_000, 2).unwrap();
            let mut txn = Env::mut_txn_begin(&env).unwrap();
            let mut db = create_db_::<MutTxn<&Env>, u64, u64, P<u64, u64>>(&mut txn).unwrap();
            let now = std::time::SystemTime::now();
            for (k, v) in test.iter() {
                assert!(put(&mut txn, &mut db, k, v).unwrap());
            }
            debug(&txn, &[&db], "debug", true);
            times[2] = now.elapsed().unwrap().as_secs_f64();
            let now = std::time::SystemTime::now();
            for (k, v) in test.iter() {
                assert_eq!(get(&txn, &db, &k, None).unwrap(), Some((k, v)))
            }
            times[3] = now.elapsed().unwrap().as_secs_f64();

            let mut b = BTreeMap::new();
            let now = std::time::SystemTime::now();
            for (k, v) in test.iter() {
                b.insert(*k, *v);
            }
            times[4] = now.elapsed().unwrap().as_secs_f64();
            let now = std::time::SystemTime::now();
            for (k, v) in test.iter() {
                assert_eq!(b.get(k), Some(v));
            }
            times[5] = now.elapsed().unwrap().as_secs_f64();

            std::fs::remove_dir_all("/tmp/test-lmdb").unwrap_or(());
            std::fs::create_dir_all("/tmp/test-lmdb").unwrap_or(());
            let env = EnvBuilder::new()
                .map_size(1 << 30)
                .open("/tmp/test-lmdb", 0o777)
                .unwrap();

            let db_handle = env.get_default_db(core::DbIntKey).unwrap();
            let txn = env.new_transaction().unwrap();
            {
                let db = txn.bind(&db_handle);
                let now = std::time::SystemTime::now();
                for (k, v) in test.iter() {
                    db.set(k, v).unwrap();
                }
                times[6] = now.elapsed().unwrap().as_secs_f64();
            }

            // Note: `commit` is choosen to be explicit as
            // in case of failure it is responsibility of
            // the client to handle the error
            match txn.commit() {
                Err(_) => panic!("failed to commit!"),
                Ok(_) => (),
            }

            let reader = env.get_reader().unwrap();
            let db = reader.bind(&db_handle);
            let now = std::time::SystemTime::now();
            for (k, v) in test.iter() {
                let name = db.get::<u64>(k).ok();
                assert_eq!(name, Some(*v))
            }
            times[7] = now.elapsed().unwrap().as_secs_f64();
            /*
            std::fs::remove_dir_all("/tmp/test-sled").unwrap_or(());
            std::fs::create_dir_all("/tmp/test-sled").unwrap_or(());
            let db: sled::Db = sled::open("/tmp/test-sled").unwrap();
            let now = std::time::SystemTime::now();
            for (k, v) in test.iter() {
                unsafe {
                    db.insert(
                        std::slice::from_raw_parts(k as *const u64 as *const u8, 8),
                        std::slice::from_raw_parts(v as *const u64 as *const u8, 8),
                    )
                    .unwrap();
                }
            }
            times[8] = now.elapsed().unwrap().as_secs_f64();
            let now = std::time::SystemTime::now();
            for (k, _v) in test.iter() {
                unsafe {
                    db.get(std::slice::from_raw_parts(k as *const u64 as *const u8, 8))
                        .unwrap();
                }
            }
            times[9] = now.elapsed().unwrap().as_secs_f64();
             */
            /*
            {
                use old_sanakirja::*;
                std::fs::remove_dir_all("/tmp/sanakirja1").unwrap_or(());
                std::fs::create_dir_all("/tmp/sanakirja1").unwrap();
                let env = Env::new("/tmp/sanakirja1", 409_600_000).unwrap();
                let mut txn = Env::mut_txn_begin(&env).unwrap();
                let mut db = txn.create_db::<u64, u64>().unwrap();
                let now = std::time::SystemTime::now();
                let mut rng = rand::thread_rng();
                for (k, v) in test.iter() {
                    assert!(txn.put(&mut rng, &mut db, *k, *v).unwrap());
                }
                times[10] = now.elapsed().unwrap().as_secs_f64();
                let now = std::time::SystemTime::now();
                for (k, v) in test.iter() {
                    assert_eq!(txn.get(&db, *k, None).unwrap(), Some(*v))
                }
                times[11] = now.elapsed().unwrap().as_secs_f64();
            }
            */
            print!("{}", n);
            for t in times.iter() {
                print!(", {}", t)
            }
            println!();
        }
    }
}

#[test]
fn split_on_del1() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db_::<MutTxn<&Env>, u64, [u8], UP<u64, [u8]>>(&mut txn).unwrap();
        for i in (0..157).step_by(10) {
            for i in i..i + 4 {
                let a = [b'a'; 500];
                put(&mut txn, &mut db, &i, &a[..]).unwrap();
            }
            put(&mut txn, &mut db, &(i + 9), &[b'b'; 250]).unwrap();
        }
        for i in (0..157).step_by(10) {
            for i in i + 4..i + 7 {
                let a = [b'a'; 500];
                put(&mut txn, &mut db, &i, &a[..]).unwrap();
            }
        }
        for i in 0..3 {
            debug!("====== del {:?}", i);
            del(&mut txn, &mut db, &i, None).unwrap();
        }
        assert_eq!(
            depth::<_, u64, [u8], UP<u64, [u8]>>(&txn, db.db.into()).unwrap(),
            2
        );
        del(&mut txn, &mut db, &3, None).unwrap();
        assert_eq!(
            depth::<_, u64, [u8], UP<u64, [u8]>>(&txn, db.db.into()).unwrap(),
            3
        );
        txn.commit().unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
    }
}

fn depth<
    T: LoadPage,
    K: Storable + ?Sized + std::fmt::Debug,
    V: Storable + ?Sized + std::fmt::Debug,
    P: BTreePage<K, V>,
>(
    txn: &T,
    mut p: u64,
) -> Result<usize, T::Error> {
    unsafe {
        let mut n = 1;
        loop {
            let pp = txn.load_page(p)?;
            let cursor = P::cursor_first(&pp);
            let l = P::left_child(pp.as_page(), &cursor);
            if l == 0 {
                return Ok(n);
            }
            p = l;
            n += 1;
        }
    }
}

#[test]
fn split_on_del2() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db_::<MutTxn<&Env>, u64, [u8], UP<u64, [u8]>>(&mut txn).unwrap();
        for i in (0..157).step_by(10) {
            for i in i..i + 4 {
                let a = [b'a'; 500];
                put(&mut txn, &mut db, &i, &a[..]).unwrap();
            }
            put(&mut txn, &mut db, &(i + 9), &[b'b'; 255]).unwrap();
        }
        for i in (0..157).step_by(10) {
            for i in i + 4..i + 7 {
                let a = [b'a'; 500];
                put(&mut txn, &mut db, &i, &a[..]).unwrap();
            }
        }
        del(&mut txn, &mut db, &0, None).unwrap();
        txn.commit().unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut refs = BTreeMap::new();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        add_refs(&txn, &db, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
    }
}

#[test]
#[cfg(feature = "uuid")]
#[ignore]
fn lmdb_uuid() {
    use lmdb_rs::*;
    env_logger::try_init().unwrap_or(());
    for i in 1..8 {
        let n = i * 1_000_000;
        std::fs::remove_dir_all("/tmp/sanakirja0").unwrap_or(());
        std::fs::create_dir_all("/tmp/sanakirja0").unwrap();

        let env = Env::new("/tmp/sanakirja0", 409_600_000, 2).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();

        let mut db =
            create_db_::<MutTxn<&Env>, uuid_::Bytes, u64, P<uuid_::Bytes, u64>>(&mut txn).unwrap();

        let mut times = [0f64; 4];

        let mut test = Vec::with_capacity(n);
        for i in 0..n {
            let uuid = uuid_::Uuid::new_v4();
            test.push((uuid, i))
        }
        let now = std::time::SystemTime::now();
        for (k, v) in test.iter() {
            assert!(put(&mut txn, &mut db, k.as_bytes(), &(*v as u64)).unwrap());
        }
        times[0] = now.elapsed().unwrap().as_secs_f64();
        let now = std::time::SystemTime::now();
        for (k, v) in test.iter() {
            assert_eq!(
                get(&txn, &db, k.as_bytes(), None).unwrap(),
                Some((k.as_bytes(), &(*v as u64)))
            )
        }
        times[1] = now.elapsed().unwrap().as_secs_f64();

        std::fs::remove_dir_all("/tmp/test-lmdb").unwrap_or(());
        std::fs::create_dir_all("/tmp/test-lmdb").unwrap_or(());
        let env = EnvBuilder::new()
            .map_size(1 << 30)
            .open("/tmp/test-lmdb", 0o777)
            .unwrap();

        let db_handle = env.get_default_db(lmdb_rs::DbFlags::empty()).unwrap();
        let txn = env.new_transaction().unwrap();
        {
            let db = txn.bind(&db_handle);
            let now = std::time::SystemTime::now();
            for (k, v) in test.iter() {
                let k = MDB_val {
                    mv_size: 16,
                    mv_data: k.as_bytes().as_ptr() as *const libc::c_void,
                };
                db.set(&k, &(*v as u64)).unwrap();
            }
            times[2] = now.elapsed().unwrap().as_secs_f64();
        }
        match txn.commit() {
            Err(_) => panic!("failed to commit!"),
            Ok(_) => (),
        }

        let reader = env.get_reader().unwrap();
        let db = reader.bind(&db_handle);
        let now = std::time::SystemTime::now();
        for (k, v) in test.iter() {
            let k = MDB_val {
                mv_size: 16,
                mv_data: k.as_bytes().as_ptr() as *const libc::c_void,
            };
            let name = db.get::<u64>(&k).ok();
            assert_eq!(name, Some(*v as u64))
        }
        times[3] = now.elapsed().unwrap().as_secs_f64();

        print!("{}", n);
        for t in times.iter() {
            print!(", {}", t)
        }
        println!();
    }
}

#[test]
fn iterators() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(40960, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db_::<MutTxn<&Env>, u64, A, P<u64, A>>(&mut txn).unwrap();
        for i in 0..100 {
            let a = A([i; 100]);
            put(&mut txn, &mut db, &i, &a).unwrap();
        }
        let mut cursor = Cursor::new(&txn, &db).unwrap();
        debug(&txn, &[&db], "debug", true);
        for i in 0..50 {
            let (k, v) = cursor.next(&txn).unwrap().unwrap();
            debug!("a {:?} {:?}", i, k);
            assert_eq!(*k, i);
            assert_eq!(v.0[0], i);
        }
        for i in (25..50).rev() {
            let (k, v) = cursor.prev(&txn).unwrap().unwrap();
            debug!("b {:?} {:?}", i, k);
            assert_eq!(*k, i);
            assert_eq!(v.0[0], i);
        }
        for i in 24..75 {
            let (k, v) = cursor.next(&txn).unwrap().unwrap();
            debug!("c {:?} {:?}", i, k);
            assert_eq!(*k, i);
            assert_eq!(v.0[0], i);
        }
        for i in (0..75).rev() {
            let (k, v) = cursor.prev(&txn).unwrap().unwrap();
            debug!("d {:?} {:?}", i, k);
            assert_eq!(*k, i);
            assert_eq!(v.0[0], i);
        }
        debug(&txn, &[&db], "debug", true);
        let i0 = 30;
        for (kv, n) in rev_iter(&txn, &db, Some((&i0, None)))
            .unwrap()
            .zip((0..=i0).rev())
        {
            let (k, _v) = kv.unwrap();
            assert_eq!(*k, n);
            debug!("k = {:?}", k);
        }
        let i0 = 40;
        for (kv, n) in iter(&txn, &db, Some((&i0, None))).unwrap().zip(i0..) {
            let (k, _v) = kv.unwrap();
            assert_eq!(*k, n);
            debug!("k = {:?}", k);
        }

        let mut it = rev_iter(&txn, &db, Some((&100, None))).unwrap();
        let (k, _v) = it.next().unwrap().unwrap();
        assert_eq!(*k, 99);

        let mut cursor = Cursor::new(&txn, &db).unwrap();
        for i in 0..100 {
            debug!("i = {:?}", i);
            let (&k, v) = cursor.set(&txn, &i, None).unwrap().unwrap();
            debug!("kv = {:?} {:?}", k, v);
            assert_eq!(i, k);
            let (&k1, v1) = cursor.next(&txn).unwrap().unwrap();
            debug!("next = {:?} {:?}", k1, v1);
            assert_eq!(i, k1);
        }

        let mut cursor = Cursor::new(&txn, &db).unwrap();
        for i in 0..100 {
            debug!("i = {:?}", i);
            let (&k, v) = cursor.set(&txn, &i, None).unwrap().unwrap();
            debug!("kv = {:?} {:?}", k, v);
            assert_eq!(i, k);
            let (&k1, v1) = cursor.prev(&txn).unwrap().unwrap();
            debug!("prev = {:?} {:?}", k1, v1);
            assert_eq!(i, k1);
        }
    }
}

#[test]
pub fn fork_del() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, A> = create_db(&mut txn).unwrap();

        let n = 6u64;
        let mut values = Vec::with_capacity(n as usize);
        let a = A([0; 100]);
        for i in 0..n {
            if put(&mut txn, &mut db, &i, &a).unwrap() {
                values.push(i);
            }
        }
        debug(&txn, &[&db], "debug0", true);

        let db2 = fork_db(&mut txn, &db).unwrap();
        del(&mut txn, &mut db, &1, None).unwrap();

        debug(&txn, &[&db, &db2], "debug1", true);
        let mut refs = BTreeMap::new();
        add_refs(&txn, &db, &mut refs).unwrap();
        add_refs(&txn, &db2, &mut refs).unwrap();
        let mut err = 0;
        for (p, r) in refs.iter() {
            println!("{:?} {:?}", p, r);
            if *r >= 2 {
                if txn.rc(*p).unwrap() != *r as u64 {
                    error!("{:?} {:?} {:?}", p, txn.rc(*p).unwrap(), *r);
                    err += 1;
                }
            } else {
                if txn.rc(*p).unwrap() != 0 {
                    error!("{:?} {:?} 0", p, txn.rc(*p).unwrap());
                    err += 1;
                }
            }
        }
        assert_eq!(err, 0);
        txn.commit().unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        add_free_refs_mut(&txn, &mut refs).unwrap();
        check_refs(&txn, &refs);
        check_free_mut(&mut txn, &refs);
        debug!("{:?}", refs);
    }
}

#[test]
pub fn fork_drop() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let env = Env::new_anon(409600000, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, u64> = create_db(&mut txn).unwrap();
        let n = 1000u64;
        let i0 = 10u64;
        let mut values = Vec::with_capacity(n as usize);
        for i in 0..n {
            put(&mut txn, &mut db, &i, &i).unwrap();
            if i != i0 {
                values.push(i);
            }
        }
        let db2 = fork_db(&mut txn, &db).unwrap();
        put(&mut txn, &mut db, &n, &n).unwrap();
        debug(&txn, &[&db, &db2], "debug1", true);
        drop(&mut txn, db2).unwrap();
        debug(&txn, &[&db], "debug2", true);

        let mut refs = BTreeMap::new();
        add_refs(&txn, &db, &mut refs).unwrap();
        let mut err = 0;
        for (p, r) in refs.iter() {
            println!("{:?} {:?}", p, r);
            if *r >= 2 {
                error!("{:?} {:?} {:?}", p, txn.rc(*p).unwrap(), *r);
                err += 1;
            } else {
                if txn.rc(*p).unwrap() != 0 {
                    error!("{:?} {:?} 0", p, txn.rc(*p).unwrap());
                    err += 1;
                }
            }
        }
        assert_eq!(err, 0);
    }
}

#[test]
pub fn delete_frees_space() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let path = tempfile::tempdir().unwrap();
        let path = path.path().join("db");
        let l0 = 1 << 13; // 2 pages
        let env = Env::new(&path, l0, 1).unwrap();
        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db = create_db::<MutTxn<&Env>, u64, u64>(&mut txn).unwrap();
        let n = 100_000u64;
        for i in 0..n {
            put(&mut txn, &mut db, &i, &i).unwrap();
        }
        txn.set_root(0, db.db.into());
        txn.commit().unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(len, (l0 << 9) - l0);

        let mut txn = Env::mut_txn_begin(&env).unwrap();
        let mut db: Db<u64, u64> = txn.root_db(0).unwrap();
        let n = 100_000u64;
        for i in 0..n {
            del(&mut txn, &mut db, &i, None).unwrap();
        }
        info!("root {:?}", db.db);
        txn.set_root(0, db.db.into());
        txn.commit().unwrap();

        // We can't really guarantee this for a single transaction.
        // Shake it a little bit.
        for i in 0..2 {
            let mut txn = Env::mut_txn_begin(&env).unwrap();
            let mut db: Db<u64, u64> = txn.root_db(0).unwrap();
            info!("new 0:0");
            put(&mut txn, &mut db, &0, &0).unwrap();
            info!("root {:?}", db.db);
            txn.set_root(0, db.db.into());
            txn.commit().unwrap();
        }
        let len = std::fs::metadata(&path).unwrap().len();
        if len >> 12 > 14 {
            panic!("len = {:?} pages", len >> 12);
        }
    }
}

/// Regression (found via palimpsest / the pijul pristine, which uses a
/// file-backed `Env::new(path, 1 << 20, 2)`): after a few commits that
/// grow and then shrink the file, a **read-only** transaction fails to
/// read a page a mutable transaction reads fine, with
/// `Error::Corrupt(offset)` — "offset is past the end of the file".
///
/// `Txn::load_page` bounds-checks offsets against the `length` stored
/// in the committed root's header, so this means a commit wrote a
/// `length` smaller than the offset of a page still alive in that very
/// generation. The scenario that triggers it: a small, long-lived
/// table gets relocated upwards by copy-on-write during a transaction
/// that also grew the file, and a later transaction frees the large
/// block below/above it, letting the end-of-file trimming loop shrink
/// `length` past the relocated table's pages.
#[test]
fn read_txn_after_shrink() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let path = tempfile::tempdir().unwrap();
        let path = path.path().join("db");
        // Same parameters as the pijul pristine.
        let env = Env::new(&path, 1 << 20, 2).unwrap();

        // Commit 1: a small long-lived table ("channels"), on low pages.
        {
            let mut txn = Env::mut_txn_begin(&env).unwrap();
            let mut chan = create_db::<MutTxn<&Env>, u64, u64>(&mut txn).unwrap();
            put(&mut txn, &mut chan, &0, &0).unwrap();
            txn.set_root(0, chan.db.into());
            txn.commit().unwrap();
        }

        let read_all = |round: usize, phase: &str| {
            let rtxn = Env::txn_begin(&env).unwrap();
            // `rtxn.size` is the `length` header of the root this
            // read-only transaction opened — the bound its
            // `load_page` enforces.
            let bound = rtxn.size;
            let chan: Db<u64, u64> = rtxn.root_db(0).unwrap();
            for x in iter(&rtxn, &chan, None).unwrap() {
                if let Err(e) = x {
                    panic!(
                        "round {} ({}): read-only txn cannot read the small \
                         table: {} (file is {} pages, committed root says \
                         length = {} pages)",
                        round,
                        phase,
                        e,
                        std::fs::metadata(&path).unwrap().len() >> 12,
                        bound >> 12
                    );
                }
            }
            let big: Option<Db<u64, u64>> = rtxn.root_db(1);
            if let Some(big) = big {
                for x in iter(&rtxn, &big, None).unwrap() {
                    if let Err(e) = x {
                        panic!(
                            "round {} ({}): read-only txn cannot read the big \
                             table: {} (file is {} pages, committed root says \
                             length = {} pages)",
                            round,
                            phase,
                            e,
                            std::fs::metadata(&path).unwrap().len() >> 12,
                            bound >> 12
                        );
                    }
                }
            }
        };

        let n = 20_000u64;
        for round in 0..8 {
            // Grow: fill a big table, and *touch the small one* so
            // copy-on-write relocates it into the newly grown region.
            {
                let mut txn = Env::mut_txn_begin(&env).unwrap();
                let mut big = if let Some(db) = txn.root_db(1) {
                    db
                } else {
                    create_db::<MutTxn<&Env>, u64, u64>(&mut txn).unwrap()
                };
                for i in 0..n {
                    put(&mut txn, &mut big, &i, &i).unwrap();
                }
                let mut chan: Db<u64, u64> = txn.root_db(0).unwrap();
                put(&mut txn, &mut chan, &(round as u64 + 1), &0).unwrap();
                txn.set_root(0, chan.db.into());
                txn.set_root(1, big.db.into());
                txn.commit().unwrap();
            }
            read_all(round, "after growth");

            // Shrink: empty the big table; the commit trims the file.
            {
                let mut txn = Env::mut_txn_begin(&env).unwrap();
                let mut big: Db<u64, u64> = txn.root_db(1).unwrap();
                for i in 0..n {
                    del(&mut txn, &mut big, &i, None).unwrap();
                }
                txn.set_root(1, big.db.into());
                txn.commit().unwrap();
            }
            read_all(round, "after shrink");
        }
    }
}

/// Adversarial variant of `read_txn_after_shrink`: a single
/// transaction that begins by trimming (the previous transaction
/// freed everything), then does *several* cycles of growing back into
/// the trimmed region and freeing a few of the re-extended pages.
/// After commit, the free db must describe exactly the final state:
/// no stale entry for a re-extended page that stayed alive, exactly
/// one entry for a re-extended page freed again, and the committed
/// `length` must cover every page reachable from the root — checked
/// both with a full read-only iteration and with the
/// `check_free`/`check_refs` consistency walkers.
#[test]
fn read_txn_after_shrink_regrow_cycles() {
    unsafe {
        env_logger::try_init().unwrap_or(());
        let path = tempfile::tempdir().unwrap();
        let path = path.path().join("db");
        let env = Env::new(&path, 1 << 20, 2).unwrap();

        // Commit 1: create the table.
        {
            let mut txn = Env::mut_txn_begin(&env).unwrap();
            let db = create_db::<MutTxn<&Env>, u64, u64>(&mut txn).unwrap();
            txn.set_root(0, db.db.into());
            txn.commit().unwrap();
        }

        let n = 20_000u64;
        for round in 0..6 {
            // Grow: fill the table (several hundred pages).
            {
                let mut txn = Env::mut_txn_begin(&env).unwrap();
                let mut db: Db<u64, u64> = txn.root_db(0).unwrap();
                for i in 0..n {
                    put(&mut txn, &mut db, &i, &i).unwrap();
                }
                txn.set_root(0, db.db.into());
                txn.commit().unwrap();
            }
            // Shrink: free (almost) everything, so the *next* begin trims.
            {
                let mut txn = Env::mut_txn_begin(&env).unwrap();
                let mut db: Db<u64, u64> = txn.root_db(0).unwrap();
                for i in 0..n {
                    del(&mut txn, &mut db, &i, None).unwrap();
                }
                txn.set_root(0, db.db.into());
                txn.commit().unwrap();
            }
            // The adversarial transaction: begins with a trim, then
            // several cycles of growing back into the trimmed region
            // and freeing a few of the freshly re-extended pages.
            {
                let mut txn = Env::mut_txn_begin(&env).unwrap();
                let mut db: Db<u64, u64> = txn.root_db(0).unwrap();
                let mut next = 0u64;
                for _cycle in 0..5 {
                    // Grow: a few thousand entries extend the file
                    // into (and past) the trimmed region.
                    for i in next..next + 4_000 {
                        put(&mut txn, &mut db, &i, &i).unwrap();
                    }
                    // Free a few of the pages just allocated.
                    for i in next..next + 500 {
                        del(&mut txn, &mut db, &i, None).unwrap();
                    }
                    next += 4_000;
                }
                txn.set_root(0, db.db.into());
                txn.commit().unwrap();
            }

            // A read-only transaction must be able to read everything.
            {
                let rtxn = Env::txn_begin(&env).unwrap();
                let bound = rtxn.size;
                let db: Db<u64, u64> = rtxn.root_db(0).unwrap();
                for x in iter(&rtxn, &db, None).unwrap() {
                    if let Err(e) = x {
                        panic!(
                            "round {}: read-only txn failed: {} (file {} pages, \
                             committed length {} pages)",
                            round,
                            e,
                            std::fs::metadata(&path).unwrap().len() >> 12,
                            bound >> 12
                        );
                    }
                }
            }

            // And the free db + references must be consistent: every
            // page is either reachable, free, or trimmed — no page
            // both alive and free, none lost.
            {
                let mut txn = Env::mut_txn_begin(&env).unwrap();
                let mut refs = BTreeMap::new();
                let db: Db<u64, u64> = txn.root_db(0).unwrap();
                add_refs(&txn, &db, &mut refs).unwrap();
                add_free_refs_mut(&txn, &mut refs).unwrap();
                check_free_mut(&mut txn, &refs);
                check_refs(&txn, &refs);
            }
        }
    }
}
