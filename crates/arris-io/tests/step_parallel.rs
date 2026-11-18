//! A read that cannot be stopped builds its solids ahead, over rayon, when
//! the `parallel` feature is on, each in a model of its own, and imports
//! them in the file's order. What it reads is the sequential read's —
//! the same solids in the same order, the same refusals, the same measures
//! and the same provenance by file entity — and its ids are the same on
//! every run. A read with a budget is never split: these tests force the
//! sequential read with a budget no read reaches. Without the feature both
//! reads are sequential and the tests hold trivially.

use arris_debug::sample;
use arris_debug::unmetered::mass_properties;
use arris_io::step::{self, Read, ReadOptions};
use arris_math::{Control, Point3};
use arris_topo::Model;
use arris_topo::provenance::Origin;

/// A file of several solids: a cylinder, a box, a NURBS box and a sphere,
/// enough to give a pool of threads something to share.
fn file() -> String {
    let mut m = Model::default();
    let bodies = [
        sample::cylinder(&mut m, 4.0, 12.0).unwrap(),
        sample::cuboid(&mut m, Point3::new(10.0, 0.0, 0.0), Point3::new(14.0, 3.0, 2.0)).unwrap(),
        sample::cuboid_nurbs(&mut m, Point3::new(-9.0, -2.0, 0.0), Point3::new(-5.0, 2.0, 3.0))
            .unwrap(),
        sample::sphere(&mut m, Point3::new(0.0, 20.0, 0.0), 3.0).unwrap(),
    ];
    step::write(&m, &bodies).unwrap()
}

fn read(text: &str, control: &Control<'_>) -> (Model, Read) {
    let mut m = Model::default();
    let read = step::read(&mut m, text, &ReadOptions::default(), control).unwrap();
    (m, read)
}

/// Each solid's volume, area and file entities of its faces, in order.
fn summary(m: &Model, read: &Read) -> Vec<(f64, f64, Vec<u64>, usize)> {
    (read.solids.iter())
        .map(|s| {
            let body = s.result.as_ref().unwrap();
            let props = mass_properties(m, body.body).unwrap();
            let mut files: Vec<u64> = (body.provenance.origins_recorded())
                .filter_map(|o| match o {
                    Origin::Role(arris_topo::provenance::Role::File(f)) => Some(f.id),
                    _ => None,
                })
                .collect();
            files.sort_unstable();
            (props.volume, props.area, files, body.healing.turned.len())
        })
        .collect()
}

#[test]
fn a_parallel_read_reads_what_a_sequential_one_does() {
    let text = file();
    let (pm, parallel) = read(&text, &Control::NONE);
    let (sm, sequential) = read(&text, &Control::budget(u64::MAX));
    assert_eq!(parallel.solids.len(), 4);
    assert_eq!(parallel.solids.len(), sequential.solids.len());
    for (p, s) in parallel.solids.iter().zip(&sequential.solids) {
        assert_eq!(p.entity, s.entity, "the file's order of solids");
    }
    for (p, s) in summary(&pm, &parallel).into_iter().zip(summary(&sm, &sequential)) {
        // The measures to rounding: an imported body's vertices may be
        // listed in another order, which a sum over them rounds by.
        assert!((p.0 - s.0).abs() <= 1e-12 * s.0, "volume {} against {}", p.0, s.0);
        assert!((p.1 - s.1).abs() <= 1e-12 * s.1, "area {} against {}", p.1, s.1);
        assert_eq!(p.2, s.2, "the file entities its faces came from");
        assert_eq!(p.3, s.3, "the faces healing turned");
    }
}

#[test]
fn a_parallel_read_has_the_same_ids_every_time() {
    let text = file();
    let (_, first) = read(&text, &Control::NONE);
    for _ in 0..3 {
        let (_, again) = read(&text, &Control::NONE);
        for (a, b) in first.solids.iter().zip(&again.solids) {
            let (a, b) = (a.result.as_ref().unwrap(), b.result.as_ref().unwrap());
            assert_eq!(a.body, b.body);
            assert_eq!(a.provenance, b.provenance);
        }
    }
}
