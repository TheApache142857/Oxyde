//! The split of a cube into tetrahedra.

/// The six tetrahedra of a cube's Kuhn (Freudenthal) split, all around the diagonal from
/// corner (0, 0, 0) to (1, 1, 1): one per order of the axes. Neighbouring cubes split
/// this way share their faces' diagonals, so the tetrahedra conform.
pub fn kuhn_tets<T: Copy>(corner: impl Fn([usize; 3]) -> T) -> [[T; 4]; 6] {
    const ORDERS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    ORDERS.map(|order| {
        let mut d = [0usize; 3];
        let v0 = corner(d);
        if let Some(x) = d.get_mut(order[0]) {
            *x = 1;
        }
        let v1 = corner(d);
        if let Some(x) = d.get_mut(order[1]) {
            *x = 1;
        }
        let v2 = corner(d);
        let v3 = corner([1, 1, 1]);
        [v0, v1, v2, v3]
    })
}
