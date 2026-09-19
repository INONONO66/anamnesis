/** Fixed-order binary64 contract for persisted Hit accessibility only.
 * No native exp/pow/sqrt, quantization, fuzzy equality or utility rounding.
 * Domain: stability in [1,3650], elapsed days >= 0, kappa in [0,1].
 * Changing the operation order/iteration counts requires a new version. */
export const ADOPTION_NUMERIC_VERSION = "binary64-adoption-v1" as const;

/** sqrt(x), x >= 1. Power-of-four reduction is exact in binary64. On
 * [1,4), Newton starting at 2 reaches rounding precision in eight steps.
 * Fixed iteration counts avoid platform-specific convergence decisions. */
function squareRoot(x: number): number {
  let scale = 1;
  while (x >= 4) { x /= 4; scale *= 2; }
  let root = 2;
  for (let i = 0; i < 8; i++) root = (root + x / root) / 2;
  return root * scale;
}

/** s^(1/10) without log/exp/pow. On s in [1,3650] the true root is
 * in [1,3). Newton starts above it. For relative error e >= 0:
 * e_next <= .9e, and once e <= .1, e_next <= 4.5e^2.
 * From e <= 2, 29 + 6 exact-arithmetic steps reach < 2e-23;
 * 64 fixed steps leave ample margin for binary64 rounding. */
function tenthRoot(s: number): number {
  let root = 3;
  for (let i = 0; i < 64; i++) {
    const squared = root * root, fourth = squared * squared, eighth = fourth * fourth;
    root = (9 * root + s / (eighth * root)) / 10;
  }
  return root;
}

/** exp(x)-1 on [0,1]. Twenty-four positive Taylor terms avoid the
 * cancellation of exp(x)-1 near zero. The exact truncation remainder is
 * <= e/25! < 1.8e-25; binary64 rounding, not truncation, dominates. */
function expMinusOne(x: number): number {
  let term = x, sum = x;
  for (let n = 2; n <= 24; n++) { term = term * x / n; sum += term; }
  return sum;
}

export function adoptionGain(stability: number, elapsedDays: number, kappa: number): number {
  if (!Number.isFinite(stability) || stability < 1 || stability > 3650
    || !Number.isFinite(elapsedDays) || elapsedDays < 0
    || !Number.isFinite(kappa) || kappa < 0 || kappa > 1) throw new RangeError("invalid adoption numeric domain");
  if (elapsedDays === 0 || kappa === 0 || stability === 3650) return 0;
  const r = 1 / squareRoot(1 + (19 / 81) * elapsedDays / stability);
  return stability * 5 * kappa * expMinusOne(1 - r) / tenthRoot(stability);
}
