//! Beta comparison with deterministic endpoint-aware quadrature.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Beta {
    pub alpha: f64,
    pub beta: f64,
}

pub fn lgamma(z: f64) -> f64 {
    const C: [f64; 9] = [
        0.9999999999998099,
        676.5203681218851,
        -1259.1392167224028,
        771.3234287776531,
        -176.6150291621406,
        12.507343278686905,
        -0.13857109526572012,
        9.984369578019572e-6,
        1.5056327351493116e-7,
    ];
    if z < 0.5 {
        return std::f64::consts::PI.ln() - (std::f64::consts::PI * z).sin().ln() - lgamma(1.0 - z);
    }
    let z = z - 1.0;
    let x = C
        .iter()
        .enumerate()
        .skip(1)
        .fold(C[0], |x, (i, c)| x + c / (z + i as f64));
    let t = z + 7.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (z + 0.5) * t.ln() - t + x.ln()
}

fn log_beta(a: f64, b: f64) -> f64 {
    lgamma(a) + lgamma(b) - lgamma(a + b)
}

fn fraction(a: f64, b: f64, x: f64) -> f64 {
    let tiny = 1e-300;
    let nonzero = |v: f64| if v.abs() < tiny { tiny.copysign(v) } else { v };
    let mut c = 1.0;
    let mut d = 1.0 / nonzero(1.0 - (a + b) * x / (a + 1.0));
    let mut h = d;
    for m in 1..=10000 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((a + m2 - 1.0) * (a + m2));
        d = 1.0 / nonzero(1.0 + aa * d);
        c = nonzero(1.0 + aa / c);
        h *= d * c;
        let aa = -(a + m) * (a + b + m) * x / ((a + m2) * (a + m2 + 1.0));
        d = 1.0 / nonzero(1.0 + aa * d);
        c = nonzero(1.0 + aa / c);
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < 2e-14 {
            return h;
        }
    }
    panic!("incomplete beta did not converge")
}

fn cdf_logs(a: f64, b: f64, log_x: f64, log_one_minus_x: f64, norm: f64) -> f64 {
    if log_x == f64::NEG_INFINITY {
        return 0.0;
    }
    if log_one_minus_x == f64::NEG_INFINITY {
        return 1.0;
    }
    let x = log_x.exp();
    let front = (a * log_x + b * log_one_minus_x - norm).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        (front * fraction(a, b, x) / a).clamp(0.0, 1.0)
    } else {
        (1.0 - front * fraction(b, a, log_one_minus_x.exp()) / b).clamp(0.0, 1.0)
    }
}

fn simpson(
    f: &impl Fn(f64) -> f64,
    lo: f64,
    hi: f64,
    values: [f64; 3],
    tol: f64,
    depth: u32,
) -> f64 {
    let [a, m, b] = values;
    let mid = (lo + hi) / 2.0;
    let l = f((lo + mid) / 2.0);
    let r = f((mid + hi) / 2.0);
    let whole = (hi - lo) * (a + 4.0 * m + b) / 6.0;
    let left = (mid - lo) * (a + 4.0 * l + m) / 6.0;
    let right = (hi - mid) * (m + 4.0 * r + b) / 6.0;
    let delta = left + right - whole;
    if delta.abs() <= 15.0 * tol || depth == 0 {
        return left + right + delta / 15.0;
    }
    simpson(f, lo, mid, [a, l, m], tol / 2.0, depth - 1)
        + simpson(f, mid, hi, [m, r, b], tol / 2.0, depth - 1)
}

fn half(c: Beta, d: Beta, right: bool, tolerance: f64) -> f64 {
    let shape = if right { c.beta } else { c.alpha };
    let exponent = (1.0 / shape).max(1.0);
    let norm = log_beta(c.alpha, c.beta);
    let d_norm = log_beta(d.alpha, d.beta);
    let f = |t: f64| {
        if t == 0.0 {
            if shape * exponent > 1.0 + 1e-12 || !right {
                return 0.0;
            }
            return (exponent.ln() - shape * 2.0f64.ln() - norm).exp();
        }
        let log_u = -2.0f64.ln() + exponent * t.ln();
        let log_other = (-log_u.exp()).ln_1p();
        let (lx, ly) = if right {
            (log_other, log_u)
        } else {
            (log_u, log_other)
        };
        let density = ((c.alpha - 1.0) * lx + (c.beta - 1.0) * ly - norm + exponent.ln()
            - 2.0f64.ln()
            + (exponent - 1.0) * t.ln())
        .exp();
        density * cdf_logs(d.alpha, d.beta, lx, ly, d_norm)
    };
    // Seed subdivisions also find narrow interior modes before the adaptive error estimate.
    (0..64)
        .map(|i| {
            let lo = i as f64 / 64.0;
            let hi = (i + 1) as f64 / 64.0;
            simpson(
                &f,
                lo,
                hi,
                [f(lo), f((lo + hi) / 2.0), f(hi)],
                tolerance / 128.0,
                30,
            )
        })
        .sum()
}

/// P(C > D). Canonical orientation enforces complementarity independently of argument order.
pub fn probability(c: Beta, d: Beta, tolerance: f64) -> f64 {
    assert!(c.alpha > 0.0 && c.beta > 0.0 && d.alpha > 0.0 && d.beta > 0.0);
    if c == d {
        return 0.5;
    }
    if c.alpha
        .total_cmp(&d.alpha)
        .then_with(|| c.beta.total_cmp(&d.beta))
        .is_gt()
    {
        return 1.0 - probability(d, c, tolerance);
    }
    (half(c, d, false, tolerance) + half(c, d, true, tolerance)).clamp(0.0, 1.0)
}
