use crate::discriminant::{bigint_from_le, bigint_to_fixed_le, bit_len, hash_prime, u64_low_word};
use crate::error::{Error, Result};
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{One, Signed, Zero};

pub const MAX_D_BITS: usize = 1024;
pub const FORM_SIZE: usize = MAX_D_BITS.div_ceil(32) * 3 + 4;
pub const B_BITS: usize = 264;
pub const B_BYTES: usize = B_BITS.div_ceil(8);

const BQFC_B_SIGN: u8 = 1;
const BQFC_T_SIGN: u8 = 1 << 1;
const BQFC_IS_1: u8 = 1 << 2;
const BQFC_IS_GEN: u8 = 1 << 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub a: BigInt,
    pub b: BigInt,
    pub c: BigInt,
}

impl Form {
    pub fn from_abd(a: BigInt, b: BigInt, discriminant: &BigInt) -> Result<Self> {
        if a <= BigInt::zero() {
            return Err(Error::InvalidForm);
        }

        let divisor = &a << 2usize;
        let numerator = &b * &b - discriminant;
        if !numerator.is_multiple_of(&divisor) {
            return Err(Error::InvalidForm);
        }

        let mut form = Self {
            a,
            b,
            c: numerator / divisor,
        };
        form.reduce();
        Ok(form)
    }

    pub fn identity(discriminant: &BigInt) -> Result<Self> {
        Self::from_abd(BigInt::one(), BigInt::one(), discriminant)
    }

    pub fn generator(discriminant: &BigInt) -> Result<Self> {
        Self::from_abd(BigInt::from(2u8), BigInt::one(), discriminant)
    }

    pub fn reduce(&mut self) {
        normalize(&mut self.a, &mut self.b, &mut self.c);
        while self.a > self.c || (self.a == self.c && self.b < BigInt::zero()) {
            reduce_impl(&mut self.a, &mut self.b, &mut self.c);
        }
        normalize(&mut self.a, &mut self.b, &mut self.c);
    }

    pub fn is_reduced(&self) -> bool {
        (self.a < self.c || (self.a == self.c && self.b >= BigInt::zero()))
            && (self.a.abs() > self.b.abs() || self.a == self.b)
    }

    pub fn check_valid(&self, discriminant: &BigInt) -> bool {
        &self.b * &self.b - BigInt::from(4u8) * &self.a * &self.c == *discriminant
    }

    /// Squaring via NUDUPL (FLINT `qfb_nudupl`): the doubling specialization of NUCOMP, with the same
    /// partial-GCD bound keeping intermediates near `|D|^(1/4)`. Handles the degenerate `gcd(a,b) > 1`
    /// case natively through its `s` branch (the historic direct-formula pitfall — see `vdf_square_fix`).
    pub fn square(&self) -> Result<Self> {
        let discriminant = &self.b * &self.b - ((&self.a * &self.c) << 2usize);
        let l = nucomp_bound(&discriminant);
        self.square_with(&discriminant, &l)
    }

    pub fn square_with(&self, discriminant: &BigInt, l: &BigInt) -> Result<Self> {
        use crate::limbs::SwGcd as G;
        use crate::limbs::SwWide as W;
        #[cfg(feature = "phase-profile")]
        let mut _pp = phase_profile::Stamp::new();
        let ga = G::from_bigint(&self.a);
        let mut gb = G::from_bigint(&self.b);
        let (s, v2) = if gb.cmp_mag(&ga) == core::cmp::Ordering::Equal {
            (ga, G::zero())
        } else if gb.is_negative() {
            // gcdinv on |b|, then negate the cofactor.
            gb.negate();
            let (g, mut v) = lehmer_gcdinv_sw(&gb, &ga);
            gb.negate();
            v.negate();
            (g, v)
        } else {
            lehmer_gcdinv_sw(&gb, &ga)
        };
        #[cfg(feature = "phase-profile")]
        _pp.lap(0); // gcdinv

        let wb = gb.resize::<34>();
        let mut wa1 = ga.resize::<34>();
        let mut wc1 = W::from_bigint(&self.c);
        let mut wk = v2.resize::<34>().mul(&wc1);
        wk.negate();
        if !s.is_one() {
            let ws = s.resize::<34>();
            wa1 = wa1.div_exact(&ws);
            wc1 = wc1.mul(&ws);
        }
        let (_, k_mod) = wk.div_mod_floor(&wa1);
        let wk = k_mod;
        #[cfg(feature = "phase-profile")]
        _pp.lap(1); // k prep (mul + div_exact + div_mod_floor)

        let wl = W::from_bigint(l);
        let (mut ca, cb, mut cc);
        if wa1.cmp_mag(&wl) == core::cmp::Ordering::Less {
            // Small a1: direct doubling, no partial reduction needed.
            let t = wa1.mul(&wk);
            ca = wa1.mul(&wa1);
            cb = t.shl1().add(&wb);
            cc = wb.add(&t).mul(&wk).add(&wc1).div_exact(&wa1);
        } else {
            // Partial reduction: shrink (a1, k) to ~|D|^(1/4), then assemble (all limb-native).
            let mut gr2 = wa1.resize::<20>();
            let mut gr1 = wk.resize::<20>();
            let (gco2, gco1) = xgcd_partial_sw(&mut gr2, &mut gr1, &G::from_bigint(l));
            #[cfg(feature = "phase-profile")]
            _pp.lap(2); // xgcd_partial
            let wr1 = gr1.resize::<34>();
            let wco1 = gco1.resize::<34>();
            let wco2 = gco2.resize::<34>();
            let t = wa1.mul(&wr1);
            let m2 = wb.mul(&wr1).sub(&wc1.mul(&wco1)).div_exact(&wa1);
            let r1r1 = wr1.mul(&wr1);
            let co1m2 = wco1.mul(&m2);
            ca = if wco1.is_negative() {
                r1r1.sub(&co1m2)
            } else {
                co1m2.sub(&r1r1)
            };
            let b = t.sub(&ca.mul(&wco2)).shl1().div_exact(&wco1).sub(&wb);
            let (_, cbr) = b.div_mod_floor(&ca.shl1());
            cb = cbr;
            let wd = W::from_bigint(discriminant);
            let four = {
                let mut f = W::one();
                f = f.shl1().shl1();
                f
            };
            let (q4, _) = cb.mul(&cb).sub(&wd).div_exact(&ca).div_mod_floor(&four);
            cc = q4;
            if ca.is_negative() {
                ca.negate();
                cc.negate();
            }
        }
        #[cfg(feature = "phase-profile")]
        _pp.lap(3); // composition assembly
        let mut result = Self {
            a: ca.to_bigint(),
            b: cb.to_bigint(),
            c: cc.to_bigint(),
        };
        result.reduce();
        #[cfg(feature = "phase-profile")]
        _pp.lap(4); // to_bigint + reduce
        Ok(result)
    }

    pub fn multiply(&self, rhs: &Self) -> Result<Self> {
        self.compose(rhs)
    }

    fn compose(&self, rhs: &Self) -> Result<Self> {
        // D = b² − 4ac (identical for both operands); L = |D|^(1/4), NUCOMP's partial-reduction bound.
        let discriminant = &self.b * &self.b - ((&self.a * &self.c) << 2usize);
        let l = nucomp_bound(&discriminant);
        self.multiply_with(rhs, &discriminant, &l)
    }

    /// [`Form::multiply`] with the discriminant and NUCOMP bound precomputed by the caller — the
    /// hot-loop variant (exponentiation, proof verification) that skips recomputing them per op.
    pub fn multiply_with(&self, rhs: &Self, discriminant: &BigInt, l: &BigInt) -> Result<Self> {
        let mut result = Self::nucomp(self, rhs, discriminant, l)?;
        result.reduce();
        Ok(result)
    }

    fn nucomp(f: &Self, g: &Self, discriminant: &BigInt, l: &BigInt) -> Result<Self> {
        if f.a > g.a {
            return Self::nucomp(g, f, discriminant, l);
        }
        let two = BigInt::from(2u8);
        let mut a1 = f.a.clone();
        let mut a2 = g.a.clone();
        let mut c2 = g.c.clone();
        let ss = (&f.b + &g.b).div_floor(&two);
        let m = (&f.b - &g.b).div_floor(&two);

        let t = a2.mod_floor(&a1);
        let (sp, v1) = if t.is_zero() {
            (a1.clone(), BigInt::zero())
        } else {
            // fmpz_gcdinv: sp = gcd(t, a1), v1·t ≡ sp (mod a1) with 0 ≤ v1 < a1.
            lehmer_gcdinv(&t, &a1)
        };
        let mut k = (&m * &v1).mod_floor(&a1);
        if !sp.is_one() {
            // s = gcd(ss, sp) = v2·ss + u2·sp.
            let e = ss.extended_gcd(&sp);
            let (s, v2, u2) = (e.gcd, e.x, e.y);
            k = &k * &u2 - &v2 * &c2;
            if !s.is_one() {
                a1 /= &s;
                a2 /= &s;
                c2 *= &s;
            }
            k = k.mod_floor(&a1);
        }

        if &a1 < l {
            // Small a1: direct composition, no partial reduction needed.
            let t = &a2 * &k;
            let ca = &a2 * &a1;
            let cb = (&t << 1usize) + &g.b;
            let cc = ((&g.b + &t) * &k + &c2) / &a1;
            return Ok(Self {
                a: ca,
                b: cb,
                c: cc,
            });
        }
        // Partial reduction: shrink (a1, k) to ~|D|^(1/4) with the Lehmer partial GCD, then assemble.
        let mut r2 = a1.clone();
        let mut r1 = k.clone();
        let (co2, co1) = xgcd_partial(&mut r2, &mut r1, l);
        let t = &a2 * &r1;
        let m1 = (&m * &co1 + &t) / &a1;
        let m2 = (&ss * &r1 - &c2 * &co1) / &a1;
        let r1m1 = &r1 * &m1;
        let co1m2 = &co1 * &m2;
        let mut ca = if co1.is_negative() {
            r1m1 - co1m2
        } else {
            co1m2 - r1m1
        };
        let mut b = (&t - &ca * &co2) << 1usize;
        b = &b / &co1;
        b -= &g.b;
        let cb = b.mod_floor(&(&ca << 1usize));
        let mut cc = ((&cb * &cb - discriminant) / &ca).div_floor(&BigInt::from(4u8));
        if ca.is_negative() {
            ca = -ca;
            cc = -cc;
        }
        Ok(Self {
            a: ca,
            b: cb,
            c: cc,
        })
    }

    #[cfg(test)]
    fn compose_reference(&self, rhs: &Self) -> Result<Self> {
        let two = BigInt::from(2u8);
        let g = (&rhs.b + &self.b) / &two;
        let h = (&rhs.b - &self.b) / &two;
        let w = self.a.gcd(&rhs.a).gcd(&g);

        let j = w.clone();
        let s = &self.a / &w;
        let t = &rhs.a / &w;
        let u = &g / &w;

        let (k_temp, constant_factor) =
            solve_linear_congruence(&(&t * &u), &(&h * &u + &s * &self.c), &(&s * &t))?;
        let (n, _) = solve_linear_congruence(&(&t * &constant_factor), &(&h - &t * &k_temp), &s)?;

        let k = &k_temp + &constant_factor * &n;
        let l = (&t * &k - &h) / &s;
        let m = (&t * &u * &k - &h * &u - &s * &self.c) / (&s * &t);

        let mut result = Self {
            a: &s * &t,
            b: &j * &u - (&k * &t + &l * &s),
            c: &k * &l - &j * &m,
        };
        result.reduce();
        Ok(result)
    }

    pub fn serialize(&self, discriminant_bits: usize) -> Result<[u8; FORM_SIZE]> {
        let mut form = self.clone();
        form.reduce();
        serialize_form(&form.a, &form.b, discriminant_bits)
    }

    pub fn deserialize(discriminant: &BigInt, bytes: &[u8]) -> Result<Self> {
        if bytes.len() != FORM_SIZE {
            return Err(Error::InvalidFormSize);
        }

        let (a, b) = deserialize_ab(discriminant, bytes)?;
        let form = Self::from_abd(a, b, discriminant)?;
        if !form.is_reduced() {
            return Err(Error::FormNotReduced);
        }
        Ok(form)
    }
}

/// A form on wide stack limbs.
#[derive(Clone, Copy)]
struct WForm {
    a: crate::limbs::SwWide,
    b: crate::limbs::SwWide,
    c: crate::limbs::SwWide,
}

impl WForm {
    fn from_form(f: &Form) -> Self {
        use crate::limbs::SwWide as W;
        Self {
            a: W::from_bigint(&f.a),
            b: W::from_bigint(&f.b),
            c: W::from_bigint(&f.c),
        }
    }

    fn to_form(self) -> Form {
        Form {
            a: self.a.to_bigint(),
            b: self.b.to_bigint(),
            c: self.c.to_bigint(),
        }
    }
}

fn wnormalize(
    a: &mut crate::limbs::SwWide,
    b: &mut crate::limbs::SwWide,
    c: &mut crate::limbs::SwWide,
) {
    use core::cmp::Ordering;
    let ord = b.cmp_mag(a);
    if ord != Ordering::Greater && (!b.is_negative() || ord != Ordering::Equal) {
        return;
    }
    let (r, _) = a.sub(b).div_mod_floor(&a.shl1());
    let ar = a.mul(&r);
    *c = c.add(&ar.add(b).mul(&r));
    *b = b.add(&ar.shl1());
}

fn wreduce_step(
    a: &mut crate::limbs::SwWide,
    b: &mut crate::limbs::SwWide,
    c: &mut crate::limbs::SwWide,
) {
    use core::cmp::Ordering;
    if b.cmp_mag(c) == Ordering::Less {
        core::mem::swap(a, c);
        b.negate();
        return;
    }
    let c2 = c.shl1();
    let c3 = c2.add(c);
    if !b.is_negative() {
        if b.cmp_mag(&c3) == Ordering::Less {
            // s = 1
            let new_b = c2.sub(b);
            let new_c = c.sub(b).add(a);
            *a = *c;
            *b = new_b;
            *c = new_c;
            return;
        }
    } else if b.cmp_mag(c) == Ordering::Greater && b.cmp_mag(&c3) == Ordering::Less {
        // s = −1
        let mut new_b = c2.add(b);
        new_b.negate();
        let new_c = c.add(b).add(a);
        *a = *c;
        *b = new_b;
        *c = new_c;
        return;
    }
    let (s, _) = c.add(b).div_mod_floor(&c2);
    let cs = c.mul(&s);
    let new_b = cs.shl1().sub(b);
    let new_c = cs.sub(b).mul(&s).add(a);
    *a = *c;
    *b = new_b;
    *c = new_c;
}

/// Limb-native `reduce` (identical semantics to [`Form::reduce`]).
fn wreduce(f: &mut WForm) {
    use core::cmp::Ordering;
    wnormalize(&mut f.a, &mut f.b, &mut f.c);
    loop {
        let ord = f.a.cmp_mag(&f.c);
        if ord == Ordering::Greater || (ord == Ordering::Equal && f.b.is_negative()) {
            wreduce_step(&mut f.a, &mut f.b, &mut f.c);
        } else {
            break;
        }
    }
    wnormalize(&mut f.a, &mut f.b, &mut f.c);
}

/// Limb-domain NUDUPL squaring: [`Form::square_with`]'s body with zero `BigInt` involvement.
fn wsquare(f: &WForm, wd: &crate::limbs::SwWide, gl: &crate::limbs::SwGcd) -> WForm {
    use crate::limbs::SwGcd as G;
    use crate::limbs::SwWide as W;
    let ga = f.a.resize::<20>();
    let mut gb = f.b.resize::<20>();
    let (s, v2) = if gb.cmp_mag(&ga) == core::cmp::Ordering::Equal {
        (ga, G::zero())
    } else if gb.is_negative() {
        gb.negate();
        let (g, mut v) = lehmer_gcdinv_sw(&gb, &ga);
        gb.negate();
        v.negate();
        (g, v)
    } else {
        lehmer_gcdinv_sw(&gb, &ga)
    };

    let wb = f.b;
    let mut wa1 = f.a;
    let mut wc1 = f.c;
    let mut wk = v2.resize::<34>().mul(&wc1);
    wk.negate();
    if !s.is_one() {
        let ws = s.resize::<34>();
        wa1 = wa1.div_exact(&ws);
        wc1 = wc1.mul(&ws);
    }
    let (_, wk) = wk.div_mod_floor(&wa1);

    let wl = gl.resize::<34>();
    let (mut ca, cb, mut cc);
    if wa1.cmp_mag(&wl) == core::cmp::Ordering::Less {
        let t = wa1.mul(&wk);
        ca = wa1.mul(&wa1);
        cb = t.shl1().add(&wb);
        cc = wb.add(&t).mul(&wk).add(&wc1).div_exact(&wa1);
    } else {
        let mut gr2 = wa1.resize::<20>();
        let mut gr1 = wk.resize::<20>();
        let (gco2, gco1) = xgcd_partial_sw(&mut gr2, &mut gr1, gl);
        let wr1 = gr1.resize::<34>();
        let wco1 = gco1.resize::<34>();
        let wco2 = gco2.resize::<34>();
        let t = wa1.mul(&wr1);
        let m2 = wb.mul(&wr1).sub(&wc1.mul(&wco1)).div_exact(&wa1);
        let r1r1 = wr1.mul(&wr1);
        let co1m2 = wco1.mul(&m2);
        ca = if wco1.is_negative() {
            r1r1.sub(&co1m2)
        } else {
            co1m2.sub(&r1r1)
        };
        let b = t.sub(&ca.mul(&wco2)).shl1().div_exact(&wco1).sub(&wb);
        let (_, cbr) = b.div_mod_floor(&ca.shl1());
        cb = cbr;
        let four = W::one().shl1().shl1();
        let (q4, _) = cb.mul(&cb).sub(wd).div_exact(&ca).div_mod_floor(&four);
        cc = q4;
        if ca.is_negative() {
            ca.negate();
            cc.negate();
        }
    }
    let mut out = WForm {
        a: ca,
        b: cb,
        c: cc,
    };
    wreduce(&mut out);
    out
}

/// Limb-domain NUCOMP composition: [`Form::nucomp`]'s body with `BigInt` only in the rare
/// `sp != 1` double-cofactor branch.
fn wmultiply(f: &WForm, g: &WForm, wd: &crate::limbs::SwWide, gl: &crate::limbs::SwGcd) -> WForm {
    use crate::limbs::SwGcd as G;
    use crate::limbs::SwWide as W;
    if f.a.cmp_mag(&g.a) == core::cmp::Ordering::Greater {
        return wmultiply(g, f, wd, gl);
    }
    let mut a1 = f.a;
    let mut a2 = g.a;
    let mut c2 = g.c;
    let ss = f.b.add(&g.b).shr1_exact();
    let m = f.b.sub(&g.b).shr1_exact();

    let (_, t) = a2.div_mod_floor(&a1);
    let (sp, v1) = if t.is_zero() {
        (a1.resize::<20>(), G::zero())
    } else {
        lehmer_gcdinv_sw(&t.resize::<20>(), &a1.resize::<20>())
    };
    let (_, mut k) = m.mul(&v1.resize::<34>()).div_mod_floor(&a1);
    if !sp.is_one() {
        // Rare double-cofactor branch: s = gcd(ss, sp) = v2·ss + u2·sp (BigInt fallback).
        let e = ss.to_bigint().extended_gcd(&sp.to_bigint());
        let (s, v2, u2) = (
            W::from_bigint(&e.gcd),
            W::from_bigint(&e.x),
            W::from_bigint(&e.y),
        );
        k = k.mul(&u2).sub(&v2.mul(&c2));
        if !s.is_one() {
            a1 = a1.div_exact(&s);
            a2 = a2.div_exact(&s);
            c2 = c2.mul(&s);
        }
        let (_, km) = k.div_mod_floor(&a1);
        k = km;
    }

    let wl = gl.resize::<34>();
    let (mut ca, cb, mut cc);
    if a1.cmp_mag(&wl) == core::cmp::Ordering::Less {
        // Small a1: direct composition.
        let t = a2.mul(&k);
        ca = a2.mul(&a1);
        cb = t.shl1().add(&g.b);
        cc = g.b.add(&t).mul(&k).add(&c2).div_exact(&a1);
    } else {
        // Partial reduction, then assemble.
        let mut gr2 = a1.resize::<20>();
        let mut gr1 = k.resize::<20>();
        let (gco2, gco1) = xgcd_partial_sw(&mut gr2, &mut gr1, gl);
        let wr1 = gr1.resize::<34>();
        let wco1 = gco1.resize::<34>();
        let wco2 = gco2.resize::<34>();
        let t = a2.mul(&wr1);
        let m1 = m.mul(&wco1).add(&t).div_exact(&a1);
        let m2 = ss.mul(&wr1).sub(&c2.mul(&wco1)).div_exact(&a1);
        let r1m1 = wr1.mul(&m1);
        let co1m2 = wco1.mul(&m2);
        ca = if wco1.is_negative() {
            r1m1.sub(&co1m2)
        } else {
            co1m2.sub(&r1m1)
        };
        let b = t.sub(&ca.mul(&wco2)).shl1().div_exact(&wco1).sub(&g.b);
        let (_, cbr) = b.div_mod_floor(&ca.shl1());
        cb = cbr;
        let four = W::one().shl1().shl1();
        let (q4, _) = cb.mul(&cb).sub(wd).div_exact(&ca).div_mod_floor(&four);
        cc = q4;
        if ca.is_negative() {
            ca.negate();
            cc.negate();
        }
    }
    let mut out = WForm {
        a: ca,
        b: cb,
        c: cc,
    };
    wreduce(&mut out);
    out
}

/// NUCOMP's partial-reduction bound `L = |D|^(1/4)` — compute once per discriminant and pass to
/// [`Form::multiply_with`] in hot loops.
pub fn nucomp_bound(discriminant: &BigInt) -> BigInt {
    sqrt_bigint(&sqrt_bigint(&(-discriminant)))
}

pub fn fast_pow_form(base: &Form, discriminant: &BigInt, exponent: &BigInt) -> Result<Form> {
    fast_pow_form_with(base, discriminant, &nucomp_bound(discriminant), exponent)
}

pub fn fast_pow_form_with(
    base: &Form,
    discriminant: &BigInt,
    l: &BigInt,
    exponent: &BigInt,
) -> Result<Form> {
    if exponent.is_zero() {
        return Form::identity(discriminant);
    }

    let nbits = bit_len(exponent);
    // Small exponents: plain square-and-multiply — the 14-multiply window table only pays for
    // itself above ~64 bits (VDF exponents are ~264-bit).
    if nbits <= 64 {
        let mut result = base.clone();
        for bit in (0..nbits.saturating_sub(1)).rev() {
            result = result.square_with(discriminant, l)?;
            if exponent.bit(bit as u64) {
                result = result.multiply_with(base, discriminant, l)?;
            }
        }
        result.reduce();
        return Ok(result);
    }

    // 4-bit fixed-window exponentiation: precompute base^2..base^15, then per window do 4 squarings
    // and at most one table multiply — ~40% fewer multiplies than bit-at-a-time for the ~264-bit
    // exponents VDF verification uses. Group-identical result; the final reduce yields the same
    // unique reduced representative. The ENTIRE loop (table build, squarings, multiplies) runs in
    // the limb domain — Form conversion happens exactly twice: base in, result out.
    let gl = crate::limbs::SwGcd::from_bigint(l);
    let wd = crate::limbs::SwWide::from_bigint(discriminant);
    let table = pow_window_table(base, &wd, &gl);
    let nwin = nbits.div_ceil(POW_WINDOW);
    let mut result: Option<WForm> = None;
    for w in (0..nwin).rev() {
        if let Some(r) = result.as_mut() {
            for _ in 0..POW_WINDOW {
                *r = wsquare(r, &wd, &gl);
            }
        }
        let v = pow_window_digit(exponent, nbits, w);
        if v != 0 {
            result = Some(match result.take() {
                Some(r) => wmultiply(&r, &table[v - 1], &wd, &gl),
                None => table[v - 1],
            });
        }
    }
    // exponent != 0 guarantees at least one nonzero window.
    let mut result = result.ok_or(Error::InvalidForm)?.to_form();
    result.reduce();
    Ok(result)
}

const POW_WINDOW: usize = 4;

/// The 4-bit-window multiples table: `table[i] = base^(i+1)`, i in 0..15.
fn pow_window_table(
    base: &Form,
    wd: &crate::limbs::SwWide,
    gl: &crate::limbs::SwGcd,
) -> Vec<WForm> {
    let wbase = WForm::from_form(base);
    let mut table: Vec<WForm> = Vec::with_capacity(1 << POW_WINDOW);
    table.push(wbase);
    for i in 1..(1 << POW_WINDOW) - 1 {
        let next = wmultiply(&table[i - 1], &wbase, wd, gl);
        table.push(next);
    }
    table
}

/// Window `w`'s 4-bit digit of `exponent` (bits above `nbits` read as zero).
fn pow_window_digit(exponent: &BigInt, nbits: usize, w: usize) -> usize {
    let mut v = 0usize;
    for j in (0..POW_WINDOW).rev() {
        let bit_index = w * POW_WINDOW + j;
        if bit_index < nbits && exponent.bit(bit_index as u64) {
            v |= 1 << j;
        }
    }
    v
}

/// Simultaneous double exponentiation (Straus/Shamir interleaving; Knuth Vol. 2 §4.6.3): computes
/// the product `x^xe · y^ye` in ONE 4-bit-window chain with the squaring run SHARED between the
/// two exponents. The Wesolowski check needs exactly this product (`witness^b · x^r`) and never
/// the individual powers; evaluated separately the two ~264-bit exponentiations cost two full
/// squaring chains (~673 group ops serially), interleaved they cost one (~411 group ops: 2×14
/// table multiplies + ~260 shared squarings + ≤2 table multiplies per window). Group-identical result:
/// squarings/compositions land on the same group element, and the final reduce yields the unique
/// reduced representative, exactly the argument the single-exponent windowed loop already relies
/// on. Used by the SERIAL verify path (saturated window drains, where throughput ≡ work); the
/// latency-bound parallel path keeps the two-thread split whose critical path (~336 ops) is
/// shorter than the fused chain.
pub fn fast_pow_form_pair_with(
    x: &Form,
    xe: &BigInt,
    y: &Form,
    ye: &BigInt,
    discriminant: &BigInt,
    l: &BigInt,
) -> Result<Form> {
    // Degenerate exponents: a zero exponent contributes the identity; a ≤64-bit exponent (tiny
    // segment iteration counts make r = 2^iters small) belongs on the plain square-and-multiply
    // route where a 14-multiply window table would not pay for itself. Both fall back to the
    // single-exponent paths and an explicit composition.
    if xe.is_zero() {
        return fast_pow_form_with(y, discriminant, l, ye);
    }
    if ye.is_zero() {
        return fast_pow_form_with(x, discriminant, l, xe);
    }
    let xbits = bit_len(xe);
    let ybits = bit_len(ye);
    if xbits <= 64 || ybits <= 64 {
        let fx = fast_pow_form_with(x, discriminant, l, xe)?;
        let fy = fast_pow_form_with(y, discriminant, l, ye)?;
        return fx.multiply_with(&fy, discriminant, l);
    }

    let gl = crate::limbs::SwGcd::from_bigint(l);
    let wd = crate::limbs::SwWide::from_bigint(discriminant);
    let table_x = pow_window_table(x, &wd, &gl);
    let table_y = pow_window_table(y, &wd, &gl);
    let nwin = xbits.max(ybits).div_ceil(POW_WINDOW);
    let mut result: Option<WForm> = None;
    for w in (0..nwin).rev() {
        if let Some(r) = result.as_mut() {
            for _ in 0..POW_WINDOW {
                *r = wsquare(r, &wd, &gl);
            }
        }
        for (table, exponent, ebits) in [(&table_x, xe, xbits), (&table_y, ye, ybits)] {
            let v = pow_window_digit(exponent, ebits, w);
            if v != 0 {
                result = Some(match result.take() {
                    Some(r) => wmultiply(&r, &table[v - 1], &wd, &gl),
                    None => table[v - 1],
                });
            }
        }
    }
    // Both exponents are nonzero, so at least one window multiplied in.
    let mut result = result.ok_or(Error::InvalidForm)?.to_form();
    result.reduce();
    Ok(result)
}

pub fn get_b(discriminant: &BigInt, x: &Form, y: &Form) -> Result<BigInt> {
    let d_bits = bit_len(discriminant);
    let mut bytes = x.serialize(d_bits)?.to_vec();
    bytes.extend_from_slice(&y.serialize(d_bits)?);
    Ok(hash_prime(&bytes, B_BITS, &[B_BITS - 1]))
}

fn normalize(a: &mut BigInt, b: &mut BigInt, c: &mut BigInt) {
    // Already normalized (`-a < b <= a`) — the common case after a reduce step; skip the division.
    if b.magnitude() <= a.magnitude() && (!b.is_negative() || b.magnitude() != a.magnitude()) {
        return;
    }
    // r = floor((a - b) / 2a); then b += 2·a·r and c += r·(a·r + b_old) — `a` is unchanged.
    let r = (&*a - &*b).div_floor(&(&*a << 1usize));
    let ar = &*a * &r;
    *c += (&ar + &*b) * &r;
    *b += ar << 1usize;
}

fn reduce_impl(a: &mut BigInt, b: &mut BigInt, c: &mut BigInt) {
    if b.magnitude() < c.magnitude() {
        // −c < b < c  ⟹  s = 0:  (a, b, c) ← (c, −b, a) — a swap and a negation, no arithmetic.
        std::mem::swap(a, c);
        *b = -std::mem::take(b);
        return;
    }
    let c2 = &*c << 1usize;
    let c3 = &c2 + &*c;
    if !b.is_negative() {
        if *b < c3 {
            // c ≤ b < 3c  ⟹  s = 1.
            let new_b = &c2 - &*b;
            let new_c = (&*c - &*b) + &*a;
            *a = std::mem::replace(c, new_c);
            *b = new_b;
            return;
        }
    } else if b.magnitude() > c.magnitude() && b.magnitude() < c3.magnitude() {
        // −3c < b < −c  ⟹  s = −1.
        let new_b = -(&c2 + &*b);
        let new_c = (&*c + &*b) + &*a;
        *a = std::mem::replace(c, new_c);
        *b = new_b;
        return;
    }
    let s = (&*c + &*b).div_floor(&c2);
    let cs = &*c * &s;
    let new_b = (&cs << 1usize) - &*b;
    let new_c = (cs - &*b) * &s + &*a;
    *a = std::mem::replace(c, new_c);
    *b = new_b;
}

#[cfg(test)]
fn solve_linear_congruence(a: &BigInt, b: &BigInt, m: &BigInt) -> Result<(BigInt, BigInt)> {
    let egcd = a.extended_gcd(m);
    if !b.is_multiple_of(&egcd.gcd) {
        return Err(Error::InvalidForm);
    }
    let q = b / &egcd.gcd;
    Ok((positive_mod(&(q * egcd.x), m), m / egcd.gcd))
}

fn positive_mod(a: &BigInt, modulus: &BigInt) -> BigInt {
    a.mod_floor(&modulus.abs())
}

fn deserialize_ab(discriminant: &BigInt, bytes: &[u8]) -> Result<(BigInt, BigInt)> {
    if bytes[0] & (BQFC_IS_1 | BQFC_IS_GEN) != 0 {
        let a = if bytes[0] & BQFC_IS_GEN != 0 {
            BigInt::from(2u8)
        } else {
            BigInt::one()
        };
        return Ok((a, BigInt::one()));
    }

    let d_bits = rounded_discriminant_bits(discriminant)?;
    let g_size = usize::from(bytes[1]);
    if g_size >= d_bits / 32 {
        return Err(Error::InvalidCompressedForm);
    }

    let mut offset = 2usize;
    let a_size = d_bits / 16 - g_size;
    let mut a = bigint_from_le(&bytes[offset..offset + a_size]);
    offset += a_size;

    let t_size = d_bits / 32 - g_size;
    let mut t = bigint_from_le(&bytes[offset..offset + t_size]);
    offset += t_size;

    let g_bytes = g_size + 1;
    let g = bigint_from_le(&bytes[offset..offset + g_bytes]);
    offset += g_bytes;
    let b0 = bigint_from_le(&bytes[offset..offset + g_bytes]);

    if bytes[0] & BQFC_T_SIGN != 0 {
        t = -t;
    }

    let b_sign = bytes[0] & BQFC_B_SIGN != 0;
    let (out_a, out_b) = decompress_ab(discriminant, &mut a, &t, &g, &b0, b_sign)?;
    if serialize_form(&out_a, &out_b, bit_len(discriminant))? != bytes {
        return Err(Error::InvalidCompressedForm);
    }
    Ok((out_a, out_b))
}

fn decompress_ab(
    discriminant: &BigInt,
    a: &mut BigInt,
    c_t: &BigInt,
    g: &BigInt,
    b0: &BigInt,
    b_sign: bool,
) -> Result<(BigInt, BigInt)> {
    if c_t.is_zero() {
        return Ok((a.clone(), a.clone()));
    }

    let t = if c_t.is_negative() {
        c_t + a.clone()
    } else {
        c_t.clone()
    };
    if a.is_zero() {
        return Err(Error::InvalidCompressedForm);
    }

    let egcd = t.extended_gcd(a);
    if egcd.gcd != BigInt::one() {
        return Err(Error::InvalidCompressedForm);
    }
    let t_inv = positive_mod(&egcd.x, a);

    let d = discriminant.mod_floor(a);
    let tmp = positive_mod(&(c_t * c_t * d), a);
    let sqrt = sqrt_bigint(&tmp);
    if &sqrt * &sqrt != tmp {
        return Err(Error::InvalidCompressedForm);
    }

    let mut out_b = (&sqrt * t_inv).mod_floor(a);
    let out_a = if *g > BigInt::one() {
        a.clone() * g
    } else {
        a.clone()
    };
    if b0 > &BigInt::zero() {
        out_b += a.clone() * b0;
    }
    if b_sign {
        out_b = -out_b;
    }
    if out_b.abs() > out_a {
        return Err(Error::InvalidCompressedForm);
    }

    Ok((out_a, out_b))
}

fn serialize_form(a: &BigInt, b: &BigInt, d_bits: usize) -> Result<[u8; FORM_SIZE]> {
    let mut out = [0u8; FORM_SIZE];

    if *b == BigInt::one() && *a <= BigInt::from(2u8) {
        out[0] = if *a == BigInt::from(2u8) {
            BQFC_IS_GEN
        } else {
            BQFC_IS_1
        };
        return Ok(out);
    }

    let compressed = compress_ab(a, b);
    serialize_compressed(&mut out, &compressed, d_bits)?;
    Ok(out)
}

struct CompressedForm {
    a: BigInt,
    t: BigInt,
    g: BigInt,
    b0: BigInt,
    b_sign: bool,
}

fn compress_ab(a: &BigInt, b: &BigInt) -> CompressedForm {
    if a == b {
        return CompressedForm {
            a: a.clone(),
            t: BigInt::zero(),
            g: BigInt::zero(),
            b0: BigInt::zero(),
            b_sign: false,
        };
    }

    let b_sign = b.is_negative();
    let a_sqrt = sqrt_bigint(a);
    let mut a_copy = a.clone();
    let mut b_copy = b.abs();
    let mut t = xgcd_partial_co1(&mut a_copy, &mut b_copy, &a_sqrt);
    t = -t;

    let g = a.gcd(&t);
    if g == BigInt::one() {
        CompressedForm {
            a: a.clone(),
            t,
            g,
            b0: BigInt::zero(),
            b_sign,
        }
    } else {
        let reduced_a = a / &g;
        let reduced_t = &t / &g;
        let mut b0 = b / &reduced_a;
        if b_sign {
            b0 = -b0;
        }
        CompressedForm {
            a: reduced_a,
            t: reduced_t,
            g,
            b0,
            b_sign,
        }
    }
}

fn serialize_compressed(
    out: &mut [u8; FORM_SIZE],
    form: &CompressedForm,
    d_bits: usize,
) -> Result<()> {
    if d_bits == 0 || d_bits > MAX_D_BITS {
        return Err(Error::InvalidDiscriminant);
    }

    let d_bits = (d_bits + 31) & !31usize;
    if d_bits > MAX_D_BITS {
        return Err(Error::InvalidDiscriminant);
    }

    out[0] = u8::from(form.b_sign);
    if form.t.is_negative() {
        out[0] |= BQFC_T_SIGN;
    }

    let g_size = bit_len(&form.g).div_ceil(8).saturating_sub(1);
    if g_size > usize::from(u8::MAX) {
        return Err(Error::InvalidCompressedForm);
    }
    out[1] = g_size as u8;
    let mut offset = 2usize;

    export_field(out, &mut offset, d_bits / 16 - g_size, &form.a)?;
    export_field(out, &mut offset, d_bits / 32 - g_size, &form.t.abs())?;
    export_field(out, &mut offset, g_size + 1, &form.g)?;
    export_field(out, &mut offset, g_size + 1, &form.b0)?;
    Ok(())
}

fn export_field(
    out: &mut [u8; FORM_SIZE],
    offset: &mut usize,
    size: usize,
    value: &BigInt,
) -> Result<()> {
    let bytes = bigint_to_fixed_le(value, size).ok_or(Error::InvalidCompressedForm)?;
    out[*offset..*offset + size].copy_from_slice(&bytes);
    *offset += size;
    Ok(())
}

fn rounded_discriminant_bits(discriminant: &BigInt) -> Result<usize> {
    let d_bits = bit_len(discriminant);
    if d_bits == 0 || d_bits > MAX_D_BITS {
        return Err(Error::InvalidDiscriminant);
    }
    Ok((d_bits + 31) & !31usize)
}

fn xgcd_bitlen(x: &BigInt) -> i64 {
    let b = bit_len(x);
    if b == 0 { 1 } else { b as i64 }
}

fn xgcd_extract_word(x: &BigInt, shift: i64) -> i128 {
    let w = if shift > 0 {
        u64_low_word(&(x >> (shift as usize)))
    } else {
        u64_low_word(x)
    };
    i64::from_ne_bytes(w.to_ne_bytes()) as i128
}

fn xgcd_partial_co1(r2: &mut BigInt, r1: &mut BigInt, limit: &BigInt) -> BigInt {
    let mut co2 = BigInt::zero();
    let mut co1 = -BigInt::one();

    while !r1.is_zero() && &*r1 > limit {
        let bits = (xgcd_bitlen(r2).max(xgcd_bitlen(r1)) - 64 + 1).max(0);
        let mut rr2 = xgcd_extract_word(r2, bits);
        let mut rr1 = xgcd_extract_word(r1, bits);
        let bb = xgcd_extract_word(limit, bits);

        let (mut aa2, mut aa1, mut bb2, mut bb1): (i128, i128, i128, i128) = (0, 1, 1, 0);
        let mut i: i64 = 0;
        while rr1 != 0 && rr1 > bb {
            let qq = rr2 / rr1; // trunc toward zero, matching C integer division
            let t1 = rr2 - qq * rr1;
            let t2 = aa2 - qq * aa1;
            let t3 = bb2 - qq * bb1;
            if i & 1 != 0 {
                if t1 < -t3 || rr1 - t1 < t2 - aa1 {
                    break;
                }
            } else if t1 < -t2 || rr1 - t1 < t3 - bb1 {
                break;
            }
            rr2 = rr1;
            rr1 = t1;
            aa2 = aa1;
            aa1 = t2;
            bb2 = bb1;
            bb1 = t3;
            i += 1;
        }

        if i == 0 {
            let q = &*r2 / &*r1;
            let rem = &*r2 - &q * &*r1;
            *r2 = r1.clone();
            *r1 = rem;
            let next = &co2 - &q * &co1;
            co2 = co1;
            co1 = next;
        } else {
            // Apply the accumulated word-level transform to the big integers, from OLD (r2, r1)/(co2, co1).
            let new_r2 = &*r2 * bb2 + &*r1 * aa2;
            let new_r1 = &*r1 * aa1 + &*r2 * bb1;
            *r2 = new_r2;
            *r1 = new_r1;
            let new_co2 = &co2 * bb2 + &co1 * aa2;
            let new_co1 = &co1 * aa1 + &co2 * bb1;
            co2 = new_co2;
            co1 = new_co1;
            if r1.is_negative() {
                co1 = -co1;
                *r1 = -&*r1;
            }
            if r2.is_negative() {
                co2 = -co2;
                *r2 = -&*r2;
            }
        }
    }

    if r2.is_negative() {
        co1 = -co1;
    }
    co1
}

fn xgcd_partial(r2: &mut BigInt, r1: &mut BigInt, limit: &BigInt) -> (BigInt, BigInt) {
    use crate::limbs::SwGcd;
    let mut sr2 = SwGcd::from_bigint(r2);
    let mut sr1 = SwGcd::from_bigint(r1);
    let (c2, c1) = xgcd_partial_sw(&mut sr2, &mut sr1, &SwGcd::from_bigint(limit));
    *r2 = sr2.to_bigint();
    *r1 = sr1.to_bigint();
    (c2.to_bigint(), c1.to_bigint())
}

/// Limb-native FLINT `fmpz_xgcd_partial` core (both cofactors), reducing `(r2, r1)` until
/// `r1 <= limit`.
fn xgcd_partial_sw(
    r2io: &mut crate::limbs::SwGcd,
    r1io: &mut crate::limbs::SwGcd,
    slimit: &crate::limbs::SwGcd,
) -> (crate::limbs::SwGcd, crate::limbs::SwGcd) {
    use crate::limbs::SwGcd as Sw;
    let mut sr2 = *r2io;
    let mut sr1 = *r1io;
    let mut co2 = Sw::zero();
    let mut co1 = Sw::one();
    co1.negate();

    while !sr1.is_zero() && sr1.cmp_mag(slimit) == core::cmp::Ordering::Greater {
        // Extract one bit narrower than a word so the values fit i64: the inner Euclidean
        // division then runs on the 64-bit divide unit instead of the __divti3 i128 libcall
        // (~30 cycles each, ~35 divides per outer iteration — the loop's former hot half).
        let bits = (sr2.bit_len().max(1).max(sr1.bit_len().max(1)) - 63 + 1).max(0);
        let mut rr2 = sr2.extract_word(bits);
        let mut rr1 = sr1.extract_word(bits);
        let bb = slimit.extract_word(bits);

        let (mut aa2, mut aa1, mut bb2, mut bb1): (i128, i128, i128, i128) = (0, 1, 1, 0);
        let mut i: i64 = 0;
        while rr1 != 0 && rr1 > bb {
            // Gauss–Kuzmin: ~41.5% of continued-fraction quotients are 1 and ~17% are 2, so
            // compare-and-subtract covers most iterations without the divide. Measured both
            // ways per target: x86-64's uniformly slow IDIV makes this worth −6% t_op
            // (26.1 → 24.5 µs), while the A72's early-terminating divider already handles
            // small quotients cheaply and the added data-dependent branches mispredict —
            // +5% t_op there (40.0 → 42.0 µs). Arch-gated accordingly; identical math on
            // both sides, pinned by the gcd differentials.
            #[cfg(target_arch = "x86_64")]
            let (qq, t1) = if rr2 - rr1 < rr1 {
                (1i128, rr2 - rr1)
            } else if rr2 - 2 * rr1 < rr1 {
                (2i128, rr2 - 2 * rr1)
            } else {
                let qq = i128::from((rr2 as i64) / (rr1 as i64));
                (qq, rr2 - qq * rr1)
            };
            #[cfg(not(target_arch = "x86_64"))]
            let (qq, t1) = {
                let qq = i128::from((rr2 as i64) / (rr1 as i64));
                (qq, rr2 - qq * rr1)
            };
            let t2 = aa2 - qq * aa1;
            let t3 = bb2 - qq * bb1;
            if i & 1 != 0 {
                if t1 < -t3 || rr1 - t1 < t2 - aa1 {
                    break;
                }
            } else if t1 < -t2 || rr1 - t1 < t3 - bb1 {
                break;
            }
            rr2 = rr1;
            rr1 = t1;
            aa2 = aa1;
            aa1 = t2;
            bb2 = bb1;
            bb1 = t3;
            i += 1;
        }

        if i == 0 {
            // Single exact floor-division Euclidean step (rare); done in BigInt.
            let (mut br2, mut br1) = (sr2.to_bigint(), sr1.to_bigint());
            let (mut bc2, mut bc1) = (co2.to_bigint(), co1.to_bigint());
            let (q, rem) = br2.div_mod_floor(&br1);
            br2 = std::mem::replace(&mut br1, rem);
            let next = &bc2 - &q * &bc1;
            bc2 = std::mem::replace(&mut bc1, next);
            sr2 = Sw::from_bigint(&br2);
            sr1 = Sw::from_bigint(&br1);
            co2 = Sw::from_bigint(&bc2);
            co1 = Sw::from_bigint(&bc1);
        } else {
            let new_r2 = sr2.linear2(bb2, &sr1, aa2);
            let new_r1 = sr1.linear2(aa1, &sr2, bb1);
            sr2 = new_r2;
            sr1 = new_r1;
            let new_co2 = co2.linear2(bb2, &co1, aa2);
            let new_co1 = co1.linear2(aa1, &co2, bb1);
            co2 = new_co2;
            co1 = new_co1;
            if sr1.is_negative() {
                co1.negate();
                sr1.negate();
            }
            if sr2.is_negative() {
                co2.negate();
                sr2.negate();
            }
        }
    }

    if sr2.is_negative() {
        co2.negate();
        co1.negate();
        sr2.negate();
    }
    *r2io = sr2;
    *r1io = sr1;
    (co2, co1)
}

// Floor square root via `num_integer::Roots` (Newton's method) — the previous bit-by-bit binary
// search cost hundreds of full-width multiplies per call, which dominated NUCOMP's `|D|^(1/4)` bound.
fn sqrt_bigint(value: &BigInt) -> BigInt {
    if value <= &BigInt::zero() {
        return BigInt::zero();
    }
    value.sqrt()
}

pub(crate) fn fast_pow_u64_mod(base: u64, exponent: u64, modulus: &BigInt) -> Result<BigInt> {
    if modulus.is_zero() {
        return Err(Error::InvalidProofParameters);
    }
    let m = modulus
        .abs()
        .to_biguint()
        .ok_or(Error::InvalidProofParameters)?;
    let b = num_bigint::BigUint::from(base);
    let e = num_bigint::BigUint::from(exponent);
    Ok(BigInt::from_biguint(
        num_bigint::Sign::Plus,
        b.modpow(&e, &m),
    ))
}

pub(crate) fn get_block(i: u64, k: u64, t: u64, b: &BigInt) -> Result<u64> {
    let mut res = fast_pow_u64_mod(2, t - k * (i + 1), b)?;
    res <<= k as usize;
    res /= b;
    Ok(u64_low_word(&res))
}
/// Lehmer-accelerated `gcdinv`: returns `(g, v)` with `g = gcd(b, a)` and `v·b ≡ g (mod a)`,
/// `0 ≤ v < a`, for `0 ≤ b < a`. Replaces the schoolbook `extended_gcd` (hundreds of full-width
/// divisions) in the NUDUPL/NUCOMP hot path with the same word-level cofactor-matrix acceleration as
/// `xgcd_partial`. The tracked pair `(u2, u1)` maintains the congruence invariant `u·b ≡ r (mod a)`
/// through every step (the linear transforms preserve it exactly; sign fixups negate `u` with `r`).
fn lehmer_gcdinv(b: &BigInt, a: &BigInt) -> (BigInt, BigInt) {
    use crate::limbs::SwGcd;
    let (g, v) = lehmer_gcdinv_sw(&SwGcd::from_bigint(b), &SwGcd::from_bigint(a));
    (g.to_bigint(), v.to_bigint())
}

/// Limb-native `gcdinv` core: `(g, v)` with `g = gcd(b, a)`, `v·b ≡ g (mod a)`, `0 ≤ v < a`.
fn lehmer_gcdinv_sw(
    b: &crate::limbs::SwGcd,
    a: &crate::limbs::SwGcd,
) -> (crate::limbs::SwGcd, crate::limbs::SwGcd) {
    use crate::limbs::SwGcd as Sw;
    let mut r2 = *a;
    let mut r1 = *b;
    // u2·b ≡ r2 (mod a) with u2 = 0 (r2 = a ≡ 0); u1·b ≡ r1 with u1 = 1.
    let mut u2 = Sw::zero();
    let mut u1 = Sw::one();

    while !r1.is_zero() {
        // i64-fitting extraction — hardware 64-bit division in the inner loop (see
        // xgcd_partial_sw; identical technique).
        let bits = (r2.bit_len().max(1).max(r1.bit_len().max(1)) - 63 + 1).max(0);
        let mut rr2 = r2.extract_word(bits);
        let mut rr1 = r1.extract_word(bits);

        let (mut aa2, mut aa1, mut bb2, mut bb1): (i128, i128, i128, i128) = (0, 1, 1, 0);
        let mut i: i64 = 0;
        while rr1 != 0 {
            // Gauss–Kuzmin: ~41.5% of continued-fraction quotients are 1 and ~17% are 2, so
            // compare-and-subtract covers most iterations without the divide. Measured both
            // ways per target: x86-64's uniformly slow IDIV makes this worth −6% t_op
            // (26.1 → 24.5 µs), while the A72's early-terminating divider already handles
            // small quotients cheaply and the added data-dependent branches mispredict —
            // +5% t_op there (40.0 → 42.0 µs). Arch-gated accordingly; identical math on
            // both sides, pinned by the gcd differentials.
            #[cfg(target_arch = "x86_64")]
            let (qq, t1) = if rr2 - rr1 < rr1 {
                (1i128, rr2 - rr1)
            } else if rr2 - 2 * rr1 < rr1 {
                (2i128, rr2 - 2 * rr1)
            } else {
                let qq = i128::from((rr2 as i64) / (rr1 as i64));
                (qq, rr2 - qq * rr1)
            };
            #[cfg(not(target_arch = "x86_64"))]
            let (qq, t1) = {
                let qq = i128::from((rr2 as i64) / (rr1 as i64));
                (qq, rr2 - qq * rr1)
            };
            let t2 = aa2 - qq * aa1;
            let t3 = bb2 - qq * bb1;
            if i & 1 != 0 {
                if t1 < -t3 || rr1 - t1 < t2 - aa1 {
                    break;
                }
            } else if t1 < -t2 || rr1 - t1 < t3 - bb1 {
                break;
            }
            rr2 = rr1;
            rr1 = t1;
            aa2 = aa1;
            aa1 = t2;
            bb2 = bb1;
            bb1 = t3;
            i += 1;
        }

        if i == 0 {
            // Single exact floor-division Euclidean step (rare); done in BigInt.
            let (mut br2, mut br1) = (r2.to_bigint(), r1.to_bigint());
            let (mut bu2, mut bu1) = (u2.to_bigint(), u1.to_bigint());
            let (q, rem) = br2.div_mod_floor(&br1);
            br2 = std::mem::replace(&mut br1, rem);
            let next = &bu2 - &q * &bu1;
            bu2 = std::mem::replace(&mut bu1, next);
            r2 = Sw::from_bigint(&br2);
            r1 = Sw::from_bigint(&br1);
            u2 = Sw::from_bigint(&bu2);
            u1 = Sw::from_bigint(&bu1);
        } else {
            let new_r2 = r2.linear2(bb2, &r1, aa2);
            let new_r1 = r1.linear2(aa1, &r2, bb1);
            r2 = new_r2;
            r1 = new_r1;
            let new_u2 = u2.linear2(bb2, &u1, aa2);
            let new_u1 = u1.linear2(aa1, &u2, bb1);
            u2 = new_u2;
            u1 = new_u1;
            if r1.is_negative() {
                u1.negate();
                r1.negate();
            }
            if r2.is_negative() {
                u2.negate();
                r2.negate();
            }
        }
    }

    let (_, v) = u2.div_mod_floor(a);
    (r2, v)
}

#[cfg(test)]
#[path = "../tests/unit/form/nucomp_tests.rs"]
mod nucomp_tests;

/// Per-phase wall-time accumulators for `square_with` (feature `phase-profile` only) — the
/// measurement that decides where the fixed-limb assembly effort goes. Thread-local so
/// bench runs need no synchronization; `take()` reads-and-resets.
#[cfg(feature = "phase-profile")]
pub mod phase_profile {
    use std::cell::RefCell;
    use std::time::Instant;

    pub const PHASES: [&str; 5] = [
        "gcdinv",
        "k-prep(mul+div)",
        "xgcd_partial",
        "composition(mul/div)",
        "to_bigint+reduce",
    ];

    thread_local! {
        static ACC: RefCell<[u64; 5]> = const { RefCell::new([0; 5]) };
    }

    pub struct Stamp(Instant);

    impl Stamp {
        #[must_use]
        pub fn new() -> Self {
            Self(Instant::now())
        }
        pub fn lap(&mut self, phase: usize) {
            let now = Instant::now();
            let ns = now.duration_since(self.0).as_nanos() as u64;
            ACC.with(|a| a.borrow_mut()[phase] += ns);
            self.0 = now;
        }
    }

    impl Default for Stamp {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Read and reset this thread's accumulators (nanoseconds per phase).
    pub fn take() -> [u64; 5] {
        ACC.with(|a| std::mem::take(&mut *a.borrow_mut()))
    }
}
