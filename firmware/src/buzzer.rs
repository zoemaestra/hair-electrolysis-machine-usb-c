use embassy_stm32::{
    Peri,
    gpio::OutputType,
    peripherals::{PC11, TIM1},
    time::Hertz,
    timer::{
        low_level::CountingMode,
        simple_pwm::{PwmPin, SimplePwm},
    },
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;

pub static BUZZER_COMMAND_CHANNEL: Channel<CriticalSectionRawMutex, BuzzerCommand, 16> =
    Channel::new();

#[derive(Debug, defmt::Format)]
pub struct BuzzerCommand {
    pub frequency_hz: u32,
    pub duration_ms: u32,
}

// Buzzer is single sided, PC11 supplies the signal

impl BuzzerCommand {
    pub const OK: Self = Self::new(700, 100);
    pub const ACCEPT: Self = Self::new(1400, 250);
    pub const ERROR: Self = Self::new(200, 100);
    pub const CLICK: Self = Self::new(100, 25);
    pub const CANCEL: Self = Self::new(500, 50);

    pub const fn new(frequency_hz: u32, duration_ms: u32) -> Self {
        Self {
            frequency_hz,
            duration_ms,
        }
    }
}

pub struct Buzzer<'a> {
    pwm: SimplePwm<'a, TIM1>,
}

impl<'a> Buzzer<'a> {
    pub fn new(timer: Peri<'a, TIM1>, output: Peri<'a, PC11>) -> Self {
        let output = PwmPin::new(output, OutputType::PushPull);
        let mut pwm = SimplePwm::new(
            timer,
            None,
            None,
            None,
            Some(output),
            Hertz(100),
            CountingMode::EdgeAlignedUp,
        );
        {
            let mut channel = pwm.ch4();
            channel.set_duty_cycle_fully_off();
            channel.enable();
        }
        Self { pwm }
    }

    pub async fn play(&mut self, command: BuzzerCommand) {
        if command.frequency_hz == 0 || command.duration_ms == 0 {
            return;
        }

        self.pwm.set_frequency(Hertz(command.frequency_hz));
        self.pwm.ch4().set_duty_cycle_percent(50);
        Timer::after_millis(command.duration_ms as u64).await;
        self.disable();
    }

    fn disable(&mut self) {
        self.pwm.ch4().set_duty_cycle_fully_off();
    }
}

impl Drop for Buzzer<'_> {
    fn drop(&mut self) {
        self.disable();
    }
}
