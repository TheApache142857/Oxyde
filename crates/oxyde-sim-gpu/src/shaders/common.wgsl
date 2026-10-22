// What every kernel shares: its parameters (a slot of one uniform buffer, chosen by a
// dynamic offset) and how it finds its record from a dispatch that may be two-dimensional.

struct Params {
    // Cells of the level along X, Y and Z, and the records the dispatch covers.
    nx: u32,
    ny: u32,
    nz: u32,
    count: u32,
    // Products: 0 for A x, 1 for b − A x, 2 for c1 out + c2 D⁻¹ (b − A x).
    mode: u32,
    // A slot of the scalars (dot products).
    slot: u32,
    pad0: u32,
    pad1: u32,
    c1: f32,
    c2: f32,
    pad2: f32,
    pad3: f32,
    // Restriction and prolongation: the coarse level's cells and the factor along each axis.
    cx: u32,
    cy: u32,
    cz: u32,
    pad4: u32,
    fx: u32,
    fy: u32,
    fz: u32,
    pad5: u32,
}

@group(1) @binding(0) var<uniform> p: Params;

// A node no fixture holds, and one no cell touches (its row the identity).
const FREE: u32 = 0xffffffffu;
const INACTIVE: u32 = 0xfffffffeu;
// A cell with no matrix; the bit marking a scalar one.
const NONE: u32 = 0xffffffffu;
const SCALAR: u32 = 0x80000000u;

// The scalars' slots.
const RZ: u32 = 0u;
const PQ: u32 = 1u;
const ALPHA: u32 = 2u;
const BETA: u32 = 3u;
const RZ_NEXT: u32 = 4u;
const RR: u32 = 5u;
const COUNTER: u32 = 6u;

const GROUP: u32 = 256u;

// The record of this invocation: dispatches wider than 65 535 groups go on in Y.
fn record(gid: vec3<u32>, groups: vec3<u32>) -> u32 {
    return gid.x + gid.y * groups.x * GROUP;
}
