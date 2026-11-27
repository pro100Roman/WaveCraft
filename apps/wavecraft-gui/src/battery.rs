//! UI-independent, bounded energy integration for a repeating constant-power load.
//! Curves describe open-circuit V/V_initial versus discharged capacity (0..1).

use serde::{Deserialize, Serialize};
use std::path::Path;

pub(crate) const MAX_PATTERNS: usize = 50;
const STEPS: usize = 4096;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Chemistry {
    LiIon,
    LiPo,
    LiFePo4,
    Lto,
    NiMh,
    NiCd,
    LeadAcid,
    Alkaline,
    ZincCarbon,
    LiFeS2,
    LiMnO2,
    LiSoCl2,
    SilverOxide,
    ZincAir,
}

impl Chemistry {
    pub(crate) const ALL: [Self; 14] = [
        Self::LiIon,
        Self::LiPo,
        Self::LiFePo4,
        Self::Lto,
        Self::NiMh,
        Self::NiCd,
        Self::LeadAcid,
        Self::Alkaline,
        Self::ZincCarbon,
        Self::LiFeS2,
        Self::LiMnO2,
        Self::LiSoCl2,
        Self::SilverOxide,
        Self::ZincAir,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LiIon => "Li-ion · rechargeable",
            Self::LiPo => "LiPo · rechargeable",
            Self::LiFePo4 => "LiFePO₄ · rechargeable",
            Self::Lto => "LTO · rechargeable",
            Self::NiMh => "NiMH · rechargeable",
            Self::NiCd => "NiCd · rechargeable",
            Self::LeadAcid => "Lead-acid · rechargeable",
            Self::Alkaline => "Alkaline · primary",
            Self::ZincCarbon => "Zinc-carbon · primary",
            Self::LiFeS2 => "Li-FeS₂ · primary",
            Self::LiMnO2 => "Li-MnO₂ · primary",
            Self::LiSoCl2 => "Li-SOCl₂ · primary",
            Self::SilverOxide => "Silver oxide · primary",
            Self::ZincAir => "Zinc-air · primary",
        }
    }

    /// Illustrative initial/end voltages, initial-drop weight, linear weight,
    /// and knee exponent. These are generic shapes, not fitted manufacturer data.
    fn profile(self) -> (f64, f64, f64, f64, i32) {
        match self {
            Self::LiIon | Self::LiPo => (4.2, 2.8, 0.22, 0.46, 8),
            Self::LiFePo4 => (3.65, 2.5, 0.32, 0.12, 12),
            Self::Lto => (2.8, 1.5, 0.22, 0.40, 8),
            Self::NiMh | Self::NiCd => (1.45, 1.0, 0.36, 0.16, 10),
            Self::LeadAcid => (2.15, 1.75, 0.12, 0.55, 6),
            Self::Alkaline => (1.6, 0.8, 0.10, 0.80, 4),
            Self::ZincCarbon => (1.6, 0.8, 0.16, 0.70, 4),
            Self::LiFeS2 => (1.8, 0.9, 0.25, 0.10, 12),
            Self::LiMnO2 => (3.2, 2.0, 0.18, 0.25, 8),
            Self::LiSoCl2 => (3.67, 2.0, 0.08, 0.05, 16),
            Self::SilverOxide => (1.6, 1.2, 0.12, 0.06, 14),
            Self::ZincAir => (1.45, 0.9, 0.16, 0.08, 12),
        }
    }

    pub(crate) fn sizes(self) -> &'static [&'static str] {
        match self {
            Self::LiIon => &[
                "18650", "21700", "26650", "14500", "16340", "18350", "10440",
            ],
            Self::LiPo => &["Pouch", "103450", "603040"],
            Self::LiFePo4 => &["18650", "26650", "32700", "14500", "Prismatic"],
            Self::Lto => &["18650", "26650", "Prismatic"],
            Self::NiMh | Self::NiCd => &["AA", "AAA", "AAAA", "C", "D", "9V / PP3"],
            Self::LeadAcid => &["2V cell", "6V pack", "12V pack"],
            Self::Alkaline | Self::ZincCarbon => {
                &["AA", "AAA", "AAAA", "C", "D", "9V / PP3", "N", "A23"]
            }
            Self::LiFeS2 => &["AA", "AAA"],
            Self::LiMnO2 => &["CR123A", "CR2", "CR2032", "CR2025", "CR2016", "CR2450"],
            Self::LiSoCl2 => &["ER14505 / AA", "ER14250 / ½AA", "ER34615 / D"],
            Self::SilverOxide => &["SR44", "SR626", "SR621"],
            Self::ZincAir => &["PR10", "PR13", "PR312", "PR675"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CurveKind {
    Chemistry,
    Polynomial,
    Table,
}

impl CurveKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Chemistry => "Chemistry approximation",
            Self::Polynomial => "Polynomial",
            Self::Table => "Measured table",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LoadPattern {
    pub(crate) power_mw: f64,
    pub(crate) duration_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BatterySettings {
    pub(crate) chemistry: Chemistry,
    pub(crate) size: String,
    pub(crate) initial_voltage: f64,
    pub(crate) capacity_mah: f64,
    pub(crate) cutoff_voltage: f64,
    pub(crate) curve: CurveKind,
    pub(crate) polynomial_degree: u8,
    /// Ascending powers, normalized V/V_initial.
    pub(crate) coefficients: [f64; 6],
    /// [discharged capacity fraction, V/V_initial], piecewise linear.
    pub(crate) table: Vec<[f64; 2]>,
    pub(crate) patterns: Vec<LoadPattern>,
}

impl Default for BatterySettings {
    fn default() -> Self {
        let mut settings = Self {
            chemistry: Chemistry::LiIon,
            size: "18650".into(),
            initial_voltage: 4.2,
            capacity_mah: 2600.0,
            cutoff_voltage: 3.0,
            curve: CurveKind::Chemistry,
            polynomial_degree: 5,
            coefficients: [0.0; 6],
            table: Vec::new(),
            patterns: vec![
                LoadPattern {
                    power_mw: 100.0,
                    duration_ms: 100.0,
                },
                LoadPattern {
                    power_mw: 1.0,
                    duration_ms: 900.0,
                },
            ],
        };
        settings.reset_curve();
        settings
    }
}

impl BatterySettings {
    pub(crate) fn select_chemistry(&mut self, chemistry: Chemistry) {
        self.chemistry = chemistry;
        let (initial, end, ..) = chemistry.profile();
        self.initial_voltage = initial;
        self.cutoff_voltage = end;
        self.size = chemistry.sizes()[0].into();
        self.curve = CurveKind::Chemistry;
        self.reset_curve();
    }

    pub(crate) fn reset_curve(&mut self) {
        let (initial, end, ..) = self.chemistry.profile();
        let drop = 1.0 - end / initial;
        self.polynomial_degree = 5;
        self.coefficients = [1.0, -drop * 0.55, 0.0, 0.0, 0.0, -drop * 0.45];
        self.table = (0..=10)
            .map(|i| {
                let x = f64::from(i) / 10.0;
                [x, self.chemistry_ratio(x)]
            })
            .collect();
    }

    fn chemistry_ratio(&self, x: f64) -> f64 {
        let (initial, end, a, b, knee) = self.chemistry.profile();
        let drop = a * (1.0 - (-20.0 * x).exp()) / (1.0 - (-20.0_f64).exp())
            + b * x
            + (1.0 - a - b) * x.powi(knee);
        1.0 - (1.0 - end / initial) * drop
    }

    pub(crate) fn voltage(&self, x: f64) -> f64 {
        let ratio = match self.curve {
            CurveKind::Chemistry => self.chemistry_ratio(x),
            CurveKind::Polynomial => self.coefficients
                [..=usize::from(self.polynomial_degree.min(5))]
                .iter()
                .rev()
                .fold(0.0, |value, coefficient| value * x + coefficient),
            CurveKind::Table => {
                let pair = self.table.windows(2).find(|pair| x <= pair[1][0]);
                if let Some(pair) = pair {
                    let t = (x - pair[0][0]) / (pair[1][0] - pair[0][0]);
                    pair[0][1] + t * (pair[1][1] - pair[0][1])
                } else {
                    self.table.last().map_or(f64::NAN, |point| point[1])
                }
            }
        };
        self.initial_voltage * ratio
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if !self.initial_voltage.is_finite()
            || !self.cutoff_voltage.is_finite()
            || self.cutoff_voltage <= 0.0
            || self.initial_voltage <= self.cutoff_voltage
        {
            return Err(
                "Initial voltage must be finite and greater than cutoff voltage > 0 V".into(),
            );
        }
        if !self.capacity_mah.is_finite() || self.capacity_mah <= 0.0 {
            return Err("Capacity must be finite and greater than 0 mAh".into());
        }
        if self.size.trim().is_empty() || self.size.len() > 128 {
            return Err("Battery size must contain 1–128 bytes".into());
        }
        if !(1..=5).contains(&self.polynomial_degree)
            || self.coefficients.iter().any(|c| !c.is_finite())
        {
            return Err("Polynomial requires degree 1–5 and finite coefficients".into());
        }
        // Inactive curves are exported too: never let NaN become JSON null,
        // which would make a file produced by this view impossible to reopen.
        if self.table.len() > 256 || self.table.iter().flatten().any(|v| !v.is_finite()) {
            return Err("Curve table must contain at most 256 points with finite values".into());
        }
        if self.curve == CurveKind::Table
            && (self.table.len() < 2
                || self.table.len() > 256
                || self
                    .table
                    .first()
                    .map_or(true, |p| p[0] != 0.0 || (p[1] - 1.0).abs() > 1e-9)
                || self.table.last().map_or(true, |p| p[0] != 1.0)
                || self
                    .table
                    .windows(2)
                    .any(|p| p[0][0] >= p[1][0] || p[1][1] > p[0][1] || p[1][1] <= 0.0))
        {
            return Err("Table needs 2–256 ordered points, x from 0 to 1, starting V/V₀ = 1, positive nonincreasing voltage".into());
        }
        let mut previous = self.initial_voltage;
        for i in 0..=STEPS {
            let voltage = self.voltage(i as f64 / STEPS as f64);
            if !voltage.is_finite()
                || voltage <= 0.0
                || voltage > previous + self.initial_voltage * 1e-10
                || (i == 0 && (voltage / self.initial_voltage - 1.0).abs() > 1e-9)
            {
                return Err(
                    "Discharge curve must start at V₀, stay positive and never increase on x = 0…1"
                        .into(),
                );
            }
            previous = voltage;
        }
        self.cycle()?;
        Ok(())
    }

    fn cycle(&self) -> Result<(f64, f64), String> {
        if self.patterns.is_empty() || self.patterns.len() > MAX_PATTERNS {
            return Err("Add between 1 and 50 consumption patterns".into());
        }
        let mut duration = 0.0;
        let mut energy = 0.0;
        for (index, pattern) in self.patterns.iter().enumerate() {
            if !pattern.power_mw.is_finite()
                || pattern.power_mw < 0.0
                || !pattern.duration_ms.is_finite()
                || pattern.duration_ms <= 0.0
            {
                return Err(format!(
                    "Pattern {} requires finite power ≥ 0 mW and duration > 0 ms",
                    index + 1
                ));
            }
            duration += pattern.duration_ms / 3_600_000.0;
            energy += pattern.power_mw * (pattern.duration_ms / 3_600_000.0);
        }
        if !duration.is_finite()
            || duration <= 0.0
            || !energy.is_finite()
            || energy <= 0.0
            || !(energy / duration).is_finite()
        {
            return Err(
                "The repeating cycle must have finite duration and positive finite energy".into(),
            );
        }
        Ok((duration, energy))
    }

    pub(crate) fn add_pattern(&mut self) {
        if self.patterns.len() < MAX_PATTERNS {
            self.patterns.push(LoadPattern {
                power_mw: 1.0,
                duration_ms: 1000.0,
            });
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BatteryResult {
    /// [hours, volts]. At most STEPS + 1 points, independent of lifetime.
    pub(crate) points: Vec<[f64; 2]>,
    pub(crate) lifetime_hours: f64,
    pub(crate) energy_mwh: f64,
    pub(crate) average_power_mw: f64,
    pub(crate) cutoff_reached: bool,
}

/// Invert the repeating load's cumulative energy without stepping each cycle.
fn time_for_energy(settings: &BatterySettings, energy: f64, cycle: (f64, f64)) -> f64 {
    if energy <= 0.0 {
        return 0.0;
    }
    // At exact cycle boundaries, stop at the consuming pattern, before any
    // trailing sleep interval. This also handles cutoff within the first cycle.
    let mut cycles = (energy / cycle.1).floor();
    let mut remainder = energy - cycles * cycle.1;
    if remainder <= cycle.1 * 1e-12 && cycles > 0.0 {
        cycles -= 1.0;
        remainder = cycle.1;
    }
    let mut hours = cycles * cycle.0;
    for pattern in &settings.patterns {
        let duration = pattern.duration_ms / 3_600_000.0;
        let consumed = pattern.power_mw * duration;
        if pattern.power_mw > 0.0 && remainder <= consumed {
            return hours + remainder / pattern.power_mw;
        }
        hours += duration;
        remainder -= consumed;
    }
    hours
}

pub(crate) fn calculate(settings: &BatterySettings) -> Result<BatteryResult, String> {
    settings.validate()?;
    let cycle = settings.cycle()?;
    let mut end = 1.0;
    let cutoff_reached = settings.voltage(1.0) <= settings.cutoff_voltage;
    if cutoff_reached {
        let mut low = 0.0;
        let mut high = 1.0;
        for _ in 0..60 {
            let middle = (low + high) * 0.5;
            if settings.voltage(middle) > settings.cutoff_voltage {
                low = middle;
            } else {
                high = middle;
            }
        }
        end = (low + high) * 0.5;
    }
    let dq = settings.capacity_mah * end / STEPS as f64;
    let mut energy = 0.0;
    let mut previous = settings.initial_voltage;
    let mut points = Vec::with_capacity(STEPS + 1);
    points.push([0.0, previous]);
    for i in 1..=STEPS {
        let voltage = settings.voltage(end * i as f64 / STEPS as f64);
        energy += (previous + voltage) * 0.5 * dq;
        let hours = time_for_energy(settings, energy, cycle);
        if !energy.is_finite() || !hours.is_finite() {
            return Err("Values are too large for a finite lifetime calculation".into());
        }
        points.push([hours, voltage]);
        previous = voltage;
    }
    let lifetime_hours = time_for_energy(settings, energy, cycle);
    if lifetime_hours <= 0.0 || energy <= 0.0 {
        return Err("Values are too small for a finite lifetime calculation".into());
    }
    Ok(BatteryResult {
        points,
        lifetime_hours,
        energy_mwh: energy,
        average_power_mw: cycle.1 / cycle.0,
        cutoff_reached,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatteryDocument {
    format: String,
    version: u32,
    settings: BatterySettings,
}

pub(crate) fn encode(settings: &BatterySettings) -> Result<String, String> {
    settings.validate()?;
    serde_json::to_string_pretty(&BatteryDocument {
        format: "wavecraft-battery".into(),
        version: 1,
        settings: settings.clone(),
    })
    .map_err(|error| error.to_string())
}

pub(crate) fn decode(json: &str) -> Result<BatterySettings, String> {
    if json.len() as u64 > MAX_FILE_BYTES {
        return Err("Battery file exceeds 1 MiB".into());
    }
    let document: BatteryDocument =
        serde_json::from_str(json).map_err(|error| error.to_string())?;
    if document.format != "wavecraft-battery" || document.version != 1 {
        return Err("Unsupported battery file format or version".into());
    }
    document.settings.validate()?;
    Ok(document.settings)
}

pub(crate) fn load(path: &Path) -> Result<BatterySettings, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut json = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut json)
        .map_err(|e| e.to_string())?;
    decode(&json)
}

pub(crate) struct BatteryState {
    pub(crate) settings: BatterySettings,
    pub(crate) calculated_settings: Option<BatterySettings>,
    pub(crate) result: Option<BatteryResult>,
    pub(crate) bounds: Option<[f64; 4]>,
    pub(crate) status: String,
    pub(crate) error: Option<String>,
}

impl Default for BatteryState {
    fn default() -> Self {
        Self {
            settings: BatterySettings::default(),
            calculated_settings: None,
            result: None,
            bounds: None,
            status: "Ready to calculate".into(),
            error: None,
        }
    }
}

impl BatteryState {
    pub(crate) fn calculate(&mut self) {
        match calculate(&self.settings) {
            Ok(result) => {
                self.result = Some(result);
                self.calculated_settings = Some(self.settings.clone());
                self.bounds = None;
                self.error = None;
                self.status = "Battery lifetime calculated".into();
            }
            Err(error) => {
                self.error = Some(error);
            }
        }
    }

    pub(crate) fn is_stale(&self) -> bool {
        self.calculated_settings
            .as_ref()
            .is_some_and(|s| s != &self.settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(voltage: f64, capacity: f64, power: f64) -> BatterySettings {
        BatterySettings {
            initial_voltage: voltage,
            cutoff_voltage: voltage / 2.0,
            capacity_mah: capacity,
            curve: CurveKind::Polynomial,
            polynomial_degree: 1,
            coefficients: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            patterns: vec![LoadPattern {
                power_mw: power,
                duration_ms: 1000.0,
            }],
            ..BatterySettings::default()
        }
    }

    #[test]
    fn constant_voltage_obeys_energy_units_and_stops_at_capacity() {
        let result = calculate(&constant(4.0, 1000.0, 100.0)).unwrap();
        assert!((result.energy_mwh - 4000.0).abs() < 1e-8);
        assert!((result.lifetime_hours - 40.0).abs() < 1e-8);
        assert!(!result.cutoff_reached);
    }

    #[test]
    fn declining_voltage_integrates_charge_and_stops_at_cutoff() {
        let mut settings = constant(4.0, 1000.0, 1000.0);
        settings.cutoff_voltage = 3.0;
        settings.coefficients[1] = -0.5;
        let result = calculate(&settings).unwrap();
        assert!((result.energy_mwh - 1750.0).abs() < 1e-8);
        assert!((result.lifetime_hours - 1.75).abs() < 1e-8);
        assert!((result.points.last().unwrap()[1] - 3.0).abs() < 1e-10);
        assert!(result.cutoff_reached);
    }

    #[test]
    fn repeats_patterns_and_stops_inside_pattern_before_trailing_sleep() {
        let mut settings = constant(1.0, 0.75, 1.0);
        settings.patterns = vec![
            LoadPattern {
                power_mw: 1.0,
                duration_ms: 1_800_000.0,
            },
            LoadPattern {
                power_mw: 0.0,
                duration_ms: 1_800_000.0,
            },
        ];
        assert!((calculate(&settings).unwrap().lifetime_hours - 1.25).abs() < 1e-10);
        settings.capacity_mah = 1.0;
        assert!((calculate(&settings).unwrap().lifetime_hours - 1.5).abs() < 1e-10);
        settings.patterns.reverse();
        assert!((calculate(&settings).unwrap().lifetime_hours - 2.0).abs() < 1e-10);
    }

    #[test]
    fn fractional_patterns_use_duration_weighted_power() {
        let mut settings = constant(1.0, 1.0, 1.0);
        settings.patterns = vec![
            LoadPattern {
                power_mw: 2.5,
                duration_ms: 0.25,
            },
            LoadPattern {
                power_mw: 0.5,
                duration_ms: 0.75,
            },
        ];
        let result = calculate(&settings).unwrap();
        assert!((result.average_power_mw - 1.0).abs() < 1e-12);
        assert!((result.lifetime_hours - 1.0).abs() < 1e-6);
    }

    #[test]
    fn tiny_loads_and_billions_of_cycles_have_bounded_work() {
        let mut settings = constant(3.0, 3000.0, 1e-6);
        settings.patterns[0].duration_ms = 0.001;
        let result = calculate(&settings).unwrap();
        assert!(result.points.len() <= STEPS + 1);
        assert!((result.lifetime_hours / 9e9 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn all_chemistries_have_finite_monotone_default_curves() {
        for chemistry in Chemistry::ALL {
            let mut settings = BatterySettings::default();
            settings.select_chemistry(chemistry);
            settings.validate().unwrap();
            for curve in [
                CurveKind::Chemistry,
                CurveKind::Polynomial,
                CurveKind::Table,
            ] {
                settings.curve = curve;
                let result = calculate(&settings).unwrap();
                assert!(result
                    .points
                    .windows(2)
                    .all(|p| p[1][0] >= p[0][0] && p[1][1] <= p[0][1]));
                assert!(result.lifetime_hours > 0.0, "{chemistry:?}");
            }
        }
    }

    #[test]
    fn higher_cutoff_shortens_lifetime_without_rescaling_the_curve() {
        let settings = BatterySettings::default();
        let before = calculate(&settings).unwrap();
        let mut higher = settings.clone();
        higher.cutoff_voltage = 3.8;
        let after = calculate(&higher).unwrap();
        assert!(after.lifetime_hours < before.lifetime_hours);
        assert_eq!(settings.voltage(0.5), higher.voltage(0.5));
    }

    #[test]
    fn rejects_invalid_parameters_zero_energy_and_excess_patterns() {
        let good = BatterySettings::default();
        for value in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let mut settings = good.clone();
            settings.capacity_mah = value;
            assert!(calculate(&settings).is_err());
            let mut settings = good.clone();
            settings.initial_voltage = value;
            assert!(calculate(&settings).is_err());
            let mut settings = good.clone();
            settings.patterns[0].duration_ms = value;
            assert!(calculate(&settings).is_err());
        }
        let mut settings = good.clone();
        settings.patterns.clear();
        assert!(calculate(&settings).is_err());
        for _ in 0..60 {
            settings.add_pattern();
        }
        assert_eq!(settings.patterns.len(), MAX_PATTERNS);
        settings.patterns.push(settings.patterns[0].clone());
        assert!(calculate(&settings).is_err());
        let mut settings = good.clone();
        for pattern in &mut settings.patterns {
            pattern.power_mw = 0.0;
        }
        assert!(calculate(&settings).is_err());
        let mut settings = good;
        settings.cutoff_voltage = settings.initial_voltage;
        assert!(calculate(&settings).is_err());
    }

    #[test]
    fn rejects_invalid_polynomials_and_tables() {
        let mut settings = BatterySettings {
            curve: CurveKind::Polynomial,
            ..BatterySettings::default()
        };
        for coefficients in [
            [0.9, -0.1, 0.0, 0.0, 0.0, 0.0],
            [1.0, 0.1, 0.0, 0.0, 0.0, 0.0],
            [1.0, -2.0, 0.0, 0.0, 0.0, 0.0],
            [1.0, f64::NAN, 0.0, 0.0, 0.0, 0.0],
        ] {
            settings.coefficients = coefficients;
            assert!(calculate(&settings).is_err());
        }
        settings.curve = CurveKind::Table;
        for table in [
            vec![],
            vec![[0.0, 1.0]],
            vec![[0.0, 1.0], [0.0, 0.8], [1.0, 0.7]],
            vec![[0.0, 1.0], [1.0, 1.1]],
            vec![[0.0, 1.0], [0.9, 0.8]],
            vec![[0.0, 1.0], [1.0, f64::INFINITY]],
        ] {
            settings.table = table;
            assert!(calculate(&settings).is_err());
        }
    }

    #[test]
    fn versioned_files_round_trip_every_curve_and_custom_size() {
        for curve in [
            CurveKind::Chemistry,
            CurveKind::Polynomial,
            CurveKind::Table,
        ] {
            let settings = BatterySettings {
                curve,
                size: "My custom battery".into(),
                ..BatterySettings::default()
            };
            assert_eq!(decode(&encode(&settings).unwrap()).unwrap(), settings);
        }
        let mut invalid = BatterySettings::default();
        invalid.table[0][1] = f64::NAN;
        assert!(encode(&invalid).is_err());
        assert!(decode(include_str!("../../../docs/examples/battery-analysis.json")).is_ok());
        let json = encode(&BatterySettings::default()).unwrap();
        assert!(decode(&json.replace("\"version\": 1", "\"version\": 99")).is_err());
        assert!(decode(&json.replace("wavecraft-battery", "other")).is_err());
        assert!(decode("{}").is_err());
        assert!(decode(&" ".repeat(MAX_FILE_BYTES as usize + 1)).is_err());
    }
}
