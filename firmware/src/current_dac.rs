use embassy_stm32::{
    Peri,
    dac::{DacChannel, Value},
    mode::Blocking,
    peripherals::{DAC1, PA4},
};
use num_traits::float::FloatCore;


// Theoretical 2.2426
// Experimentally determined below
pub const MAX_CURRENT_MA: f64 = 2.426;

pub struct CurrentDac<'a> {
    dac: DacChannel<'a, Blocking>,
}

impl<'a> CurrentDac<'a> {
    pub fn new(peripheral: Peri<'a, DAC1>, pin: Peri<'a, PA4>) -> Self {
        let mut dac = DacChannel::new_blocking(peripheral, pin);
        dac.set(Value::Bit12Right(0));
        Self { dac }
    }

    pub fn disable(&mut self) {
        self.dac.set(Value::Bit12Right(0));
    }

    pub fn set_ma(&mut self, milliamps: f64) {
        let raw = (milliamps.clamp(0.0, MAX_CURRENT_MA) * 4095.0 / MAX_CURRENT_MA).round() as u16;
        self.dac.set(Value::Bit12Right(raw));
    }
}

impl Drop for CurrentDac<'_> {
    fn drop(&mut self) {
        self.disable();
    }
}
