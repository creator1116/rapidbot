//! Ports of `net.minecraft.util.Mth` and `Vec3`. Vanilla uses a sine lookup
//! table, so `f64::sin` would give different results.

use std::sync::OnceLock;

const SIN_SCALE: f64 = 10430.378350470453;

/// `(float) (Math.PI / 180.0)`, the degrees-to-radians factor vanilla
/// multiplies float angles by.
pub const DEG_TO_RAD: f32 = (std::f64::consts::PI / 180.0) as f32;

fn sin_table() -> &'static [f32; 65536] {
    static TABLE: OnceLock<Box<[f32; 65536]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = Box::new([0f32; 65536]);
        for (i, v) in t.iter_mut().enumerate() {
            *v = (i as f64 / SIN_SCALE).sin() as f32;
        }
        t
    })
}

/// `Mth.sin(double)`. Java's `(long)` cast saturates and maps NaN to 0, as
/// Rust's `as i64` does.
pub fn sin(x: f64) -> f32 {
    sin_table()[((x * SIN_SCALE) as i64 & 65535) as usize]
}

/// `Mth.cos(double)`.
pub fn cos(x: f64) -> f32 {
    sin_table()[((x * SIN_SCALE + 16384.0) as i64 & 65535) as usize]
}

/// `Mth.clamp(float)`: `value < min ? min : Math.min(value, max)`.
pub fn clamp_f32(v: f32, min: f32, max: f32) -> f32 {
    if v < min { min } else { java_min_f32(v, max) }
}

pub fn clamp_f64(v: f64, min: f64, max: f64) -> f64 {
    if v < min { min } else { rapidbot_world::aabb::java_min(v, max) }
}

fn java_min_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() {
        return a;
    }
    if a == 0.0 && b == 0.0 && a.is_sign_negative() {
        return a;
    }
    if a <= b { a } else { b }
}

/// `Mth.wrapDegrees(float)`: into [-180, 180).
pub fn wrap_degrees(angle: f32) -> f32 {
    let mut a = angle % 360.0;
    if a >= 180.0 {
        a -= 360.0;
    }
    if a < -180.0 {
        a += 360.0;
    }
    a
}

/// `Mth.fastInvSqrt(double)`.
fn fast_inv_sqrt(x: f64) -> f64 {
    let xhalf = 0.5 * x;
    let i = 6910469410427058090i64.wrapping_sub((x.to_bits() as i64) >> 1);
    let x = f64::from_bits(i as u64);
    x * (1.5 - xhalf * x * x)
}

/// `Mth.atan2`: vanilla's table-based approximation, not `f64::atan2`.
pub fn atan2(mut y: f64, mut x: f64) -> f64 {
    let d2 = x * x + y * y;
    if d2.is_nan() {
        return f64::NAN;
    }
    let neg_y = y < 0.0;
    if neg_y {
        y = -y;
    }
    let neg_x = x < 0.0;
    if neg_x {
        x = -x;
    }
    let steep = y > x;
    if steep {
        std::mem::swap(&mut x, &mut y);
    }
    let rinv = fast_inv_sqrt(d2);
    x *= rinv;
    y *= rinv;
    let frac_bias = f64::from_bits(4805340802404319232);
    let yp = frac_bias + y;
    // `(int) Double.doubleToRawLongBits(yp)`: the low 32 bits.
    let index = yp.to_bits() as u32 as i32 as usize;
    let phi = f64::from_bits(crate::mth_tables::ASIN_TAB[index]);
    let c_phi = f64::from_bits(crate::mth_tables::COS_TAB[index]);
    let s_phi = yp - frac_bias;
    let sd = y * c_phi - x * s_phi;
    let d = (6.0 + sd * sd) * sd * 0.16666666666666666;
    let mut theta = phi + d;
    if steep {
        theta = std::f64::consts::FRAC_PI_2 - theta;
    }
    if neg_x {
        theta = std::f64::consts::PI - theta;
    }
    if neg_y {
        theta = -theta;
    }
    theta
}

/// `Mth.floor(double)`.
pub fn floor(v: f64) -> i32 {
    v.floor() as i32
}

/// `Mth.lerp(float, float, float)`.
pub fn lerp_f32(delta: f32, a: f32, b: f32) -> f32 {
    a + delta * (b - a)
}

/// `Mth.equal(double, double)`: within `1.0E-5F`.
pub fn equal(a: f64, b: f64) -> bool {
    (b - a).abs() < 1.0e-5f32 as f64
}

/// `Mth.sqrt(float)`.
pub fn sqrt_f32(x: f32) -> f32 {
    (x as f64).sqrt() as f32
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub fn add(self, o: Vec3) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }

    pub fn subtract(self, x: f64, y: f64, z: f64) -> Self {
        Self::new(self.x - x, self.y - y, self.z - z)
    }

    pub fn scale(self, f: f64) -> Self {
        Self::new(self.x * f, self.y * f, self.z * f)
    }

    pub fn multiply(self, x: f64, y: f64, z: f64) -> Self {
        Self::new(self.x * x, self.y * y, self.z * z)
    }

    pub fn length_sqr(self) -> f64 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    pub fn horizontal_distance_sqr(self) -> f64 {
        self.x * self.x + self.z * self.z
    }

    /// `Vec3.normalize`: zero below `1.0E-5F`.
    pub fn normalize(self) -> Self {
        let dist = (self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        if dist < 1.0e-5f32 as f64 { Self::ZERO } else { Self::new(self.x / dist, self.y / dist, self.z / dist) }
    }

    pub fn get(self, axis: rapidbot_world::Axis) -> f64 {
        match axis {
            rapidbot_world::Axis::X => self.x,
            rapidbot_world::Axis::Y => self.y,
            rapidbot_world::Axis::Z => self.z,
        }
    }

    pub fn with(self, axis: rapidbot_world::Axis, v: f64) -> Self {
        match axis {
            rapidbot_world::Axis::X => Self::new(v, self.y, self.z),
            rapidbot_world::Axis::Y => Self::new(self.x, v, self.z),
            rapidbot_world::Axis::Z => Self::new(self.x, self.y, v),
        }
    }

    /// `Vec3.xRot(float)`.
    pub fn x_rot(self, radians: f32) -> Self {
        let cos = cos(radians as f64) as f64;
        let sin = sin(radians as f64) as f64;
        Self::new(self.x, self.y * cos + self.z * sin, self.z * cos - self.y * sin)
    }

    /// `Vec3.yRot(float)`.
    pub fn y_rot(self, radians: f32) -> Self {
        let cos = cos(radians as f64) as f64;
        let sin = sin(radians as f64) as f64;
        Self::new(self.x * cos + self.z * sin, self.y, self.z * cos - self.x * sin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_lookups() {
        assert_eq!(sin(0.0), 0.0);
        assert_eq!(cos(0.0), 1.0);
        assert_eq!(sin(std::f64::consts::FRAC_PI_2), 1.0);
        assert_eq!(sin(f64::NAN), 0.0);
    }

    #[test]
    fn sin_table_matches_vanilla() {
        let mut hash = 0i64;
        for v in sin_table().iter() {
            hash = hash.wrapping_mul(31).wrapping_add(v.to_bits() as i32 as i64);
        }
        assert_eq!(hash, crate::mth_tables::SIN_TABLE_HASH);
    }

    #[test]
    fn atan2_matches_vanilla_bit_for_bit() {
        // Reference values printed by net.minecraft.util.Mth.atan2 (26.3).
        // Vanilla's approximation is off from the true value by up to ~5e-7.
        let cases = [
            (1.0, 1.0, 0.7853981366411398),
            (-1.0, 2.0, -0.4636472634610012),
            (3.0, -0.5, 1.735944546704755),
            (-2.0, -7.0, -2.86329304374212),
            (0.0, 1.0, 0.0),
            (5.0, 0.0, 1.5707963267948966),
        ];
        for (y, x, expected) in cases {
            assert_eq!(atan2(y, x), expected, "atan2({y}, {x})");
        }
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap_degrees(190.0), -170.0);
        assert_eq!(wrap_degrees(-190.0), 170.0);
        assert_eq!(wrap_degrees(180.0), -180.0);
        assert_eq!(wrap_degrees(540.0), -180.0);
    }
}
