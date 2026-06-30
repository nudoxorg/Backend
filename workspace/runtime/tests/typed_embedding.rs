//! Real (passing) tests for the const-generic embedding type — the dimension is
//! carried in the type, so a wrong-length vector cannot even be built.

use runtime::vector::Embedding;

/// An embedding knows its dimension at the type level.
#[test]
fn embedding_carries_its_dimension() {
    let e = Embedding::<4>::new([0.1, 0.2, 0.3, 0.4]);
    assert_eq!(e.dimensions(), 4);
    assert_eq!(e.as_slice(), &[0.1, 0.2, 0.3, 0.4]);
}

/// Two differently-sized embeddings are *different types* — there is no runtime
/// path on which they could be confused. (If you uncomment the line below it
/// will not compile.)
#[test]
fn dimensions_are_part_of_the_type() {
    let a = Embedding::<3>::new([1.0, 0.0, 0.0]);
    let b = Embedding::<2>::new([1.0, 0.0]);
    assert_eq!(a.dimensions(), 3);
    assert_eq!(b.dimensions(), 2);
    // let _mismatch: Embedding<3> = b; // <- type error: expected `Embedding<3>`, found `Embedding<2>`
}
