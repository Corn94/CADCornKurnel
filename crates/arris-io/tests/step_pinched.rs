//! A solid whose shell touches itself at one vertex: four square bars in a
//! ring whose two ends meet at the corner (0,2,2) and nowhere else, in
//! `tests/data/pinched` (written by Open CASCADE from the fused bars, with
//! `kernel-lab/tools/make_pinched_case.py`). SOLIDWORKS and Inventor export
//! weldments and bodies touching at a corner this way: every edge used
//! twice, but at the corner two separate fans of faces share the one
//! `VERTEX_POINT`. A healing read splits the vertex, one per fan, at the
//! file's point; a read that does not heal refuses the shell.

use arris_check::{Level, check};
use arris_debug::unmetered::{mass_properties, step_read};
use arris_io::step::{Heal, ReadOptions, Refusal, RefusalKind};
use arris_topo::Model;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/pinched");

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
fn a_shell_touching_itself_at_a_vertex_reads_with_the_vertex_split() {
    let (m, result) = read_one("pinched_ring.step", Heal::GapsAndOrientation);
    let read = result.unwrap();
    // 10x2x2 + 2x8x2 + 12x2x2 + 2x6x2.
    let v = mass_properties(&m, read.body).unwrap().volume;
    assert!((v - 144.0).abs() < 1e-9, "volume {v}");
    assert_eq!(read.healing.pinched.len(), 1, "the one corner named");
    assert!(read.healing.grown.is_empty() && read.healing.turned.is_empty());
    assert!(!read.healing.is_empty());
    // The split vertices sit at the one corner, and the solid checks.
    let corner = arris_math::Point3::new(0.0, 2.0, 2.0);
    let at_corner = m
        .vertices(read.body)
        .unwrap()
        .iter()
        .filter(|v| (m.vertex(v.id).unwrap().point() - corner).norm() == 0.0)
        .count();
    assert_eq!(at_corner, 2, "one vertex per fan, both at the file's point");
    let report = check(&m, read.body, Level::Full);
    assert!(report.is_ok(), "{report:?}");
}

#[test]
fn a_shell_touching_itself_at_a_vertex_is_refused_without_healing() {
    let (_, result) = read_one("pinched_ring.step", Heal::Off);
    let refusal = result.unwrap_err();
    assert_eq!(refusal.kind(), RefusalKind::Topology, "{refusal}");
}
