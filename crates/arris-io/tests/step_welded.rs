//! One vertex written as two `VERTEX_POINT`s at the same point, as
//! Inventor and SOLIDWORKS exports have it, reduced to a synthetic part in
//! `tests/data/welded` (a 20 x 20 x 10 block with a radius 4 hole, written
//! by Open CASCADE and edited with `kernel-lab/tools/make_welded_case.py`):
//!
//! - the hole's rim, a closed circle edge, ends at a second vertex;
//! - a straight edge ends at a second vertex, the next edge of its loops
//!   starting at the first.
//!
//! Either way the loop cannot close. A healing read welds the two and
//! names the one welded away; a read that does not heal refuses the solid.
//!
//! Also a sliver face, one loop of the same segment twice, in
//! `tests/data/sliver`: dropped by a healing read.

use arris_check::{Level, check};
use arris_debug::unmetered::{mass_properties, step_read};
use arris_io::step::{Heal, ReadOptions, Refusal};
use arris_topo::Model;
use core::f64::consts::PI;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/welded");
const CASES: [&str; 2] = [
    "closed_edge_two_vertices.step",
    "junction_two_vertices.step",
];

fn read_one(name: &str, heal: Heal) -> (Model, Result<arris_io::step::ReadBody, Refusal>) {
    let text = std::fs::read_to_string(format!("{DATA}/{name}")).unwrap();
    let mut m = Model::default();
    let options = ReadOptions {
        heal,
        ..ReadOptions::default()
    };
    let read = step_read(&mut m, &text, &options).unwrap();
    assert_eq!(read.solids.len(), 1, "{name}");
    let result = read.solids.into_iter().next().unwrap().result;
    (m, result)
}

#[test]
fn a_vertex_written_twice_at_one_point_is_welded_when_healing() {
    let expected = 20.0 * 20.0 * 10.0 - PI * 16.0 * 10.0;
    for name in CASES {
        let (m, result) = read_one(name, Heal::GapsAndOrientation);
        let read = result.unwrap_or_else(|r| panic!("{name}: {r}"));
        let v = mass_properties(&m, read.body).unwrap().volume;
        assert!((v - expected).abs() < 1e-9 * expected, "{name}: volume {v}");
        assert_eq!(read.healing.welded.len(), 1, "{name}: the one twin named");
        assert!(read.healing.pinched.is_empty(), "{name}");
        // The block's eight corners and the hole's two rim vertices.
        assert_eq!(m.vertices(read.body).unwrap().len(), 10, "{name}");
        let report = check(&m, read.body, Level::Full);
        assert!(report.is_ok(), "{name}: {report:?}");
    }
}

#[test]
fn a_sliver_face_of_one_segment_twice_is_dropped_when_healing() {
    // A 10 cube one of whose edges is doubled, a plane face between the
    // edge and its copy (`kernel-lab/tools/make_sliver_case.py`).
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/sliver/sliver_cube.step"
    ))
    .unwrap();
    let mut m = Model::default();
    let read = step_read(&mut m, &text, &ReadOptions::default()).unwrap();
    let back = read.solids[0]
        .result
        .clone()
        .unwrap_or_else(|r| panic!("{r}"));
    let v = mass_properties(&m, back.body).unwrap().volume;
    assert!((v - 1000.0).abs() < 1e-9, "volume {v}");
    assert_eq!(back.healing.slivers.len(), 1);
    assert_eq!(m.faces(back.body).unwrap().len(), 6);
    assert_eq!(m.edges(back.body).unwrap().len(), 12);
    let report = check(&m, back.body, Level::Full);
    assert!(report.is_ok(), "{report:?}");

    let mut m = Model::default();
    let off = ReadOptions {
        heal: Heal::Off,
        ..ReadOptions::default()
    };
    let read = step_read(&mut m, &text, &off).unwrap();
    assert!(read.solids[0].result.is_err());
}

#[test]
fn a_vertex_written_twice_at_one_point_is_refused_without_healing() {
    for name in CASES {
        let (_, result) = read_one(name, Heal::Off);
        assert!(result.is_err(), "{name}");
    }
}
