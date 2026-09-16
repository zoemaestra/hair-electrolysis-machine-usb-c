use embassy_stm32::{
    Peri,
    gpio::OutputType,
    peripherals::TIM15,
    time::Hertz,
    timer::{
        low_level::{CountingMode, OutputPolarity},
        simple_pwm::{PwmPin, SimplePwm},
    },
};

pub struct ChargePump<'a> {
    pwm: SimplePwm<'a, TIM15>,
}

impl<'a> ChargePump<'a> {
    // Charge pump generates a 10khz
    pub fn new(
        timer: Peri<'a, TIM15>,
        osc1: Peri<'a, embassy_stm32::peripherals::PC1>,
        osc0: Peri<'a, embassy_stm32::peripherals::PC2>,
    ) -> Self {
        let ch1 = PwmPin::new(osc1, OutputType::PushPull);
        let ch2 = PwmPin::new(osc0, OutputType::PushPull);
        let mut pwm = SimplePwm::new(
            timer,
            Some(ch1),
            Some(ch2),
            None,
            None,
            Hertz(10_000),
            CountingMode::EdgeAlignedUp,
        );
        {
            let mut a = pwm.ch1();
            a.set_duty_cycle_fully_off();
            a.enable();
        }
        {
            let mut b = pwm.ch2();
            b.set_polarity(OutputPolarity::ActiveLow);
            b.set_duty_cycle_fully_on();
            b.enable();
        }
        Self { pwm }
    }

    pub fn enable(&mut self) {
        self.pwm.ch1().set_duty_cycle_percent(50);
        self.pwm.ch2().set_duty_cycle_percent(50);
    }

    pub fn disable(&mut self) {
        self.pwm.ch1().set_duty_cycle_fully_off();
        self.pwm.ch2().set_duty_cycle_fully_on();
    }
}

impl Drop for ChargePump<'_> {
    fn drop(&mut self) {
        self.disable();
    }
}
