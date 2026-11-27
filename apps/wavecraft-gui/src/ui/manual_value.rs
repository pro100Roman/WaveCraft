//! Shared numeric editing without pointer-based value changes.

use crate::egui;
use std::ops::RangeInclusive;

/// A compact typed numeric editor. Horizontal pointer drags never change its
/// value. Use this for parameter fields unless drag-to-adjust is explicitly
/// requested. Keyboard editing, precision, bounds, and number formats are retained.
pub(crate) struct ManualValue<'a, Num: egui::emath::Numeric> {
    value: &'a mut Num,
    speed: Option<f64>,
    range: Option<RangeInclusive<f64>>,
    clamp_existing_to_range: Option<bool>,
    min_decimals: Option<usize>,
    max_decimals: Option<usize>,
    hexadecimal: Option<(usize, bool, bool)>,
}

impl<'a, Num: egui::emath::Numeric> ManualValue<'a, Num> {
    pub(crate) fn new(value: &'a mut Num) -> Self {
        Self {
            value,
            speed: None,
            range: None,
            clamp_existing_to_range: None,
            min_decimals: None,
            max_decimals: None,
            hexadecimal: None,
        }
    }

    pub(crate) fn speed(mut self, speed: impl Into<f64>) -> Self {
        self.speed = Some(speed.into());
        self
    }

    pub(crate) fn range<RangeNum: egui::emath::Numeric>(
        mut self,
        range: RangeInclusive<RangeNum>,
    ) -> Self {
        self.range = Some(range.start().to_f64()..=range.end().to_f64());
        self
    }

    pub(crate) fn clamp_existing_to_range(mut self, clamp: bool) -> Self {
        self.clamp_existing_to_range = Some(clamp);
        self
    }

    pub(crate) fn min_decimals(mut self, decimals: usize) -> Self {
        self.min_decimals = Some(decimals);
        self
    }

    pub(crate) fn max_decimals(mut self, decimals: usize) -> Self {
        self.max_decimals = Some(decimals);
        self
    }

    pub(crate) fn hexadecimal(
        mut self,
        min_width: usize,
        twos_complement: bool,
        upper: bool,
    ) -> Self {
        self.hexadecimal = Some((min_width, twos_complement, upper));
        self
    }
}

impl<Num: egui::emath::Numeric> egui::Widget for ManualValue<'_, Num> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            value,
            speed,
            range,
            clamp_existing_to_range,
            min_decimals,
            max_decimals,
            hexadecimal,
        } = self;
        let old_value = *value;
        let mut editor = egui::DragValue::new(&mut *value);
        if let Some(speed) = speed {
            editor = editor.speed(speed);
        }
        if let Some(range) = range {
            editor = editor.range(range);
        }
        if let Some(clamp) = clamp_existing_to_range {
            editor = editor.clamp_existing_to_range(clamp);
        }
        if let Some(decimals) = min_decimals {
            editor = editor.min_decimals(decimals);
        }
        if let Some(decimals) = max_decimals {
            editor = editor.max_decimals(decimals);
        }
        if let Some((min_width, twos_complement, upper)) = hexadecimal {
            editor = editor.hexadecimal(min_width, twos_complement, upper);
        }

        let explanation_tooltips = ui.style().explanation_tooltips;
        ui.style_mut().explanation_tooltips = false;
        let mut response = ui.add(editor);
        ui.style_mut().explanation_tooltips = explanation_tooltips;
        if response.dragged() {
            *value = old_value;
            response.changed = false;
        }
        response.on_hover_cursor(egui::CursorIcon::Text)
    }
}
