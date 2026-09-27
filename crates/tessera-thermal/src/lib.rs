//! `tessera-thermal`: native compact RC thermal network (P3-F4).
//!
//! A HotSpot-style (D4) per-tile lumped model solving
//! `C·dT/dt = P − G_vert·(T − T_amb) − Σ_neighbours G_lat·(T − T_j)`
//! on a rectangular grid of tiles, where the capacitance `C`, the tile-to-
//! ambient conductance `G_vert`, and the lateral conductance `G_lat` are derived
//! from **editable, cited material + geometry parameters** in the parameter DB
//! ([`tessera_params::ThermalParams`]): silicon `k`, specific heat, density,
//! die thickness, tile area, tile-to-ambient resistance, and ambient temp
//! (INITIAL_PRD §9A).
//!
//! # Integration
//! The state is advanced with forward-Euler integration, internally sub-stepped
//! to a stability-safe step so a single [`ThermalModel::step`] call over a long
//! interval (the TEI loop's default 100k cycles, Phase 3 step 7) stays stable
//! and accurate. Sub-stepping and the fixed neighbour-iteration order keep the
//! solve deterministic (P3-N3).
//!
//! # Accuracy stance
//! Like `tessera-power`, absolute validation against HotSpot is a later
//! deliverable (FFI + regression, gate G3.1). The tests here assert solver
//! **behaviour**: the single-tile RC step response matches the analytic
//! transient `T(t) = T_amb + P·R·(1 − e^{−t/RC})`, steady state matches
//! `T_amb + P·R`, lateral coupling equalises symmetric loads, and the solve is
//! stable under a large step.

use tessera_params::ParamDb;

pub mod tei;

/// Power in watts.
pub type Watts = f64;
/// Temperature in degrees Celsius.
pub type Celsius = f32;

/// Fraction of the explicit-Euler stability limit each internal sub-step takes.
/// Below 1.0 for both stability margin and transient accuracy.
const SUBSTEP_SAFETY: f64 = 0.25;

/// A thermal model: given per-tile power and an elapsed time, advance the tile
/// temperatures and return them (°C per tile), in tile order.
pub trait ThermalModel: Send + Sync {
    /// Advance by `dt_s` seconds under the given per-tile power draw and return
    /// the updated per-tile temperatures.
    fn step(&mut self, power_w: &[Watts], dt_s: f32) -> &[Celsius];
    /// The current per-tile temperatures without advancing.
    fn temperatures(&self) -> &[Celsius];
    /// Reset every tile to ambient.
    fn reset(&mut self);
}

/// A compact RC thermal network over a `rows × cols` grid of uniform tiles.
///
/// Tiles are indexed row-major (`tile = row*cols + col`), matching the natural
/// router ordering of a 2-D mesh; `power_w[tile]` is the heat dissipated by that
/// tile over the step.
#[derive(Clone, Debug)]
pub struct RcGrid {
    rows: usize,
    cols: usize,
    /// Thermal capacitance per tile (J/K).
    cap: f64,
    /// Tile-to-ambient conductance per tile (W/K) = 1/R_conv.
    g_vert: f64,
    /// Lateral conductance between orthogonally-adjacent tiles (W/K); 0 disables
    /// lateral coupling.
    g_lat: f64,
    /// Ambient temperature (°C).
    t_amb: f64,
    /// Tile temperatures (°C), computed in f64 for accuracy.
    temp: Vec<f64>,
    /// Scratch buffer for the double-buffered Euler update.
    scratch: Vec<f64>,
    /// f32 view returned to callers.
    out: Vec<Celsius>,
}

impl RcGrid {
    /// Build a grid directly from explicit lumped R/C values: used by tests and
    /// callers that already have calibrated coefficients.
    ///
    /// `cap` is J/K per tile, `g_vert = 1/R_conv` in W/K, `g_lat` the lateral
    /// conductance in W/K (0 to disable lateral coupling).
    pub fn new(rows: usize, cols: usize, cap: f64, g_vert: f64, g_lat: f64, t_amb: f64) -> Self {
        let n = rows * cols;
        Self {
            rows,
            cols,
            cap,
            g_vert,
            g_lat,
            t_amb,
            temp: vec![t_amb; n],
            scratch: vec![0.0; n],
            out: vec![t_amb as f32; n],
        }
    }

    /// Build a grid from the cited parameter DB (P3-F4). Derives:
    /// - `C = ρ·c_p·(A_tile·t_die)`: tile heat capacity,
    /// - `G_vert = 1/R_conv`: tile-to-ambient conductance,
    /// - `G_lat = k·t_die`: lateral conductance across the shared tile face
    ///   (the tile-edge length cancels for square tiles: `A_face/pitch =
    ///   (t_die·edge)/edge = t_die`).
    ///
    /// `lateral = false` yields decoupled per-tile RC elements.
    pub fn from_params(rows: usize, cols: usize, p: &ParamDb, lateral: bool) -> Self {
        let t = &p.thermal;
        let volume = t.tile_area_m2.value * t.die_thickness_m.value;
        let cap = t.si_density.value * t.si_cp.value * volume;
        let g_vert = 1.0 / t.r_conv_k_per_w.value;
        let g_lat = if lateral {
            t.si_k.value * t.die_thickness_m.value
        } else {
            0.0
        };
        Self::new(rows, cols, cap, g_vert, g_lat, t.t_amb_c.value)
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn num_tiles(&self) -> usize {
        self.temp.len()
    }

    /// Steady-state temperature of one isolated tile under constant power `p_w`:
    /// `T_amb + p_w·R_conv`. (Only exact when lateral coupling is off or all
    /// tiles carry equal power.)
    pub fn isolated_steady_state(&self, p_w: Watts) -> Celsius {
        (self.t_amb + p_w / self.g_vert) as f32
    }

    /// Largest explicit-Euler step that stays stable, from the worst-case tile
    /// conductance (interior tiles have the most neighbours).
    fn stable_dt(&self) -> f64 {
        let max_neighbours = if self.g_lat > 0.0 {
            // Interior tile degree on a grid (capped by actual grid extent).
            let deg = (self.rows > 1) as usize * 2 + (self.cols > 1) as usize * 2;
            deg as f64
        } else {
            0.0
        };
        let g_total = self.g_vert + max_neighbours * self.g_lat;
        if g_total <= 0.0 {
            f64::INFINITY
        } else {
            2.0 * self.cap / g_total
        }
    }

    #[inline]
    fn neighbours(&self, tile: usize) -> [Option<usize>; 4] {
        let (r, c) = (tile / self.cols, tile % self.cols);
        [
            (r > 0).then(|| tile - self.cols),               // up
            (r + 1 < self.rows).then_some(tile + self.cols), // down
            (c > 0).then(|| tile - 1),                       // left
            (c + 1 < self.cols).then_some(tile + 1),         // right
        ]
    }

    /// One forward-Euler sub-step of duration `dt` (seconds).
    fn euler_substep(&mut self, power_w: &[Watts], dt: f64) {
        let inv_cap = dt / self.cap;
        for (tile, &pw) in power_w.iter().enumerate() {
            let ti = self.temp[tile];
            let mut flow = pw - self.g_vert * (ti - self.t_amb);
            if self.g_lat > 0.0 {
                for nb in self.neighbours(tile).into_iter().flatten() {
                    flow -= self.g_lat * (ti - self.temp[nb]);
                }
            }
            self.scratch[tile] = ti + inv_cap * flow;
        }
        std::mem::swap(&mut self.temp, &mut self.scratch);
    }

    fn refresh_out(&mut self) {
        for (o, &t) in self.out.iter_mut().zip(self.temp.iter()) {
            *o = t as f32;
        }
    }
}

impl ThermalModel for RcGrid {
    fn step(&mut self, power_w: &[Watts], dt_s: f32) -> &[Celsius] {
        assert_eq!(
            power_w.len(),
            self.temp.len(),
            "power vector must have one entry per tile"
        );
        let dt = dt_s as f64;
        if dt > 0.0 {
            let stable = self.stable_dt();
            let target = SUBSTEP_SAFETY * stable;
            let n_sub = if target.is_finite() && target > 0.0 {
                (dt / target).ceil().max(1.0) as u64
            } else {
                1
            };
            let sub_dt = dt / n_sub as f64;
            for _ in 0..n_sub {
                self.euler_substep(power_w, sub_dt);
            }
        }
        self.refresh_out();
        &self.out
    }

    fn temperatures(&self) -> &[Celsius] {
        &self.out
    }

    fn reset(&mut self) {
        self.temp.fill(self.t_amb);
        self.refresh_out();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_params::ParamDb;

    const PTM: &str = include_str!("../../../params/ptm_22nm.toml");

    /// Run `grid.step` repeatedly until it settles, returning the final temps.
    fn settle(grid: &mut RcGrid, power: &[Watts], dt: f32, steps: usize) -> Vec<Celsius> {
        let mut last = grid.temperatures().to_vec();
        for _ in 0..steps {
            last = grid.step(power, dt).to_vec();
        }
        last
    }

    #[test]
    fn starts_and_resets_at_ambient() {
        let mut g = RcGrid::new(2, 2, 1.0, 1.0, 0.0, 25.0);
        assert!(g.temperatures().iter().all(|&t| (t - 25.0).abs() < 1e-6));
        // Zero power keeps it at ambient.
        let out = g.step(&[0.0; 4], 1.0);
        assert!(out.iter().all(|&t| (t - 25.0).abs() < 1e-4));
        // Heat it, then reset.
        g.step(&[10.0; 4], 1.0);
        assert!(g.temperatures().iter().any(|&t| t > 25.0));
        g.reset();
        assert!(g.temperatures().iter().all(|&t| (t - 25.0).abs() < 1e-6));
    }

    #[test]
    fn single_tile_reaches_analytic_steady_state() {
        // C=1, R=1 (g_vert=1), P=2 -> steady T = amb + P*R = 25 + 2.
        let mut g = RcGrid::new(1, 1, 1.0, 1.0, 0.0, 25.0);
        let final_t = settle(&mut g, &[2.0], 0.5, 400);
        assert!(
            (final_t[0] - 27.0).abs() < 1e-3,
            "steady state {} != 27.0",
            final_t[0]
        );
        assert!((g.isolated_steady_state(2.0) - 27.0).abs() < 1e-6);
    }

    #[test]
    fn single_tile_matches_analytic_transient() {
        // tau = R*C = 1 s. After t = tau, T = amb + P*R*(1 - e^-1).
        let (cap, g_vert, p, amb) = (1.0, 1.0, 1.0_f64, 20.0);
        let mut g = RcGrid::new(1, 1, cap, g_vert, 0.0, amb);
        let tau = cap / g_vert;
        // Advance exactly one time constant in one call (internally sub-stepped).
        let out = g.step(&[p], tau as f32);
        let analytic = amb + p * (1.0 / g_vert) * (1.0 - (-1.0_f64).exp());
        assert!(
            (out[0] as f64 - analytic).abs() < analytic.abs() * 0.01,
            "transient {} vs analytic {analytic}",
            out[0]
        );
    }

    #[test]
    fn lateral_coupling_equalises_symmetric_load() {
        // Two coupled tiles, equal power -> equal temperature.
        let mut g = RcGrid::new(1, 2, 1.0, 1.0, 0.5, 25.0);
        let out = settle(&mut g, &[3.0, 3.0], 0.2, 500);
        assert!((out[0] - out[1]).abs() < 1e-4, "{} vs {}", out[0], out[1]);
    }

    #[test]
    fn lateral_coupling_spreads_asymmetric_load() {
        // Only tile 0 is heated. With coupling, tile 1 warms above ambient but
        // stays cooler than tile 0, and tile 0 is pulled below its isolated
        // steady state by conducting heat sideways.
        let mut coupled = RcGrid::new(1, 2, 1.0, 1.0, 0.5, 25.0);
        let out = settle(&mut coupled, &[4.0, 0.0], 0.2, 800);
        assert!(
            out[0] > out[1],
            "hot {} should exceed cold {}",
            out[0],
            out[1]
        );
        assert!(
            out[1] > 25.0,
            "cold tile {} should warm above ambient",
            out[1]
        );
        let isolated = coupled.isolated_steady_state(4.0); // 29.0
        assert!(
            out[0] < isolated,
            "coupled hot tile {} should be below isolated {}",
            out[0],
            isolated
        );
    }

    #[test]
    fn hotter_tile_tracks_more_power() {
        let mut g = RcGrid::new(1, 3, 1.0, 1.0, 0.3, 25.0);
        let out = settle(&mut g, &[1.0, 5.0, 1.0], 0.2, 800);
        assert!(
            out[1] > out[0] && out[1] > out[2],
            "middle tile should be hottest"
        );
        assert!(
            (out[0] - out[2]).abs() < 1e-4,
            "symmetric ends should match"
        );
    }

    #[test]
    fn large_single_step_is_stable_and_matches_incremental() {
        // A single huge step (sub-stepped internally) must not blow up and must
        // land near the same steady state as many small steps.
        let mut small = RcGrid::new(3, 3, 2.0, 1.5, 0.4, 30.0);
        let p = vec![2.0; 9];
        let incremental = settle(&mut small, &p, 0.1, 5000);

        let mut big = RcGrid::new(3, 3, 2.0, 1.5, 0.4, 30.0);
        let one_shot = big.step(&p, 1000.0).to_vec();

        assert!(one_shot.iter().all(|t| t.is_finite()), "must stay finite");
        for (a, b) in one_shot.iter().zip(incremental.iter()) {
            assert!((a - b).abs() < 0.1, "one-shot {a} vs incremental {b}");
        }
    }

    #[test]
    fn solve_is_deterministic() {
        let build = || RcGrid::new(4, 4, 1.5, 1.2, 0.3, 40.0);
        let power: Vec<Watts> = (0..16).map(|i| (i % 3) as f64).collect();
        let mut a = build();
        let mut b = build();
        for _ in 0..50 {
            a.step(&power, 0.05);
            b.step(&power, 0.05);
        }
        assert_eq!(a.temperatures(), b.temperatures());
    }

    #[test]
    fn from_params_builds_a_physical_grid() {
        let p = ParamDb::from_toml(PTM).unwrap();
        let mut g = RcGrid::from_params(2, 2, &p, true);
        assert_eq!(g.num_tiles(), 4);
        // Ambient matches the profile.
        let amb = p.thermal.t_amb_c.value as f32;
        assert!((g.temperatures()[0] - amb).abs() < 1e-4);
        // Under load, all tiles heat above ambient and stay finite.
        let out = settle(&mut g, &[0.05; 4], 1e-5, 2000);
        assert!(out.iter().all(|&t| t > amb && t.is_finite()));
    }

    #[test]
    #[should_panic(expected = "one entry per tile")]
    fn wrong_power_length_panics() {
        let mut g = RcGrid::new(2, 2, 1.0, 1.0, 0.0, 25.0);
        g.step(&[1.0, 2.0], 1.0);
    }
}
