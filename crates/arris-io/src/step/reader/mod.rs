//! The STEP reader (ADR-0025): what a parsed exchange structure
//! ([`super::part21`]) means, entity by entity. This module holds the
//! reader's public vocabulary — [`read`], the options a caller reads
//! with, what it returns, and the typed refusal each solid the reader
//! cannot take comes back as — and its layers: [`assembly`] flattens the
//! product structure to every placement of every solid, [`entities`] resolves
//! references and reads parameters by the schema's types, [`units`] reads
//! a representation context's units and converts every length and angle
//! to the caller's, [`geometry`] maps each curve and surface onto its
//! Arris variant or refuses it by name, and [`topology`] reads one solid
//! into a body.

pub(crate) mod assembly;
mod colours;
pub(crate) mod entities;
pub(crate) mod geometry;
mod products;
pub(crate) mod topology;
pub(crate) mod units;

pub use products::{FaceColour, Occurrence, ProductTree, Rgb};

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use arris_check::Report;
use arris_math::{Control, Interrupted, Meter};
use arris_topo::provenance::FileEntity;
use arris_topo::{Body, EntityId, FaceId, Model, Provenance};

use super::part21::{self, Part21Error};
use entities::Entities;
use geometry::Geometry;
use topology::Halt;
use units::Units;

/// How the reader reads a file: the unit the caller's model is in, and
/// whether it heals what it reads.
///
/// ```
/// use arris_io::step::{Heal, LengthUnit, ReadOptions};
///
/// let options = ReadOptions::default();
/// assert_eq!(options.length_unit, LengthUnit::Millimetre);
/// assert_eq!(options.heal, Heal::GapsAndOrientation);
/// let inches = ReadOptions { length_unit: LengthUnit::Inch, ..ReadOptions::default() };
/// assert_ne!(inches, options);
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadOptions {
    /// The unit every length the reader returns is in: a file in any
    /// other converts to it (ADR-0025 §5). Millimetres by default, the
    /// unit [`write`](super::write) declares.
    pub length_unit: LengthUnit,
    /// What the reader heals (ADR-0058): on by default, as Open
    /// CASCADE's reader heals every read.
    pub heal: Heal,
    /// How thoroughly each solid is measured as it is read
    /// ([`ReadCheck`]): [`ReadCheck::Full`] by default.
    pub check: ReadCheck,
    /// Solids read otherwise than [`check`](ReadOptions::check) says, by
    /// the `#id` of their file entity (a `MANIFOLD_SOLID_BREP`, say): a
    /// part to read fast in an assembly read in full, or the reverse.
    pub check_for: BTreeMap<u64, ReadCheck>,
    /// The largest gap a healing read heals, as a fraction of the part's
    /// size: `arris_math::HEAL_GAP_FRACTION` (`5e-4`) where `None`, the
    /// default. A file whose exporter left gaps past it — Inventor
    /// simulation exports leave up to `2.2e-3` — is refused with
    /// [`Refusal::Gap`] unless the caller raises it, to
    /// [`LOOSE_HEAL_GAP_FRACTION`] say. What a read grows is named in
    /// [`Healing::grown`] either way.
    pub heal_ceiling: Option<f64>,
}

impl ReadOptions {
    /// How the solid of file entity `solid` is to be read: its entry in
    /// [`check_for`](ReadOptions::check_for), else
    /// [`check`](ReadOptions::check).
    pub fn check_of(&self, solid: u64) -> ReadCheck {
        self.check_for.get(&solid).copied().unwrap_or(self.check)
    }

    /// The healing ceiling a read uses: [`heal_ceiling`](ReadOptions::heal_ceiling),
    /// else `arris_math::HEAL_GAP_FRACTION`.
    pub fn ceiling(&self) -> f64 {
        self.heal_ceiling.unwrap_or(arris_math::HEAL_GAP_FRACTION)
    }
}

/// How thoroughly the reader measures a solid. Either way the body passes
/// the checker and its tolerances cover every gap measured; they differ
/// in how many points are measured and how closely each pcurve is fitted.
///
/// ```
/// use arris_io::step::{ReadCheck, ReadOptions};
///
/// let mut options = ReadOptions { check: ReadCheck::Fast, ..ReadOptions::default() };
/// options.check_for.insert(17, ReadCheck::Full);
/// assert_eq!(options.check_of(17), ReadCheck::Full);
/// assert_eq!(options.check_of(18), ReadCheck::Fast);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReadCheck {
    /// Every gap measured at the checker's samples and at the ones a
    /// pcurve fit is held to, each peak climbed to its top, and every
    /// pcurve fitted to the model's default tolerance where the file
    /// allows: the reader's own standard.
    #[default]
    Full,
    /// Gaps measured at the checker's samples only, no peak climbed, and
    /// pcurves fitted to [`FAST_FIT_FACTOR`] times the default tolerance
    /// (at most the gap cap), the edges' tolerances grown to what is
    /// measured. Faster on free-form parts; a body for which a coarse
    /// figure is enough (a clearance check, a preview). A solid the fast
    /// read refuses is read again in full, and its [`ReadBody::check`]
    /// says so.
    Fast,
}

/// The factor a [`ReadCheck::Fast`] read fits pcurves to, over the model's
/// default tolerance: `1e-4` of a length unit at Arris's `1e-7`.
pub const FAST_FIT_FACTOR: f64 = 1000.0;

/// A healing ceiling, as a fraction of the part's size, for files whose
/// exporter left gaps past `arris_math::HEAL_GAP_FRACTION`
/// ([`ReadOptions::heal_ceiling`]): `5e-3`, `2.3×` the largest gap seen in
/// Autodesk Inventor simulation exports (`2.2e-3` of a small part's size),
/// and ten times the default. Gaps that large are the exporter's
/// tolerance, which the read carries as the entities' tolerances, not
/// moved geometry; a boolean on such a body works to those tolerances.
pub const LOOSE_HEAL_GAP_FRACTION: f64 = 5e-3;

/// What the reader heals (ADR-0058). Healing never moves geometry and
/// never heals silently: what it did is the [`ReadBody::healing`] record.
///
/// ```
/// use arris_io::step::Heal;
///
/// assert_eq!(Heal::default(), Heal::GapsAndOrientation);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Heal {
    /// Nothing: a gap past the read cap — the part's size times
    /// `arris_math::READ_GAP_FRACTION`, at most the model's
    /// `max_tolerance` — is [`Refusal::Gap`], as before healing.
    Off,
    /// A vertex or an edge lying past the read cap off the entities it
    /// meets has its tolerance grown to the gap, up to the healing
    /// ceiling — the part's size times `arris_math::HEAL_GAP_FRACTION`,
    /// which the model's `max_tolerance` does not bound — and is named in
    /// [`Healing::grown`]. A gap past the ceiling is [`Refusal::Gap`]. A
    /// face that walks every edge it shares with others (two at least)
    /// the way the face across it does, beside faces that agree, has its
    /// walk turned back — what Open CASCADE's writer does to a face
    /// reversed on a left-handed surface frame — and is named in
    /// [`Healing::turned`]; a face that disagrees on some shared edges
    /// and not all, or one of two faces each against the other, is still
    /// [`Refusal::Topology`].
    #[default]
    GapsAndOrientation,
}

/// What the reader healed in one body (ADR-0058 §3): empty where the file
/// needed nothing. Provenance is unchanged by it — every entity is still
/// generated from its file entity — since healing changes no topology.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Healing {
    /// Every vertex and edge whose tolerance the read grew past the read
    /// cap, largest gap first.
    pub grown: Vec<Grown>,
    /// Every face the read turned.
    pub turned: Vec<FaceId>,
    /// Every `VERTEX_POINT` (its file `#id`) where separate parts of a
    /// shell touch at that one point — the faces around it forming two or
    /// more fans that share no edge there — split into one vertex per fan,
    /// all at the file's point: what a weldment's members touching at a
    /// corner export as. Nothing moved; the shell becomes a manifold one.
    pub pinched: Vec<u64>,
    /// Every `VERTEX_POINT` (its file `#id`) welded into another at the
    /// same point that the topology says is the same vertex: the second
    /// vertex of an edge that starts and ends at one point (a closed edge
    /// written with a vertex at each end), or of a loop junction written
    /// with a vertex for each edge. Nothing moved; the loop closes. Two
    /// loops of one face that then touch at a vertex are joined into one.
    pub welded: Vec<u64>,
    /// Every face (its file `#id`) dropped as a sliver: one loop of two
    /// straight edges between the same two vertices, the same segment
    /// twice, of no area. The faces either side meet at the one edge.
    pub slivers: Vec<u64>,
}

impl Healing {
    /// The read healed nothing.
    ///
    /// ```
    /// use arris_io::step::Healing;
    ///
    /// assert!(Healing::default().is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.grown.is_empty()
            && self.turned.is_empty()
            && self.pinched.is_empty()
            && self.welded.is_empty()
            && self.slivers.is_empty()
    }
}

/// One entity a read grew the tolerance of (ADR-0058 §3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grown {
    /// The vertex or edge.
    pub entity: EntityId,
    /// The `#id` of the file entity it was read from, as its provenance
    /// names it: its `VERTEX_POINT` or `EDGE_CURVE`, or the face of an
    /// edge the file left out.
    pub file: u64,
    /// The tolerance it was grown to: the gap measured at it, in the
    /// caller's unit.
    pub gap: f64,
}

/// A unit of length a caller's model is in (ADR-0025 §5).
///
/// Guarantees: the conversion between two units is their exact ratio
/// where it is a representable number — millimetres to millimetres is
/// `1`, inches to millimetres `25.4`, metres to millimetres `1000` — and
/// the nearest `f64` to it otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LengthUnit {
    /// 10⁻⁶ m.
    Micrometre,
    /// 10⁻³ m.
    #[default]
    Millimetre,
    /// 10⁻² m.
    Centimetre,
    /// 1 m.
    Metre,
    /// 25.4 mm.
    Inch,
    /// 304.8 mm.
    Foot,
}

impl LengthUnit {
    /// The unit as `mantissa · 10^exponent` metres, so that a ratio of two
    /// units divides the mantissas and adds the exponents: exact wherever
    /// the ratio is.
    pub(crate) fn scale(self) -> units::Scale {
        let (mantissa, exponent) = match self {
            LengthUnit::Micrometre => (1.0, -6),
            LengthUnit::Millimetre => (1.0, -3),
            LengthUnit::Centimetre => (1.0, -2),
            LengthUnit::Metre => (1.0, 0),
            LengthUnit::Inch => (25.4, -3),
            LengthUnit::Foot => (304.8, -3),
        };
        units::Scale { mantissa, exponent }
    }
}

/// Why the reader did not return a solid, naming the file entity
/// (`#id`) where it stopped (ADR-0025 §2). One refused solid never hides
/// another: a refusal is per solid, and only a parse error fails a file.
///
/// Guarantees: every refusal names an instance of the file; [`kind`]
/// is its fieldless twin, which a histogram counts.
///
/// [`kind`]: Refusal::kind
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Refusal {
    /// A representation context declares no length unit, so no length in
    /// it has a size.
    #[error("#{context}: the context declares no length unit")]
    NoLengthUnit {
        /// The context.
        context: u64,
    },
    /// An `OFFSET_SURFACE` or an `OFFSET_CURVE_3D`: Arris has no offset
    /// variant, and a fitted one would not be the part (ADR-0025 §2).
    #[error("#{entity}: {name} is an offset, which Arris does not hold")]
    Offset {
        /// The entity.
        entity: u64,
        /// Its type.
        name: String,
    },
    /// A composite curve or surface: `COMPOSITE_CURVE`,
    /// `COMPOSITE_CURVE_ON_SURFACE` and its boundary subtypes,
    /// `RECTANGULAR_COMPOSITE_SURFACE`.
    #[error("#{entity}: {name} is a composite, which Arris does not hold")]
    Composite {
        /// The entity.
        entity: u64,
        /// Its type.
        name: String,
    },
    /// A `CURVE_BOUNDED_SURFACE`.
    #[error("#{entity}: {name} is a curve-bounded surface, which Arris does not hold")]
    CurveBounded {
        /// The entity.
        entity: u64,
        /// Its type.
        name: String,
    },
    /// A `DEGENERATE_TOROIDAL_SURFACE`: a torus whose tube meets its axis.
    #[error("#{entity}: a degenerate torus, which Arris does not hold")]
    DegenerateTorus {
        /// The entity.
        entity: u64,
    },
    /// A torus whose major radius is not above zero, written as a
    /// `TOROIDAL_SURFACE` or turned from a circle. A horn or a spindle
    /// torus (`R <= r`, `R > 0`) is read: Arris holds the regular band of
    /// a torus, `R > 0` and `r > 0` (ADR-0080).
    #[error(
        "#{entity}: a torus of major radius {major} and minor radius {minor} has no regular band"
    )]
    SelfIntersectingTorus {
        /// The entity.
        entity: u64,
        /// `R`, in the caller's unit.
        major: f64,
        /// `r`, in the caller's unit.
        minor: f64,
    },
    /// An entity where the subset has none of its type: outside the
    /// AP203/214/242 B-Rep subset the reader maps (ADR-0025 §1), or a
    /// B-spline of a degree Arris does not hold.
    #[error("#{entity}: {name} is outside the subset the reader maps")]
    Unsupported {
        /// The entity.
        entity: u64,
        /// Its type, or what about it is outside.
        name: String,
    },
    /// Geometry of the subset whose values describe nothing Arris can
    /// hold: a radius that is not positive, a direction of zero length,
    /// a line extruded along itself, a knot vector that is not one.
    #[error("#{entity}: {what}")]
    Degenerate {
        /// The entity.
        entity: u64,
        /// What is wrong with it.
        what: String,
    },
    /// The solid's faces, bounds and edges do not make closed shells a
    /// solid can have: a loop that does not close, an edge not used
    /// twice and in opposite directions — that orientation healing does
    /// not turn back ([`Heal`]), or a face it turned back that then fails
    /// the checker — a shell in two pieces, two shells sharing an edge:
    /// what the builder refuses (`arris_topo::builder::BuildError`), said
    /// in its words.
    #[error("#{entity}: {what}")]
    Topology {
        /// The solid.
        entity: u64,
        /// What does not close or pair up.
        what: String,
    },
    /// An edge has no pcurve on a face it bounds: it is not on the face's
    /// surface within the tolerance, it runs through a singular point of
    /// it, or the fit does not reach the tolerance (`arris_geom::pcurve_on`).
    #[error("#{edge} on face #{face}: {what}")]
    Pcurve {
        /// The `EDGE_CURVE`.
        edge: u64,
        /// The face.
        face: u64,
        /// Why.
        what: String,
    },
    /// A bound's walk in (u, v) jumps between two of its edges where
    /// they meet, and not along a singular row of the face's surface —
    /// a pole, an apex, a NURBS surface's collapsed row — where the
    /// reader rebuilds the degenerate edge the file left out: the edges
    /// meet in 3D but not on the face.
    #[error(
        "#{face}: its bound #{bound} jumps in (u, v) at vertex #{vertex}, not along a singular row"
    )]
    OpenLoop {
        /// The face.
        face: u64,
        /// The bound.
        bound: u64,
        /// The `VERTEX_POINT` where the walk jumps.
        vertex: u64,
    },
    /// A vertex or an edge is farther from where the entities it meets
    /// put it than the gap a read closes by a tolerance: with
    /// [`Heal::Off`], the part's size times
    /// `arris_math::READ_GAP_FRACTION`, and never more than the model's
    /// `max_tolerance` (ADR-0025 §4); healing, the part's size times
    /// `arris_math::HEAL_GAP_FRACTION` (ADR-0058). Closing it would mean
    /// moving geometry, which healing does not.
    #[error("#{entity}: a gap of {gap} past the cap of {cap}")]
    Gap {
        /// The `VERTEX_POINT` or `EDGE_CURVE` — or, for an edge the file
        /// left out, its face.
        entity: u64,
        /// The gap measured, in the caller's unit.
        gap: f64,
        /// The cap it is past.
        cap: f64,
    },
    /// The body read fails the checker at `Level::Fast`, which the
    /// reader runs in every build (ADR-0025 §5): the file's fault, not
    /// the kernel's, so a refusal rather than a panic.
    #[error("#{entity}: the solid read fails the checker:\n{report}")]
    Invalid {
        /// The solid.
        entity: u64,
        /// The checker's report.
        report: Box<Report>,
    },
    /// An instance is not what the schema says belongs where it is: a
    /// parameter of the wrong type or count, a reference to an entity
    /// the file does not define or of the wrong type, a unit nested past
    /// any file's need.
    #[error("#{entity}: {what}")]
    Malformed {
        /// The instance.
        entity: u64,
        /// What is wrong with it.
        what: String,
    },
}

/// The fieldless twin of [`Refusal`]: what a refusal histogram counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefusalKind {
    /// [`Refusal::NoLengthUnit`].
    NoLengthUnit,
    /// [`Refusal::Malformed`].
    Malformed,
    /// [`Refusal::Offset`].
    Offset,
    /// [`Refusal::Composite`].
    Composite,
    /// [`Refusal::CurveBounded`].
    CurveBounded,
    /// [`Refusal::DegenerateTorus`].
    DegenerateTorus,
    /// [`Refusal::SelfIntersectingTorus`].
    SelfIntersectingTorus,
    /// [`Refusal::Unsupported`].
    Unsupported,
    /// [`Refusal::Degenerate`].
    Degenerate,
    /// [`Refusal::Topology`].
    Topology,
    /// [`Refusal::Pcurve`].
    Pcurve,
    /// [`Refusal::OpenLoop`].
    OpenLoop,
    /// [`Refusal::Gap`].
    Gap,
    /// [`Refusal::Invalid`].
    Invalid,
}

impl RefusalKind {
    /// Every kind, in declaration order: what a refusal histogram
    /// iterates so that a kind no file met still has its row.
    ///
    /// ```
    /// use arris_io::step::RefusalKind;
    ///
    /// assert!(RefusalKind::ALL.contains(&RefusalKind::Offset));
    /// assert!(RefusalKind::ALL.windows(2).all(|w| w[0] < w[1]));
    /// ```
    pub const ALL: [RefusalKind; 14] = [
        RefusalKind::NoLengthUnit,
        RefusalKind::Malformed,
        RefusalKind::Offset,
        RefusalKind::Composite,
        RefusalKind::CurveBounded,
        RefusalKind::DegenerateTorus,
        RefusalKind::SelfIntersectingTorus,
        RefusalKind::Unsupported,
        RefusalKind::Degenerate,
        RefusalKind::Topology,
        RefusalKind::Pcurve,
        RefusalKind::OpenLoop,
        RefusalKind::Gap,
        RefusalKind::Invalid,
    ];

    /// Its place in [`RefusalKind::ALL`]: an exhaustive match, so a kind
    /// added without its row fails to compile here.
    pub fn index(self) -> usize {
        match self {
            RefusalKind::NoLengthUnit => 0,
            RefusalKind::Malformed => 1,
            RefusalKind::Offset => 2,
            RefusalKind::Composite => 3,
            RefusalKind::CurveBounded => 4,
            RefusalKind::DegenerateTorus => 5,
            RefusalKind::SelfIntersectingTorus => 6,
            RefusalKind::Unsupported => 7,
            RefusalKind::Degenerate => 8,
            RefusalKind::Topology => 9,
            RefusalKind::Pcurve => 10,
            RefusalKind::OpenLoop => 11,
            RefusalKind::Gap => 12,
            RefusalKind::Invalid => 13,
        }
    }
}

impl Refusal {
    /// Which refusal this is.
    ///
    /// ```
    /// use arris_io::step::{Refusal, RefusalKind};
    ///
    /// let r = Refusal::NoLengthUnit { context: 12 };
    /// assert_eq!(r.kind(), RefusalKind::NoLengthUnit);
    /// assert_eq!(r.to_string(), "#12: the context declares no length unit");
    /// ```
    pub fn kind(&self) -> RefusalKind {
        match self {
            Refusal::NoLengthUnit { .. } => RefusalKind::NoLengthUnit,
            Refusal::Malformed { .. } => RefusalKind::Malformed,
            Refusal::Offset { .. } => RefusalKind::Offset,
            Refusal::Composite { .. } => RefusalKind::Composite,
            Refusal::CurveBounded { .. } => RefusalKind::CurveBounded,
            Refusal::DegenerateTorus { .. } => RefusalKind::DegenerateTorus,
            Refusal::SelfIntersectingTorus { .. } => RefusalKind::SelfIntersectingTorus,
            Refusal::Unsupported { .. } => RefusalKind::Unsupported,
            Refusal::Degenerate { .. } => RefusalKind::Degenerate,
            Refusal::Topology { .. } => RefusalKind::Topology,
            Refusal::Pcurve { .. } => RefusalKind::Pcurve,
            Refusal::OpenLoop { .. } => RefusalKind::OpenLoop,
            Refusal::Gap { .. } => RefusalKind::Gap,
            Refusal::Invalid { .. } => RefusalKind::Invalid,
        }
    }

    /// The file instance the refusal names.
    pub fn entity(&self) -> u64 {
        match self {
            Refusal::NoLengthUnit { context } => *context,
            Refusal::Malformed { entity, .. }
            | Refusal::Offset { entity, .. }
            | Refusal::Composite { entity, .. }
            | Refusal::CurveBounded { entity, .. }
            | Refusal::DegenerateTorus { entity }
            | Refusal::SelfIntersectingTorus { entity, .. }
            | Refusal::Unsupported { entity, .. }
            | Refusal::Degenerate { entity, .. }
            | Refusal::Topology { entity, .. }
            | Refusal::Gap { entity, .. }
            | Refusal::Invalid { entity, .. } => *entity,
            Refusal::Pcurve { edge, .. } => *edge,
            Refusal::OpenLoop { face, .. } => *face,
        }
    }
}

impl fmt::Display for RefusalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RefusalKind::NoLengthUnit => "no length unit",
            RefusalKind::Malformed => "malformed",
            RefusalKind::Offset => "offset",
            RefusalKind::Composite => "composite",
            RefusalKind::CurveBounded => "curve-bounded surface",
            RefusalKind::DegenerateTorus => "degenerate torus",
            RefusalKind::SelfIntersectingTorus => "self-intersecting torus",
            RefusalKind::Unsupported => "unsupported entity",
            RefusalKind::Degenerate => "degenerate geometry",
            RefusalKind::Topology => "not a closed shell",
            RefusalKind::Pcurve => "no pcurve",
            RefusalKind::OpenLoop => "open loop",
            RefusalKind::Gap => "gap past the cap",
            RefusalKind::Invalid => "fails the checker",
        })
    }
}

/// Why [`read`] read nothing: the file is not a Part 21 exchange
/// structure. Past parsing, nothing fails the whole file — each solid is
/// its own result ([`ReadSolid`]).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ReadError {
    /// The text does not parse, at the line and column named.
    #[error(transparent)]
    Parse(#[from] Part21Error),
    /// The caller's poll or budget stopped the read (ADR-0030). Unlike a
    /// refusal, which is one solid's, an interrupt is the whole call's:
    /// the model is as it was before it, the solids already read
    /// included.
    #[error("{0}")]
    Interrupted(Interrupted),
}

impl From<Interrupted> for ReadError {
    fn from(stop: Interrupted) -> Self {
        ReadError::Interrupted(stop)
    }
}

/// What [`read`] returns: one result per solid of the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Read {
    /// Every solid of the file at every placement an assembly puts it —
    /// the product structure flattened, one body per instance (ADR-0025
    /// §5) — and every shape standing where a solid would that the reader
    /// refuses — a faceted B-rep — ascending
    /// by the file entity, then by the placement (its paths from the
    /// root assembly in order, [`FileEntity::instance`]).
    pub solids: Vec<ReadSolid>,
    /// The file's assemblies: each root's occurrences of products, their
    /// names and placements, holding indices into [`solids`](Read::solids)
    /// (ADR-0033). One root with one occurrence for a file of one part.
    pub products: ProductTree,
}

/// One solid of a file, read or refused.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadSolid {
    /// The solid's file entity, and which placement of it.
    pub entity: FileEntity,
    /// The length uncertainty the solid's representation context claims,
    /// in the caller's unit: the file's claim, kept beside the result and
    /// never an entity's tolerance (ADR-0025 §4). `None` where the
    /// context claims none or could not be read.
    pub uncertainty: Option<f64>,
    /// The body, or why there is none.
    pub result: Result<ReadBody, Refusal>,
}

/// A solid or a surface model read into the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadBody {
    /// The body: a `Solid` — or a `Sheet` for a
    /// `SHELL_BASED_SURFACE_MODEL` or `FACE_BASED_SURFACE_MODEL` — that
    /// passes the checker at `Level::Fast`.
    pub body: Body,
    /// Its record: the body, each shell, face, edge and vertex
    /// `Generated` from the file entity it was read from
    /// (`Role::File`), at the solid's placement.
    pub provenance: Provenance,
    /// What the read healed in it: empty unless the file needed it.
    pub healing: Healing,
    /// How it was read: what [`ReadOptions::check_of`] asked, or
    /// [`ReadCheck::Full`] where a fast read refused it and the full one
    /// did not.
    pub check: ReadCheck,
}

/// The entity names of the solids the reader reads.
const SOLIDS: [&str; 4] = [
    "MANIFOLD_SOLID_BREP",
    "BREP_WITH_VOIDS",
    "SHELL_BASED_SURFACE_MODEL",
    "FACE_BASED_SURFACE_MODEL",
];

/// The entity names that stand where a solid would, which the reader
/// counts and refuses (ADR-0025 §2): a faceted B-rep and its tessellated
/// relatives of AP242. The surface models read as sheets (ADR-0057).
const REFUSED_SOLIDS: [&str; 3] = ["FACETED_BREP", "TESSELLATED_SOLID", "TESSELLATED_SHELL"];

/// Reads every solid of the Part 21 file `text` into `model`, lengths in
/// `options.length_unit` (ADR-0025).
///
/// Guarantees: every `Ok` body is a `Solid`, or a `Sheet` where the file
/// holds a surface model, that passes the checker at `Level::Fast`, in
/// every build, with provenance naming the file entity
/// each of its entities came from; a solid that cannot be read is a
/// [`Refusal`] naming the file entity where it stopped, and leaves nothing
/// in the model; one refused solid never hides another. Deterministic:
/// the same text reads to the same entities with the same ids.
///
/// Errors: [`ReadError::Parse`] when the text is not a Part 21 exchange
/// structure — the only failure of the file's own — and
/// [`ReadError::Interrupted`] when `control`'s poll or budget stops the
/// call (a step is a solid placement, a file edge or face, a pcurve
/// fitted; ADR-0030), which leaves `model` as it was, the solids already
/// read dropped with it.
///
/// ```
/// use arris_io::step::{self, ReadOptions};
/// use arris_check::{check, Level};
/// use arris_topo::Model;
/// use arris_math::Control;
/// use arris_debug::sample;
///
/// let mut m = Model::default();
/// let body = sample::cylinder(&mut m, 4.0, 12.0)?;
/// let text = step::write(&m, &[body])?;
///
/// let mut back = Model::default();
/// let read = step::read(&mut back, &text, &ReadOptions::default(), &Control::NONE)?;
/// assert_eq!(read.solids.len(), 1);
/// let solid = read.solids[0].result.as_ref().unwrap();
/// assert!(check(&back, solid.body, Level::Full).is_ok());
/// assert_eq!(back.faces(solid.body)?.len(), 3);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn read(
    model: &mut Model,
    text: &str,
    options: &ReadOptions,
    control: &Control<'_>,
) -> Result<Read, ReadError> {
    let exchange = part21::parse(text)?;
    let mut meter = Meter::new(control);
    model.transaction(|model| read_solids(model, &exchange, options, &mut meter))
}

/// [`read`] past the parse, inside the one transaction an interrupt rolls
/// back.
fn read_solids(
    model: &mut Model,
    exchange: &part21::Exchange,
    options: &ReadOptions,
    meter: &mut Meter<'_>,
) -> Result<Read, ReadError> {
    let entities = Entities::new(&exchange.instances);
    // The solids, and what stands where a solid would and is refused.
    let mut solids: BTreeSet<u64> = BTreeSet::new();
    let mut refused: BTreeSet<u64> = BTreeSet::new();
    for (&id, instance) in &exchange.instances {
        let names = instance.records();
        if names.iter().any(|r| SOLIDS.contains(&r.name.as_str())) {
            solids.insert(id);
        } else if names
            .iter()
            .any(|r| REFUSED_SOLIDS.contains(&r.name.as_str()))
        {
            solids.insert(id);
            refused.insert(id);
        }
    }
    let mut units: BTreeMap<u64, Result<Units, Refusal>> = BTreeMap::new();
    let mut units_of = |context: u64| {
        units
            .entry(context)
            .or_insert_with(|| Units::of_context(&entities, context, options.length_unit))
            .clone()
    };
    let flat = assembly::placements(&exchange.instances, &entities, &solids, &mut units_of);
    let mut out = Vec::with_capacity(flat.placed.len());
    let mut index: BTreeMap<(u64, u32), usize> = BTreeMap::new();
    for p in &flat.placed {
        index.insert((p.solid, p.instance), index.len());
    }
    let mut ahead = built_ahead(model, &flat.placed, &refused, entities, options, meter);
    for (k, p) in flat.placed.iter().enumerate() {
        meter.tick()?;
        let entity = FileEntity {
            id: p.solid,
            instance: p.instance,
        };
        let context_units = p.context.map(&mut units_of);
        let uncertainty = match &context_units {
            Some(Ok(u)) => u.uncertainty,
            _ => None,
        };
        let result = if refused.contains(&p.solid) {
            let name = exchange
                .instances
                .get(&p.solid)
                .map_or_else(String::new, entities::describe);
            Err(Refusal::Unsupported {
                entity: p.solid,
                name,
            })
        } else {
            match (context_units, p.motion.clone()) {
                (None, _) => Err(entities::malformed(
                    p.solid,
                    "no representation holds the solid, so it has no units",
                )),
                (Some(Err(r)), _) | (Some(Ok(_)), Err(r)) => Err(r),
                (Some(Ok(units)), Ok(motion)) => match ahead
                    .get_mut(k)
                    .and_then(Option::take)
                    .map(|built| imported(model, p.solid, built))
                    .unwrap_or_else(|| {
                        let geometry = Geometry {
                            entities,
                            units: units.placed(motion),
                        };
                        read_solid(&geometry, model, p, options, meter)
                    }) {
                    Ok((s, check)) => Ok(ReadBody {
                        body: s.body,
                        provenance: s.provenance,
                        healing: s.healing,
                        check,
                    }),
                    Err(Halt::Refused(refusal)) => Err(refusal),
                    Err(Halt::Interrupted(stop)) => return Err(stop.into()),
                },
            }
        };
        out.push(ReadSolid {
            entity,
            uncertainty,
            result,
        });
    }
    let mut products = products::build(&exchange.instances, &flat, &index, meter)?;
    colours::Colours::of(&exchange.instances).apply(&mut products, &out);
    Ok(Read {
        solids: out,
        products,
    })
}

/// A solid built ahead of the read's loop, in a model of its own, or the
/// refusal its build ended in.
type Ahead = Result<(Model, topology::Solid, ReadCheck), Refusal>;

/// The solid `p` read into `model` as `options` ask for it: fast where
/// [`ReadOptions::check_of`] says so, and again in full where the fast
/// read refuses it for anything but a gap past the cap (the refused
/// attempt leaves `model` as it was). With the check it was read by.
fn read_solid(
    geometry: &Geometry<'_>,
    model: &mut Model,
    p: &assembly::Placed,
    options: &ReadOptions,
    meter: &mut Meter<'_>,
) -> Result<(topology::Solid, ReadCheck), Halt> {
    let check = options.check_of(p.solid);
    let first = geometry.solid(
        model,
        p.solid,
        p.instance,
        options.heal,
        options.ceiling(),
        check,
        meter,
    );
    match (first, check) {
        // A gap past the cap is the file's: the full read measures every
        // point the fast one does and more, so it would refuse it too.
        (Err(Halt::Refused(refusal)), ReadCheck::Fast) if refusal.kind() == RefusalKind::Gap => {
            Err(Halt::Refused(refusal))
        }
        (Err(Halt::Refused(_)), ReadCheck::Fast) => geometry
            .solid(
                model,
                p.solid,
                p.instance,
                options.heal,
                options.ceiling(),
                ReadCheck::Full,
                meter,
            )
            .map(|s| (s, ReadCheck::Full)),
        (first, _) => first.map(|s| (s, check)),
    }
}

/// Every placed solid the reader builds, built ahead over rayon, each in a
/// model of its own with `model`'s precision — where the `parallel` feature
/// is on and `meter` can never stop the read, since a stop has to land
/// where a sequential read's would. Indexed as `placed`; a solid the loop
/// refuses without building it, or one whose units or placement do not
/// read, is `None` and left to the loop. Empty otherwise: the loop builds
/// every solid itself.
#[cfg(feature = "parallel")]
fn built_ahead(
    model: &Model,
    placed: &[assembly::Placed],
    refused: &BTreeSet<u64>,
    entities: Entities<'_>,
    options: &ReadOptions,
    meter: &Meter<'_>,
) -> Vec<Option<Ahead>> {
    use rayon::prelude::*;
    if !meter.is_unbounded() || placed.len() < 2 {
        return Vec::new();
    }
    let precision = model.precision();
    placed
        .par_iter()
        .map(|p| {
            if refused.contains(&p.solid) {
                return None;
            }
            let units = Units::of_context(&entities, p.context?, options.length_unit).ok()?;
            let motion = p.motion.clone().ok()?;
            let mut scratch = Model::new(precision).ok()?;
            let mut own = Meter::new(&Control::NONE);
            let geometry = Geometry {
                entities,
                units: units.placed(motion),
            };
            match read_solid(&geometry, &mut scratch, p, options, &mut own) {
                Ok((solid, check)) => Some(Ok((scratch, solid, check))),
                Err(Halt::Refused(refusal)) => Some(Err(refusal)),
                Err(Halt::Interrupted(_)) => None,
            }
        })
        .collect()
}

#[cfg(not(feature = "parallel"))]
fn built_ahead(
    _: &Model,
    _: &[assembly::Placed],
    _: &BTreeSet<u64>,
    _: Entities<'_>,
    _: &ReadOptions,
    _: &Meter<'_>,
) -> Vec<Option<Ahead>> {
    Vec::new()
}

/// A solid built ahead, imported into `model`: its body copied with every
/// entity and value it reaches, its provenance and healing record moved to
/// the copy's ids. The ids are the import's, in the file's order of solids
/// whatever the order they were built in — the same for every run and every
/// number of threads, though not those of a sequential build, which keeps
/// in the model the values a build made and did not use.
fn imported(
    model: &mut Model,
    solid: u64,
    built: Ahead,
) -> Result<(topology::Solid, ReadCheck), Halt> {
    let (scratch, built, check) = built.map_err(Halt::Refused)?;
    let (body, map) = model.import(&scratch, built.body).map_err(|e| {
        Halt::Refused(Refusal::Topology {
            entity: solid,
            what: format!("the solid built ahead does not import: {e}"),
        })
    })?;
    let healing = Healing {
        grown: (built.healing.grown.into_iter())
            .filter_map(|g| {
                Some(Grown {
                    entity: map.map_entity(g.entity)?,
                    ..g
                })
            })
            .collect(),
        turned: (built.healing.turned.iter())
            .filter_map(|f| map.faces.get(f).copied())
            .collect(),
        pinched: built.healing.pinched,
        welded: built.healing.welded,
        slivers: built.healing.slivers,
    };
    Ok((
        topology::Solid {
            body,
            provenance: built.provenance.mapped(&map),
            healing,
        },
        check,
    ))
}

#[cfg(test)]
mod tests {
    use super::RefusalKind;

    #[test]
    fn every_kind_is_in_all_at_its_index() {
        for (i, kind) in RefusalKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), i, "{kind}");
        }
    }
}
