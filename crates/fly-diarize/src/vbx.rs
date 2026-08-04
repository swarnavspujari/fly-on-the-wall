//! VBx: variational Bayes HMM clustering of speaker embeddings.
//!
//! Rust port of `VBx.py` from BUTSpeechFIT/VBx (Apache-2.0, Copyright 2021
//! Lukáš Burget, Mireia Diez), the reference implementation of:
//!   Landini, Profant, Diez, Burget: "Bayesian HMM clustering of x-vector
//!   sequences (VBx) in speaker diarization", Computer Speech & Language 2022.
//!
//! The model: embeddings x_t ~ N(V·y_s, I) with diagonal across-class
//! covariance `phi` (V = sqrt(phi)) and identity within-class covariance;
//! an HMM over speakers with self-loop probability `loop_prob` handles the
//! temporal structure. Redundant initial clusters collapse as their priors
//! go to zero — which is exactly the over-splitting failure mode of plain
//! agglomerative clustering.
//!
//! Equation numbers in comments refer to the paper.

#[derive(Debug, Clone)]
pub struct VbxParams {
    /// HMM probability of staying with the current speaker between frames.
    pub loop_prob: f64,
    /// Acoustic scaling factor on the sufficient statistics.
    pub fa: f64,
    /// Speaker regularization coefficient — larger = fewer final speakers.
    pub fb: f64,
    pub max_iters: usize,
    pub epsilon: f64,
    /// Softmax sharpness when converting hard init labels to soft gamma.
    pub init_smoothing: f64,
}

impl Default for VbxParams {
    fn default() -> Self {
        Self {
            // Measured on the committed fixtures (docs/BENCHMARKS.md,
            // Phase 3): the harness grid was insensitive across
            // Fa ∈ {0.1..1.0}, Fb ∈ {4..64}, loopP ∈ {0.9, 0.99}, with
            // Fa = 0.1 best everywhere; this exact combo validated on all
            // three fixtures.
            loop_prob: 0.99,
            fa: 0.1,
            fb: 17.0,
            max_iters: 40,
            epsilon: 1e-6,
            init_smoothing: 5.0,
        }
    }
}

pub struct VbxOutcome {
    /// Per-frame winning speaker (index into the INITIAL cluster set).
    pub labels: Vec<usize>,
    /// Learned speaker priors (redundant speakers converge to ~0).
    pub pi: Vec<f64>,
    /// ELBO trajectory, one value per iteration.
    pub elbo: Vec<f64>,
}

fn logsumexp(v: &[f64]) -> f64 {
    let m = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if m == f64::NEG_INFINITY {
        return m;
    }
    m + v.iter().map(|x| (x - m).exp()).sum::<f64>().ln()
}

/// Forward-backward over the speaker HMM. `lls` is T×S log output probs
/// (row-major), `tr` S×S transition matrix, `ip` initial probs.
/// Returns (gamma T×S, total log-likelihood, log-forward, log-backward).
#[allow(clippy::type_complexity)]
fn forward_backward(
    lls: &[f64],
    t: usize,
    s: usize,
    tr: &[f64],
    ip: &[f64],
) -> (Vec<f64>, f64, Vec<f64>, Vec<f64>) {
    const EPS: f64 = 1e-8;
    let ltr: Vec<f64> = tr.iter().map(|p| (p + EPS).ln()).collect();
    let mut lfw = vec![f64::NEG_INFINITY; t * s];
    let mut lbw = vec![f64::NEG_INFINITY; t * s];
    for j in 0..s {
        lfw[j] = lls[j] + (ip[j] + EPS).ln();
        lbw[(t - 1) * s + j] = 0.0;
    }
    let mut scratch = vec![0.0f64; s];
    for i in 1..t {
        for j in 0..s {
            for (k, sc) in scratch.iter_mut().enumerate() {
                *sc = lfw[(i - 1) * s + k] + ltr[k * s + j];
            }
            lfw[i * s + j] = lls[i * s + j] + logsumexp(&scratch);
        }
    }
    for i in (0..t - 1).rev() {
        for j in 0..s {
            for (k, sc) in scratch.iter_mut().enumerate() {
                *sc = ltr[j * s + k] + lls[(i + 1) * s + k] + lbw[(i + 1) * s + k];
            }
            lbw[i * s + j] = logsumexp(&scratch);
        }
    }
    let tll = logsumexp(&lfw[(t - 1) * s..t * s]);
    let mut gamma = vec![0.0f64; t * s];
    for i in 0..t * s {
        gamma[i] = (lfw[i] + lbw[i] - tll).exp();
    }
    (gamma, tll, lfw, lbw)
}

/// Run VBx over `t` embeddings of dimension `d` (`x` row-major, already in
/// the scoring space), with diagonal across-class covariance `phi` (len `d`)
/// and hard initial labels in `0..n_init`.
pub fn vbx(
    x: &[f64],
    t: usize,
    d: usize,
    phi: &[f64],
    init_labels: &[usize],
    n_init: usize,
    params: &VbxParams,
) -> VbxOutcome {
    assert_eq!(x.len(), t * d);
    assert_eq!(init_labels.len(), t);
    assert!(n_init > 0 && t > 0);
    let s = n_init;

    // soft init gamma: softmax(one_hot * init_smoothing) — vbhmm.py's
    // init-smoothing step
    let hot = params.init_smoothing.exp();
    let cold = 1.0f64.exp() * 0.0 + 1.0; // exp(0)
    let mut gamma = vec![0.0f64; t * s];
    for (i, &l) in init_labels.iter().enumerate() {
        let denom = hot + cold * (s as f64 - 1.0);
        for j in 0..s {
            gamma[i * s + j] = if j == l { hot / denom } else { cold / denom };
        }
    }
    let mut pi = vec![1.0 / s as f64; s];

    // per-frame constant term in (23)
    let g: Vec<f64> = (0..t)
        .map(|i| {
            let ss: f64 = x[i * d..(i + 1) * d].iter().map(|v| v * v).sum();
            -0.5 * (ss + d as f64 * (2.0 * std::f64::consts::PI).ln())
        })
        .collect();
    // rho = X * sqrt(phi)  (18)
    let v: Vec<f64> = phi.iter().map(|p| p.sqrt()).collect();
    let mut rho = vec![0.0f64; t * d];
    for i in 0..t {
        for k in 0..d {
            rho[i * d + k] = x[i * d + k] * v[k];
        }
    }

    let mut elbo_track: Vec<f64> = Vec::new();
    let mut labels = init_labels.to_vec();
    for _iter in 0..params.max_iters {
        // (17) invL and (16) alpha for all speakers
        let mut gamma_sum = vec![0.0f64; s];
        for i in 0..t {
            for j in 0..s {
                gamma_sum[j] += gamma[i * s + j];
            }
        }
        let mut inv_l = vec![0.0f64; s * d];
        let mut alpha = vec![0.0f64; s * d];
        for j in 0..s {
            for k in 0..d {
                inv_l[j * d + k] = 1.0 / (1.0 + params.fa / params.fb * gamma_sum[j] * phi[k]);
            }
        }
        // gamma^T · rho
        for j in 0..s {
            for i in 0..t {
                let gij = gamma[i * s + j];
                if gij < 1e-12 {
                    continue;
                }
                for k in 0..d {
                    alpha[j * d + k] += gij * rho[i * d + k];
                }
            }
            for k in 0..d {
                alpha[j * d + k] *= params.fa / params.fb * inv_l[j * d + k];
            }
        }
        // (23) log p_ts = Fa*(rho·alpha_s − 0.5·(invL+alpha²)·phi + G_t)
        let mut spk_const = vec![0.0f64; s];
        for j in 0..s {
            let mut acc = 0.0;
            for k in 0..d {
                acc += (inv_l[j * d + k] + alpha[j * d + k] * alpha[j * d + k]) * phi[k];
            }
            spk_const[j] = -0.5 * acc;
        }
        let mut log_p = vec![0.0f64; t * s];
        for i in 0..t {
            for j in 0..s {
                let mut dot = 0.0;
                for k in 0..d {
                    dot += rho[i * d + k] * alpha[j * d + k];
                }
                log_p[i * s + j] = params.fa * (dot + spk_const[j] + g[i]);
            }
        }
        // (1) transitions
        let mut tr = vec![0.0f64; s * s];
        for a in 0..s {
            for b in 0..s {
                tr[a * s + b] =
                    if a == b { params.loop_prob } else { 0.0 } + (1.0 - params.loop_prob) * pi[b];
            }
        }
        let (new_gamma, log_px, lfw, lbw) = forward_backward(&log_p, t, s, &tr, &pi);
        gamma = new_gamma;

        // (25) ELBO
        let mut reg = 0.0;
        for j in 0..s {
            for k in 0..d {
                let il = inv_l[j * d + k];
                let a = alpha[j * d + k];
                reg += il.ln() - il - a * a + 1.0;
            }
        }
        let elbo = log_px + params.fb * 0.5 * reg;

        // (24) pi update
        let mut new_pi = vec![0.0f64; s];
        for j in 0..s {
            new_pi[j] = gamma[j];
        }
        for i in 1..t {
            let row_lse = logsumexp(&lfw[(i - 1) * s..i * s]);
            for j in 0..s {
                new_pi[j] += (1.0 - params.loop_prob)
                    * pi[j]
                    * (row_lse + log_p[i * s + j] + lbw[i * s + j] - log_px).exp();
            }
        }
        let total: f64 = new_pi.iter().sum();
        for p in new_pi.iter_mut() {
            *p /= total.max(f64::MIN_POSITIVE);
        }
        pi = new_pi;

        let done = elbo_track
            .last()
            .is_some_and(|prev| elbo - prev < params.epsilon);
        elbo_track.push(elbo);
        if done {
            break;
        }
    }

    for (i, l) in labels.iter_mut().enumerate() {
        *l = (0..s)
            .max_by(|&a, &b| gamma[i * s + a].total_cmp(&gamma[i * s + b]))
            .expect("s > 0");
    }
    VbxOutcome {
        labels,
        pi,
        elbo: elbo_track,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two well-separated synthetic "voices" in 4-D, deliberately shattered
    /// into 4 init clusters — VBx must merge them back to 2 speakers.
    #[test]
    fn merges_shattered_clusters() {
        let d = 4;
        let a = [3.0, 0.0, 0.0, 0.0];
        let b = [0.0, 3.0, 0.0, 0.0];
        let mut x = Vec::new();
        let mut init = Vec::new();
        let mut truth = Vec::new();
        // A(60) as init 0/2 alternating blocks, then B(60) as init 1/3
        for i in 0..60 {
            let noise = ((i * 37 % 13) as f64 - 6.0) / 60.0;
            x.extend(a.iter().map(|v| v + noise));
            init.push(if (i / 15) % 2 == 0 { 0 } else { 2 });
            truth.push(0usize);
        }
        for i in 0..60 {
            let noise = ((i * 29 % 11) as f64 - 5.0) / 55.0;
            x.extend(b.iter().map(|v| v + noise));
            init.push(if (i / 15) % 2 == 0 { 1 } else { 3 });
            truth.push(1usize);
        }
        let phi = vec![2.0; d];
        let out = vbx(&x, 120, d, &phi, &init, 4, &VbxParams::default());
        let distinct: std::collections::BTreeSet<usize> = out.labels.iter().cloned().collect();
        assert_eq!(distinct.len(), 2, "pi = {:?}", out.pi);
        // all A-frames one label, all B-frames another
        let la = out.labels[0];
        assert!(out.labels[..60].iter().all(|&l| l == la));
        let lb = out.labels[60];
        assert_ne!(la, lb);
        assert!(out.labels[60..].iter().all(|&l| l == lb));
    }

    #[test]
    fn elbo_is_nondecreasing() {
        let d = 3;
        let mut x = Vec::new();
        let mut init = Vec::new();
        for i in 0..40 {
            let s = i / 20;
            let base = if s == 0 {
                [2.0, 0.0, 0.5]
            } else {
                [0.0, 2.0, -0.5]
            };
            let noise = ((i * 17 % 7) as f64 - 3.0) / 20.0;
            x.extend(base.iter().map(|v| v + noise));
            init.push(i % 3);
        }
        let phi = vec![1.5; d];
        let out = vbx(&x, 40, d, &phi, &init, 3, &VbxParams::default());
        for w in out.elbo.windows(2) {
            assert!(w[1] >= w[0] - 1e-6, "ELBO decreased: {:?}", out.elbo);
        }
    }

    #[test]
    fn single_speaker_collapses_to_one() {
        let d = 4;
        let mut x = Vec::new();
        let mut init = Vec::new();
        for i in 0..80 {
            let noise = ((i * 31 % 17) as f64 - 8.0) / 80.0;
            x.extend([1.5 + noise, 1.0 - noise, 0.0, 0.5]);
            init.push(i % 4); // wildly over-split init
        }
        let phi = vec![2.0; d];
        let out = vbx(&x, 80, d, &phi, &init, 4, &VbxParams::default());
        let distinct: std::collections::BTreeSet<usize> = out.labels.iter().cloned().collect();
        assert_eq!(distinct.len(), 1, "pi = {:?}", out.pi);
    }
}
