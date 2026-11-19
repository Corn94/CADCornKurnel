//! The fast read (`ReadCheck::Fast`): fewer points measured and looser
//! pcurve fits, chosen for the whole read or per solid. A fast body passes
//! the checker, measures as the full read's to well within its looser
//! tolerances, and records that it was read fast.

use arris_check::{Level, check};
use arris_debug::unmetered::mass_properties;
use arris_io::step::{self, ReadCheck, ReadOptions};
use arris_math::Control;
use arris_topo::Model;

/// NIST's FTC-07: free-form faces whose pcurves are fitted, the part the
/// fast read is for.
const PART: &str = "../../tests/fixtures/real/nist-ftc-07/nist_ftc_07_asme1_rd.stp";

fn read(options: &ReadOptions) -> (Model, step::Read) {
    let text = std::fs::read_to_string(PART).unwrap();
    let mut m = Model::default();
    let read = step::read(&mut m, &text, options, &Control::NONE).unwrap();
    (m, read)
}

#[test]
fn a_fast_read_passes_the_checker_and_measures_as_the_full_one() {
    let (fm, fast) = read(&ReadOptions {
        check: ReadCheck::Fast,
        ..ReadOptions::default()
    });
    let (sm, full) = read(&ReadOptions::default());
    assert_eq!(fast.solids.len(), full.solids.len());
    for (f, s) in fast.solids.iter().zip(&full.solids) {
        let (f, s) = (f.result.as_ref().unwrap(), s.result.as_ref().unwrap());
        assert_eq!(f.check, ReadCheck::Fast);
        assert_eq!(s.check, ReadCheck::Full);
        assert!(check(&fm, f.body, Level::Fast).is_ok());
        let (vf, vs) = (
            mass_properties(&fm, f.body).unwrap().volume,
            mass_properties(&sm, s.body).unwrap().volume,
        );
        assert!((vf - vs).abs() <= 1e-6 * vs, "fast {vf} against full {vs}");
        assert_eq!(
            fm.faces(f.body).unwrap().len(),
            sm.faces(s.body).unwrap().len()
        );
    }
}

#[test]
fn a_solid_can_be_read_otherwise_than_the_rest() {
    let (_, plain) = read(&ReadOptions::default());
    let solid = plain.solids[0].entity.id;

    let mut fast_one = ReadOptions::default();
    fast_one.check_for.insert(solid, ReadCheck::Fast);
    let (_, read_fast) = read(&fast_one);
    assert_eq!(
        read_fast.solids[0].result.as_ref().unwrap().check,
        ReadCheck::Fast
    );

    let mut full_one = ReadOptions {
        check: ReadCheck::Fast,
        ..ReadOptions::default()
    };
    full_one.check_for.insert(solid, ReadCheck::Full);
    let (_, read_full) = read(&full_one);
    assert_eq!(
        read_full.solids[0].result.as_ref().unwrap().check,
        ReadCheck::Full
    );
}

#[test]
fn a_body_written_by_the_application_reads_back_with_the_fast_check() {
    use arris_io::body;
    let (m, read) = read(&ReadOptions::default());
    let solid = read.solids[0].result.as_ref().unwrap();
    let bytes = body::write(&m, solid.body, &solid.provenance).unwrap();
    let mut back = Model::default();
    let fast = body::read_checked(&mut back, &bytes, &Control::NONE, Level::Fast).unwrap();
    assert_eq!(
        mass_properties(&back, fast.body).unwrap().volume,
        mass_properties(&m, solid.body).unwrap().volume
    );
    assert_eq!(fast.provenance, solid.provenance);
}
