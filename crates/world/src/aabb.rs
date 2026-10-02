//! `net.minecraft.world.phys.AABB`.

/// Java's `Math.min` for doubles: NaN wins, and -0.0 is less than 0.0.
pub fn java_min(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        return a;
    }
    if a == 0.0 && b == 0.0 && a.is_sign_negative() {
        return a;
    }
    if a <= b { a } else { b }
}

/// Java's `Math.max` for doubles.
pub fn java_max(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        return a;
    }
    if a == 0.0 && b == 0.0 && a.is_sign_positive() {
        return a;
    }
    if a >= b { a } else { b }
}

/// `Direction.Axis`, in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The next two axes in cyclic order (`AxisCycle`): for X that is Y, Z;
    /// for Y it is Z, X; for Z it is X, Y.
    pub fn cycle(self) -> (Axis, Axis) {
        match self {
            Axis::X => (Axis::Y, Axis::Z),
            Axis::Y => (Axis::Z, Axis::X),
            Axis::Z => (Axis::X, Axis::Y),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min_x: f64,
    pub min_y: f64,
    pub min_z: f64,
    pub max_x: f64,
    pub max_y: f64,
    pub max_z: f64,
}

impl Aabb {
    /// The constructor sorts each axis, like vanilla's.
    pub fn new(x1: f64, y1: f64, z1: f64, x2: f64, y2: f64, z2: f64) -> Self {
        Self {
            min_x: java_min(x1, x2),
            min_y: java_min(y1, y2),
            min_z: java_min(z1, z2),
            max_x: java_max(x1, x2),
            max_y: java_max(y1, y2),
            max_z: java_max(z1, z2),
        }
    }

    pub fn min(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.min_x,
            Axis::Y => self.min_y,
            Axis::Z => self.min_z,
        }
    }

    pub fn max(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.max_x,
            Axis::Y => self.max_y,
            Axis::Z => self.max_z,
        }
    }

    pub fn expand_towards(&self, xa: f64, ya: f64, za: f64) -> Self {
        let (mut min_x, mut min_y, mut min_z) = (self.min_x, self.min_y, self.min_z);
        let (mut max_x, mut max_y, mut max_z) = (self.max_x, self.max_y, self.max_z);
        if xa < 0.0 {
            min_x += xa;
        } else if xa > 0.0 {
            max_x += xa;
        }
        if ya < 0.0 {
            min_y += ya;
        } else if ya > 0.0 {
            max_y += ya;
        }
        if za < 0.0 {
            min_z += za;
        } else if za > 0.0 {
            max_z += za;
        }
        Self::new(min_x, min_y, min_z, max_x, max_y, max_z)
    }

    pub fn move_by(&self, xa: f64, ya: f64, za: f64) -> Self {
        Self::new(self.min_x + xa, self.min_y + ya, self.min_z + za, self.max_x + xa, self.max_y + ya, self.max_z + za)
    }

    pub fn inflate(&self, x: f64, y: f64, z: f64) -> Self {
        Self::new(self.min_x - x, self.min_y - y, self.min_z - z, self.max_x + x, self.max_y + y, self.max_z + z)
    }

    pub fn deflate(&self, x: f64, y: f64, z: f64) -> Self {
        self.inflate(-x, -y, -z)
    }

    pub fn set_min_y(&self, min_y: f64) -> Self {
        Self::new(self.min_x, min_y, self.min_z, self.max_x, self.max_y, self.max_z)
    }

    pub fn set_max_y(&self, max_y: f64) -> Self {
        Self::new(self.min_x, self.min_y, self.min_z, self.max_x, max_y, self.max_z)
    }

    /// `intersects(double...)`: strict on every axis.
    pub fn intersects(&self, min_x: f64, min_y: f64, min_z: f64, max_x: f64, max_y: f64, max_z: f64) -> bool {
        self.min_x < max_x
            && self.max_x > min_x
            && self.min_y < max_y
            && self.max_y > min_y
            && self.min_z < max_z
            && self.max_z > min_z
    }

    pub fn intersects_aabb(&self, o: &Aabb) -> bool {
        self.intersects(o.min_x, o.min_y, o.min_z, o.max_x, o.max_y, o.max_z)
    }

    pub fn get_xsize(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn get_zsize(&self) -> f64 {
        self.max_z - self.min_z
    }
}
