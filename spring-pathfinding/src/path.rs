/// A computed path: a list of world-space waypoints.
#[derive(Debug, Clone)]
pub struct Path {
    pub points: Vec<[f32; 2]>,
}

impl Path {
    pub fn empty() -> Self {
        Self { points: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Total path length in world units.
    pub fn total_length(&self) -> f32 {
        self.points
            .windows(2)
            .map(|w| {
                let dx = w[1][0] - w[0][0];
                let dz = w[1][1] - w[0][1];
                (dx * dx + dz * dz).sqrt()
            })
            .sum()
    }
}
