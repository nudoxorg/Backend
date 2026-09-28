use super::*;

/// toml 0.8.23's modules in the fixture index (`v6/gems/data/packages.json`):
/// value, ser, map, de, edit, table, examples, fmt, macros, lib.
const TOML: [f32; 10] = [175.0, 123.0, 58.0, 23.0, 13.0, 10.0, 8.0, 7.0, 5.0, 1.0];

fn parts(weights: &[f32]) -> Vec<Part> {
    weights.iter().enumerate().map(|(i, &w)| Part::new(w, key(&format!("m{i}")))).collect()
}

fn floored(weights: &[f32]) -> Vec<f32> {
    let total: f32 = weights.iter().sum();
    weights.iter().map(|w| w.max(total * FLOOR)).collect()
}

const ALL: [Outline; 7] = [
    Outline::Octagon,
    Outline::Emerald,
    Outline::Shield,
    Outline::Oval,
    Outline::Marquise,
    Outline::Hexagon,
    Outline::Trillion,
];

#[test]
fn every_facet_holds_its_modules_share_of_the_stone() {
    for outline in ALL {
        let c = cut(outline, &parts(&TOML), key("toml"), u8::MAX);
        let whole = c.rim.area();
        let want = floored(&TOML);
        let total: f32 = want.iter().sum();
        assert_eq!(c.facets.len(), TOML.len(), "{outline:?}: one facet per module");
        for (i, w) in want.iter().enumerate() {
            let got = c.facet_of(i).expect("cut").poly.area() / whole;
            assert!((got - w / total).abs() < 0.01, "{outline:?} module {i}: {got:.4} of the stone, weight share {:.4}", w / total);
        }
    }
}

#[test]
fn the_facets_tile_the_stone_exactly_and_each_is_convex() {
    let weights = [40.0, 3.0, 17.0, 9.0, 1.0, 1.0, 22.0, 5.0, 12.0, 30.0, 2.0, 7.0];
    for outline in ALL {
        let c = cut(outline, &parts(&weights), key("tile"), u8::MAX);
        let sum: f32 = c.facets.iter().map(|f| f.poly.area()).sum();
        assert!((sum / c.rim.area() - 1.0).abs() < 1e-3, "{outline:?}: facets cover {sum} of {}", c.rim.area());
        assert!(c.rough.is_empty());
        for f in &c.facets {
            assert!(f.poly.area() > 0.0, "{outline:?}: positively oriented");
            let pts = f.poly.points();
            // Convex: every vertex is on the interior side of every edge.
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                for &p in pts {
                    assert!(super::super::geom::side(a, b, p) >= -1e-2, "{outline:?}: a facet is not convex");
                }
            }
            assert!(c.rim.contains(centroid(&f.poly)));
        }
    }
}

#[test]
fn the_same_package_is_cut_the_same_way_every_time() {
    let a = cut(Outline::Octagon, &parts(&TOML), key("toml"), u8::MAX);
    let b = cut(Outline::Octagon, &parts(&TOML), key("toml"), u8::MAX);
    assert_eq!(a, b);
    let other = cut(Outline::Octagon, &parts(&TOML), key("basic-toml"), u8::MAX);
    assert_ne!(a.facets[0].poly, other.facets[0].poly, "another package, the same weights: another stone");
    // FNV-1a is pinned: a changed hash would recut every stone a person has learned.
    assert_eq!(key("toml"), 0x2fdd_d4ef_389d_b785);
}

#[test]
fn a_reveal_cuts_the_biggest_splits_first_and_leaves_the_rest_rough() {
    let whole = cut(Outline::Octagon, &parts(&TOML), key("toml"), u8::MAX);
    let none = cut(Outline::Octagon, &parts(&TOML), key("toml"), 0);
    assert!(none.facets.is_empty());
    assert_eq!(none.rough.len(), 1);
    assert!((none.rough[0].area() - whole.rim.area()).abs() < 1e-2);
    let one = cut(Outline::Octagon, &parts(&TOML), key("toml"), 1);
    let pieces = one.facets.len() + one.rough.len();
    assert_eq!(pieces, 2, "the first level is one straight cut");
    let area: f32 = one.facets.iter().map(|f| f.poly.area()).sum::<f32>() + one.rough.iter().map(Poly::area).sum::<f32>();
    assert!((area / whole.rim.area() - 1.0).abs() < 1e-3);
    // The first cut of the reveal is the first cut of the whole stone.
    let first = &whole.facets.iter().find(|f| f.part == 0).expect("value").poly;
    assert!(one.facets.iter().any(|f| &f.poly == first) || one.rough.iter().any(|r| (r.area() - first.area()).abs() > 1.0));
}

#[test]
fn submodules_are_cut_inside_their_module_only_when_it_is_big_enough() {
    let mut p = parts(&TOML);
    // ser (123) has ser_value, array, map; lib (1) has one child it is too small to cut.
    p[1].children = vec![Part::new(60.0, key("ser_value")), Part::new(20.0, key("array")), Part::new(20.0, key("map"))];
    p[9].children = vec![Part::new(1.0, key("inner"))];
    let c = cut(Outline::Octagon, &p, key("toml"), u8::MAX);
    let ser = c.facet_of(1).expect("ser").poly.area();
    let kids: Vec<_> = c.facets.iter().filter(|f| f.part == 1 && f.child.is_some()).collect();
    assert_eq!(kids.len(), 4, "three submodules and ser's own remainder");
    let sum: f32 = kids.iter().map(|f| f.poly.area()).sum();
    assert!((sum / ser - 1.0).abs() < 1e-3, "the submodules tile ser's facet");
    assert!(c.facets.iter().all(|f| !(f.part == 9 && f.child.is_some())), "lib is too small to cut");
}

#[test]
fn every_outline_is_convex_positive_and_fills_its_box() {
    for outline in ALL {
        let (w, h) = (100.0 * outline.aspect(), 100.0);
        let rim = outline.poly(w, h);
        assert!(rim.area() > 0.5 * w * h, "{outline:?} fills its box");
        let (min, max) = rim.bounds();
        assert!(min.x.abs() < 0.5 && min.y.abs() < 0.5 && (max.x - w).abs() < 0.5 && (max.y - h).abs() < 0.5, "{outline:?}: {min:?} {max:?}");
        assert!(rim.contains(centroid(&rim)));
    }
}

#[test]
fn a_label_is_placed_only_where_it_fits_inside_its_facet() {
    let c = cut(Outline::Octagon, &parts(&TOML), key("toml"), u8::MAX);
    let floor = Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(px(784.0), px(250.0)));
    assert!(c.label_at(0, 60.0, 16.0, floor).is_some(), "value is the biggest facet: its name fits");
    let (min, max) = c.placed(&c.facet_of(9).expect("lib").poly, floor).bounds();
    assert!(c.label_at(9, max.x - min.x + 1.0, 16.0, floor).is_none(), "wider than lib's facet: no label, never a clipped one");
    let at = c.label_at(0, 60.0, 16.0, floor).expect("fits");
    let placed = c.placed(&c.facet_of(0).expect("value").poly, floor);
    for dx in [-30.0, 30.0] {
        for dy in [-8.0, 8.0] {
            assert!(placed.contains(pt(at.x + dx, at.y + dy)));
        }
    }
}

#[test]
fn only_a_facet_turned_to_the_light_shows_fire() {
    assert!(facing(pt(0.55, 0.83), LIGHT) > FIRE_FROM, "facing the upper-left light");
    assert!(facing(pt(-0.55, -0.83), LIGHT) < 0.0);
    assert!(shade(pt(0.55, 0.83), LIGHT, false) > shade(pt(0.0, 1.0), LIGHT, false));
    assert!(shade(pt(-1.0, 0.0), LIGHT, false) < 0.06, "turned away: near the ground");
}
