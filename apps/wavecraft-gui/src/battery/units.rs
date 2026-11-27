//! Display units convert to the solver's canonical V, mAh, mW, mA and ms.

use serde::{Deserialize, Serialize};

pub(crate) trait DisplayUnit: Copy + Eq + 'static {
    const ALL: &'static [Self];
    fn symbol(self) -> &'static str;
    fn scale(self) -> f64;
    fn to_display(self, canonical: f64) -> f64 {
        canonical / self.scale()
    }
    fn to_canonical(self, displayed: f64) -> f64 {
        displayed * self.scale()
    }
}

macro_rules! units {
    ($name:ident, $default:ident, $( $variant:ident => ($symbol:literal, $scale:expr) ),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum $name { $( $variant ),+ }
        impl Default for $name { fn default() -> Self { Self::$default } }
        impl DisplayUnit for $name {
            const ALL: &'static [Self] = &[$(Self::$variant),+];
            fn symbol(self) -> &'static str { match self { $(Self::$variant => $symbol),+ } }
            fn scale(self) -> f64 { match self { $(Self::$variant => $scale),+ } }
        }
    };
}

units!(TimeUnit, Milliseconds,
    Nanoseconds => ("ns", 1e-6), Milliseconds => ("ms", 1.0),
    Seconds => ("s", 1_000.0), Minutes => ("min", 60_000.0));
units!(VoltageUnit, Volts, Millivolts => ("mV", 0.001), Volts => ("V", 1.0));
units!(CurrentUnit, Milliamps,
    Microamps => ("µA", 0.001), Milliamps => ("mA", 1.0), Amps => ("A", 1_000.0));
units!(PowerUnit, Milliwatts,
    Microwatts => ("µW", 0.001), Milliwatts => ("mW", 1.0), Watts => ("W", 1_000.0));
units!(CapacityUnit, MilliampHours,
    MilliampHours => ("mAh", 1.0), AmpHours => ("Ah", 1_000.0));

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supported_units_convert_to_canonical_quantities() {
        assert_eq!(TimeUnit::Nanoseconds.to_canonical(1_000_000.0), 1.0);
        assert_eq!(TimeUnit::Milliseconds.to_canonical(1.0), 1.0);
        assert_eq!(TimeUnit::Seconds.to_canonical(1.0), 1000.0);
        assert_eq!(TimeUnit::Minutes.to_canonical(1.0), 60_000.0);
        assert_eq!(VoltageUnit::Millivolts.to_canonical(4200.0), 4.2);
        assert_eq!(CurrentUnit::Microamps.to_canonical(1000.0), 1.0);
        assert_eq!(CurrentUnit::Amps.to_canonical(1.0), 1000.0);
        assert_eq!(PowerUnit::Microwatts.to_canonical(1000.0), 1.0);
        assert_eq!(PowerUnit::Watts.to_canonical(1.0), 1000.0);
        assert_eq!(CapacityUnit::AmpHours.to_canonical(1.0), 1000.0);
    }
}
