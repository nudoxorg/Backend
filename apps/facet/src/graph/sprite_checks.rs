//! Native glyph evidence for navigation-history determinism.
use crate::gallery::{Shot, find, run};
use backend_gui_harness::Script;
use sha2::{Digest, Sha256};

/// Overlapping transparent sprites painted in one layer, as a text line is.
/// Cache IDs run backwards from paint order; texture slots may interleave.
fn layered_sprites(kind: gpui::AtlasTextureKind, textures: [u32; 3]) -> gpui::Scene {
    use gpui::{
        AtlasTextureId, AtlasTile, Bounds, ContentMask, DevicePixels, ScaledPixels, TileId,
    };
    let bounds = Bounds::new(
        gpui::point(ScaledPixels(0.0), ScaledPixels(0.0)),
        gpui::size(ScaledPixels(20.0), ScaledPixels(20.0)),
    );
    let mut scene = gpui::Scene::default();
    scene.push_layer(bounds);
    for ((tile_id, texture), hue) in [30, 20, 10].into_iter().zip(textures).zip([0.0, 0.3, 0.6]) {
        let tile = AtlasTile {
            texture_id: AtlasTextureId {
                index: texture,
                kind,
            },
            tile_id: TileId(tile_id),
            padding: 0,
            bounds: Bounds::new(
                gpui::point(DevicePixels(0), DevicePixels(0)),
                gpui::size(DevicePixels(20), DevicePixels(20)),
            ),
        };
        let color = gpui::hsla(hue, 0.8, 0.6, 0.5).into();
        match kind {
            gpui::AtlasTextureKind::Monochrome => scene.insert_primitive(gpui::MonochromeSprite {
                order: 0,
                pad: 0,
                bounds,
                content_mask: ContentMask { bounds },
                color,
                tile,
                transformation: gpui::TransformationMatrix::default(),
            }),
            gpui::AtlasTextureKind::Subpixel => scene.insert_primitive(gpui::SubpixelSprite {
                order: 0,
                pad: 0,
                bounds,
                content_mask: ContentMask { bounds },
                color,
                tile,
                transformation: gpui::TransformationMatrix::default(),
            }),
            gpui::AtlasTextureKind::Polychrome => scene.insert_primitive(gpui::PolychromeSprite {
                order: 0,
                pad: 0,
                grayscale: false.into(),
                opacity: 0.5,
                bounds,
                content_mask: ContentMask { bounds },
                corner_radii: gpui::Corners::default(),
                tile,
            }),
        }
    }
    scene.pop_layer();
    scene
}

fn painted_tile_ids(scene: &gpui::Scene, kind: gpui::AtlasTextureKind) -> Vec<u32> {
    match kind {
        gpui::AtlasTextureKind::Monochrome => scene
            .monochrome_sprites
            .iter()
            .map(|s| s.tile.tile_id.0)
            .collect(),
        gpui::AtlasTextureKind::Subpixel => scene
            .subpixel_sprites
            .iter()
            .map(|s| s.tile.tile_id.0)
            .collect(),
        gpui::AtlasTextureKind::Polychrome => scene
            .polychrome_sprites
            .iter()
            .map(|s| s.tile.tile_id.0)
            .collect(),
    }
}

fn sprite_batches(
    scene: &gpui::Scene,
    kind: gpui::AtlasTextureKind,
) -> Vec<(u32, std::ops::Range<usize>)> {
    scene
        .batches()
        .map(|batch| {
            let (texture, range) = match batch {
                gpui::PrimitiveBatch::MonochromeSprites { texture_id, range }
                | gpui::PrimitiveBatch::SubpixelSprites { texture_id, range }
                | gpui::PrimitiveBatch::PolychromeSprites { texture_id, range } => {
                    (texture_id, range)
                }
                _ => panic!("sprite fixture emitted a non-sprite batch"),
            };
            assert_eq!(texture.kind, kind);
            (texture.index, range)
        })
        .collect()
}

#[test]
fn same_atlas_reversed_cache_ids_preserve_paint_order_and_one_batch() {
    for kind in [
        gpui::AtlasTextureKind::Monochrome,
        gpui::AtlasTextureKind::Subpixel,
        gpui::AtlasTextureKind::Polychrome,
    ] {
        let mut scene = layered_sprites(kind, [0, 0, 0]);
        scene.finish();
        assert_eq!(
            painted_tile_ids(&scene, kind),
            [30, 20, 10],
            "{kind:?} cache allocation changed paint order"
        );
        assert_eq!(
            sprite_batches(&scene, kind),
            [(0, 0..3)],
            "same texture should retain one batch"
        );
        // Retained GPUI elements replay the original paint operations, not the
        // atlas-sorted primitive arrays. Their next frame must keep this order.
        let mut replay = gpui::Scene::default();
        replay.replay(0..scene.len(), &scene);
        replay.finish();
        assert_eq!(painted_tile_ids(&replay, kind), [30, 20, 10]);
        assert_eq!(sprite_batches(&replay, kind), [(0, 0..3)]);
    }
}

#[test]
fn interleaved_atlases_preserve_paint_order_and_contiguous_batch_ranges() {
    for kind in [
        gpui::AtlasTextureKind::Monochrome,
        gpui::AtlasTextureKind::Subpixel,
        gpui::AtlasTextureKind::Polychrome,
    ] {
        let mut scene = layered_sprites(kind, [0, 1, 0]);
        scene.finish();
        assert_eq!(
            painted_tile_ids(&scene, kind),
            [30, 20, 10],
            "{kind:?} texture batching changed paint order"
        );
        assert_eq!(
            sprite_batches(&scene, kind),
            [(0, 0..1), (1, 1..2), (0, 2..3)],
            "only contiguous texture runs may be batched"
        );
    }
}

#[test]
fn star_bank_copy_bounds_cover_edge_pixels_under_layer_transforms_at_both_scales() {
    use gpui::{Bounds, LayerTransform, point, px, size};
    let original = Bounds::new(point(px(15.5625), px(12.1875)), size(px(1.25), px(1.5)));
    for scale in [1.0_f32, 2.0] {
        for transform in [
            LayerTransform::IDENTITY,
            LayerTransform {
                scale: size(0.875, 0.625),
                offset: point(px(-3.3), px(4.125)),
            },
            LayerTransform {
                scale: size(-0.875, -0.625),
                offset: point(px(33.3), px(24.125)),
            },
        ] {
            let covered = crate::graph::draw::star_copy_bounds(original, transform, scale)
                .expect("invertible transform");
            let painted = transform.apply_bounds(original);
            let copy = transform.apply_bounds(covered);
            let actual = [
                copy.origin.x.as_f32() * scale,
                copy.origin.y.as_f32() * scale,
                copy.bottom_right().x.as_f32() * scale,
                copy.bottom_right().y.as_f32() * scale,
            ];
            let expected = [
                (painted.origin.x.as_f32() * scale).floor(),
                (painted.origin.y.as_f32() * scale).floor(),
                (painted.bottom_right().x.as_f32() * scale).ceil(),
                (painted.bottom_right().y.as_f32() * scale).ceil(),
            ];
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!(
                    (actual - expected).abs() < 1e-5,
                    "path copy clips a touched device pixel: {actual} != {expected}"
                );
            }
        }
    }
}

#[test]
fn analytical_star_pixel_areas_conserve_flux_and_move_continuously() {
    for scale in [1.0_f32, 2.0] {
        for side in [1.25_f32, 1.5] {
            let mut previous: Option<f32> = None;
            for step in 0_u8..=64 {
                let pan = f32::from(step) / 64.0;
                let center = (pan - 0.25) * scale;
                let width = side * scale;
                let (rectangles, len) = crate::graph::draw::star_coverage_rectangles(
                    center - width / 2.0,
                    center - width / 2.0,
                    width,
                    width,
                );
                assert!((1..=9).contains(&len));
                let (mut area, mut moment) = (0.0, 0.0);
                for &([x, y, w, h], coverage) in &rectangles[..len] {
                    assert!(coverage > 0.0 && coverage <= 1.0);
                    assert_eq!(x, x.floor());
                    assert_eq!(y, y.floor());
                    assert_eq!(w, w.floor());
                    assert_eq!(h, h.floor());
                    let flux = w * h * coverage;
                    area += flux;
                    moment += (x + w / 2.0) * flux;
                }
                assert!(
                    (area - width * width).abs() < 1e-6,
                    "pixel overlap lost square area"
                );
                let centroid = moment / area / scale;
                assert!(
                    (centroid - (pan - 0.25)).abs() <= 0.1 / scale,
                    "pixel-cell centroid strays from the square's true center"
                );
                if let Some(prior) = previous {
                    assert!(
                        (centroid - prior).abs() <= 2.0 / 64.0 + 1e-6,
                        "coverage has a centroid jump at a pixel boundary"
                    );
                }
                previous = Some(centroid);
            }
        }
    }
}

#[test]
fn closed_find_history_has_identical_native_glyph_geometry() {
    let scripts = [
        "leave @0; key / @2000; type \"glyph::RelationLabel\" @2040; key enter @2080; key / @2200; type \"serde_json::de::from_str\" @2240; key enter @2280; key escape @2400; key escape @2520; leave @2600; leave @6000",
        "leave @0; key / @200; type \"glyph::RelationLabel\" @240; key enter @280; key / @1200; type \"serde_json::de::from_str\" @1240; key enter @1280; key escape @2400; key escape @2520; leave @2600; leave @6000",
    ];
    let scene = find("graph-pinned-world").expect("registered pinned world");
    let mut packets = Vec::new();
    let mut images = Vec::new();
    let mut atlas_packets = Vec::new();
    let directory = std::env::temp_dir().join("facet-graph-glyph-diagnostic");
    std::fs::create_dir_all(&directory).expect("create glyph evidence directory");
    for (case, script) in scripts.iter().enumerate() {
        let mut shot = Shot::new(&scene);
        shot.scale = 1;
        shot.probe = true;
        shot.times = vec![7600];
        shot.script = Some(Script::parse(script).expect("native history script"));
        run(&scene, &shot, &mut |tick, window, cx| {
            if tick.drawn.at_ms != 7600 {
                return Ok(());
            }
            let graph = cx
                .global::<super::Current>()
                .0
                .upgrade()
                .expect("mounted graph");
            let state = graph.read(cx).inspect(cx);
            assert_eq!(state.query, "serde_json::de::from_str");
            assert_eq!(state.find_scroll, (0.0, 0.0));
            assert_eq!(state.find_selection, (24, 24));
            assert!(!state.find_open);
            let scene = window.rendered_scene_for_test();
            let mut geometry = Vec::new();
            let mut evidence = Vec::new();
            let mut atlas = Vec::new();
            for glyph in &scene.monochrome_sprites {
                if glyph.bounds.origin.y.0 >= 45.0 || glyph.bounds.origin.x.0 >= 316.0 {
                    continue;
                }
                let shape = format!(
                    "{:?} {:?} {:?} {:?}",
                    glyph.bounds, glyph.content_mask, glyph.color, glyph.transformation
                );
                let bytes = window.read_atlas_tile_for_test(glyph.tile);
                #[cfg(target_os = "macos")]
                assert!(
                    bytes.is_some(),
                    "Metal must expose the live native glyph tile"
                );
                if let Some(bytes) = bytes {
                    let hash = format!("{:x}", Sha256::digest(&bytes));
                    let filename = format!("case-{case}-tile-{}.bin", atlas.len());
                    std::fs::write(directory.join(&filename), &bytes)
                        .expect("save atlas tile bytes");
                    evidence.push(format!(
                        "{} tile {:?} bytes={} sha256={} file={}",
                        shape,
                        glyph.tile,
                        bytes.len(),
                        hash,
                        filename
                    ));
                    atlas.push((
                        shape.clone(),
                        glyph.tile.bounds.size.width.0,
                        glyph.tile.bounds.size.height.0,
                        bytes,
                    ));
                } else {
                    evidence.push(format!(
                        "{} tile {:?} raw atlas readback unsupported",
                        shape, glyph.tile
                    ));
                }
                geometry.push(shape);
            }
            for glyph in &scene.subpixel_sprites {
                if glyph.bounds.origin.y.0 >= 45.0 || glyph.bounds.origin.x.0 >= 316.0 {
                    continue;
                }
                let shape = format!(
                    "subpixel {:?} {:?} {:?} {:?}",
                    glyph.bounds, glyph.content_mask, glyph.color, glyph.transformation
                );
                evidence.push(format!("{} tile {:?}", shape, glyph.tile));
                geometry.push(shape);
            }
            assert!(geometry.len() >= 20, "native field glyphs were not painted");
            geometry.sort();
            atlas.sort();
            packets.push(geometry);
            atlas_packets.push(atlas);
            std::fs::write(
                directory.join(format!("case-{case}.txt")),
                evidence.join("\n"),
            )
            .expect("save native sprite evidence");
            if let Some(image) = tick.image {
                image
                    .save(directory.join(format!("case-{case}.png")))
                    .expect("save native paint evidence");
                images.push(image.clone());
            }
            Ok(())
        })
        .expect("native glyph history capture");
    }
    assert_eq!(packets.len(), 2);
    assert_eq!(
        packets[0], packets[1],
        "native glyph geometry or color differs by history"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        atlas_packets[0].len(),
        packets[0].len(),
        "every native Metal field sprite needs raw tile evidence"
    );
    if !atlas_packets[0].is_empty() && !atlas_packets[1].is_empty() {
        assert_eq!(
            atlas_packets[0], atlas_packets[1],
            "native glyph atlas bitmap differs by history"
        );
    } else {
        println!("raw atlas byte comparison skipped: renderer does not support atlas readback");
    }
    assert_eq!(
        images.len(),
        2,
        "both native history screenshots must be captured"
    );
    assert_eq!(
        images[0].dimensions(),
        images[1].dimensions(),
        "native history screenshot dimensions differ"
    );
    let changed = images[0]
        .pixels()
        .zip(images[1].pixels())
        .filter(|(a, b)| a != b)
        .count();
    println!(
        "identical native field geometry; {changed} differing pixels; evidence {}",
        directory.display()
    );
    assert_eq!(
        changed,
        0,
        "native painting differs across navigation histories; evidence {}",
        directory.display()
    );
}

#[cfg(target_os = "macos")]
mod star_coverage {
    use gpui::{
        AnyWindowHandle, AppContext as _, Context, HeadlessAppContext, IntoElement,
        ParentElement as _, Render, Styled as _, Window, canvas, div, fill, point, px, rgb, size,
    };
    use std::sync::Arc;

    #[derive(Clone, Copy)]
    enum Variant {
        Quad,
        Path,
        PathOutward,
        Analytical,
    }

    impl Variant {
        fn name(self) -> &'static str {
            match self {
                Self::Quad => "quad",
                Self::Path => "path",
                Self::PathOutward => "path_outward",
                Self::Analytical => "analytical",
            }
        }
    }

    struct SingleStar {
        pan: f32,
        side: f32,
        variant: Variant,
    }

    /// The production tiny-star bank painter, mounted with one rectangle.
    /// This separates MSAA coverage from the final single-sample PathSprite
    /// cropping its fractional edge pixels.
    fn paint_path_outward(window: &mut Window, x: f32, y: f32, side: f32) {
        let mut batch = crate::paint::geom::Fill::new();
        batch.poly(&crate::paint::geom::Poly::rect(x, y, side, side));
        assert!(crate::graph::draw::paint_star_bank(
            window,
            batch,
            gpui::white()
        ));
    }

    impl Render for SingleStar {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let (pan, side, variant) = (self.pan, self.side, self.variant);
            div().size_full().bg(rgb(0x000000)).child(
                canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        // Exactly the graph's tiny-star quad path: a square
                        // centered on the projected world position.
                        let (x, y) = (16.0 + pan - side / 2.0, 16.0 + pan - side / 2.0);
                        match variant {
                            Variant::Quad => window.paint_quad(fill(
                                gpui::Bounds::new(point(px(x), px(y)), size(px(side), px(side))),
                                rgb(0xffffff),
                            )),
                            Variant::Path => crate::graph::draw::paint_star_path(
                                window,
                                x,
                                y,
                                side,
                                gpui::white(),
                            ),
                            Variant::PathOutward => paint_path_outward(window, x, y, side),
                            Variant::Analytical => {
                                crate::graph::draw::paint_star_coverage(
                                    window,
                                    x,
                                    y,
                                    side,
                                    gpui::white(),
                                );
                            }
                        }
                    },
                )
                .size_full(),
            )
        }
    }

    /// A diagnostic, not a requirement to preserve the current snapping defect.
    /// Use `--ignored --exact` to capture the real Metal coverage before/after a
    /// graph-star primitive change without mounting a graph or shaping labels.
    #[test]
    #[ignore = "native Metal single-star coverage evidence; run explicitly"]
    fn native_single_star_micro_pan_reports_flux_and_centroid_at_both_scales() {
        let directory = std::env::temp_dir().join("facet-single-star-diagnostic");
        std::fs::create_dir_all(&directory).expect("create single-star evidence directory");
        let mut evidence = Vec::new();
        for variant in [
            Variant::Quad,
            Variant::Path,
            Variant::PathOutward,
            Variant::Analytical,
        ] {
            for scale in [1_u8, 2] {
                for side in [1.25_f32, 1.5] {
                    let platform = gpui_platform::current_platform(true);
                    let mut cx = HeadlessAppContext::with_platform(
                        platform.text_system(),
                        Arc::new(crate::icons::Assets),
                        gpui_platform::current_headless_renderer,
                    );
                    cx.update(|cx| {
                        crate::gallery::bootstrap(crate::theme::Facet::default(), false, cx)
                    })
                    .expect("bootstrap single-star native renderer");
                    let window = cx
                        .open_window(size(px(32.0), px(32.0)), move |window, cx| {
                            window.set_scale_factor(f32::from(scale));
                            cx.new(|_| SingleStar {
                                pan: 0.0,
                                side,
                                variant,
                            })
                        })
                        .expect("open single-star native window");
                    let handle: AnyWindowHandle = window.into();
                    let mut fluxes = Vec::new();
                    for step in 0_u8..=16 {
                        let pan = f32::from(step) / 16.0;
                        let (snapped, path_bounds, path_vertices, image) = cx
                            .update_window(handle, |view, window, cx| {
                                view.downcast::<SingleStar>()
                                    .expect("single-star view")
                                    .update(cx, |star, cx| {
                                        star.pan = pan;
                                        cx.notify();
                                    });
                                window.refresh();
                                window.draw(cx).clear(cx);
                                let snapped: Vec<_> = window
                                    .rendered_scene_for_test()
                                    .quads
                                    .iter()
                                    .filter(|quad| quad.bounds.size.width.0 < 10.0)
                                    .map(|quad| {
                                        [
                                            quad.bounds.origin.x.0,
                                            quad.bounds.origin.y.0,
                                            quad.bounds.size.width.0,
                                            quad.bounds.size.height.0,
                                        ]
                                    })
                                    .collect();
                                let path_bounds: Vec<_> = window
                                    .rendered_scene_for_test()
                                    .paths
                                    .iter()
                                    .map(|path| {
                                        [
                                            path.bounds.origin.x.as_f32(),
                                            path.bounds.origin.y.as_f32(),
                                            path.bounds.size.width.as_f32(),
                                            path.bounds.size.height.as_f32(),
                                        ]
                                    })
                                    .collect();
                                let path_vertices: Vec<Vec<_>> = window
                                    .rendered_scene_for_test()
                                    .paths
                                    .iter()
                                    .map(|path| {
                                        path.vertices
                                            .iter()
                                            .map(|vertex| {
                                                [
                                                    vertex.xy_position.x.as_f32(),
                                                    vertex.xy_position.y.as_f32(),
                                                ]
                                            })
                                            .collect()
                                    })
                                    .collect();
                                window
                                    .render_to_image()
                                    .map(|image| (snapped, path_bounds, path_vertices, image))
                            })
                            .expect("draw single-star native frame")
                            .expect("read native Metal pixels");
                        let (mut flux, mut weighted_x, mut weighted_y) =
                            (0.0_f64, 0.0_f64, 0.0_f64);
                        let (mut left, mut top, mut right, mut bottom) =
                            (image.width(), image.height(), 0, 0);
                        for (x, y, pixel) in image.enumerate_pixels() {
                            assert_eq!(pixel[0], pixel[1], "white star must remain achromatic");
                            assert_eq!(pixel[0], pixel[2], "white star must remain achromatic");
                            let coverage = f64::from(pixel[0]) / 255.0;
                            flux += coverage;
                            weighted_x += coverage * (f64::from(x) + 0.5);
                            weighted_y += coverage * (f64::from(y) + 0.5);
                            if pixel[0] > 0 {
                                left = left.min(x);
                                top = top.min(y);
                                right = right.max(x + 1);
                                bottom = bottom.max(y + 1);
                            }
                        }
                        assert!(
                            flux > 0.0 && flux.is_finite(),
                            "star disappeared from its native image"
                        );
                        let logical_scale = f64::from(scale);
                        let filename = format!(
                            "{}-side-{side}-scale-{scale}-phase-{step:02}.png",
                            variant.name()
                        );
                        image
                            .save(directory.join(&filename))
                            .expect("save single-star native frame");
                        fluxes.push(flux);
                        evidence.push(serde_json::json!({
                        "variant": variant.name(), "scale": scale, "logical_side": side, "phase": pan,
                        "requested_center": [16.0 + pan, 16.0 + pan],
                        "snapped_device_bounds": snapped,
                        "quads": snapped.len(), "paths": path_bounds.len(),
                        "path_device_bounds": path_bounds, "path_device_vertices": path_vertices,
                        "device_flux": flux,
                        "logical_flux": flux / (logical_scale * logical_scale),
                        "ideal_device_flux": (f64::from(side) * logical_scale).powi(2),
                        "logical_centroid": [weighted_x / flux / logical_scale, weighted_y / flux / logical_scale],
                        "nonzero_device_bounds": [left, top, right - left, bottom - top],
                        "image": filename,
                    }));
                    }
                    println!(
                        "single-star {} side={side} scale={scale}: device flux {:.6}..{:.6}",
                        variant.name(),
                        fluxes.iter().copied().fold(f64::INFINITY, f64::min),
                        fluxes.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                    );
                }
            }
        }
        for exact in evidence.iter().filter(|row| row["variant"] == "path") {
            let outward = evidence
                .iter()
                .find(|row| {
                    row["variant"] == "path_outward"
                        && row["scale"] == exact["scale"]
                        && row["logical_side"] == exact["logical_side"]
                        && row["phase"] == exact["phase"]
                })
                .expect("matching outward path capture");
            assert_eq!(
                exact["path_device_vertices"], outward["path_device_vertices"],
                "outward diagnostic must preserve the exact painted triangles"
            );
        }
        std::fs::write(
            directory.join("samples.json"),
            serde_json::to_vec_pretty(&evidence).expect("encode single-star evidence"),
        )
        .expect("save single-star native coverage evidence");
        println!("single-star native evidence {}", directory.display());
    }
}
