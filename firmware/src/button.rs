use embassy_stm32::{
    Peri,
    gpio::{Input, Pin, Pull},
};
use embassy_time::Instant;

// Added some debouncing to try and avoid accidental false presses

pub struct Button<'a> {
    input: Input<'a>,
    stable_high: bool,
    candidate_high: bool,
    changed_at: Instant,
}

impl<'a> Button<'a> {
    pub fn new<P: Pin>(pin: Peri<'a, P>) -> Self {
        Self {
            input: Input::new(pin, Pull::Up),
            stable_high: true,
            candidate_high: true,
            changed_at: Instant::now(),
        }
    }

    pub fn clicked(&mut self) -> bool {
        let high = self.input.is_high();
        if high != self.candidate_high {
            self.candidate_high = high;
            self.changed_at = Instant::now();
        }
        // Deliberately ignore presses less than 20ms
        if high != self.stable_high && self.changed_at.elapsed().as_millis() >= 20 {
            self.stable_high = high;
            return !high;
        }
        false
    }
}
