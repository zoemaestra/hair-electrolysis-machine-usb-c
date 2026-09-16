use core::{
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering},
};

use arrayvec::ArrayString;
use embassy_stm32::{
    Peri,
    gpio::{Input, Pull},
    peripherals::{IWDG, PB2},
    wdg::IndependentWatchdog,
};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};
use embassy_time::{Duration, Instant, TimeoutError, Timer, WithTimeout};
use micromath::F32Ext;

use crate::{
    buzzer::{BUZZER_COMMAND_CHANNEL, BuzzerCommand},
    charge_pump::ChargePump,
    current_dac::{CurrentDac, MAX_CURRENT_MA},
    gui::self_test_results::{SelfTestResults, SweepPoint},
    meter::Meter,
    safety::USB_CONNECTED,
};

static OUTPUT_COMMAND_CHANNEL: Channel<CriticalSectionRawMutex, OutputCommand, 1> = Channel::new();
static ABORT_REQUESTED: AtomicBool = AtomicBool::new(false);
static SELF_TEST_RESULT_CHANNEL: Signal<CriticalSectionRawMutex, SelfTestResults> = Signal::new();
static BATTERY_VOLTAGE_CHANNEL: Signal<CriticalSectionRawMutex, f64> = Signal::new();
static RUN_STATUS_CHANNEL: Signal<CriticalSectionRawMutex, RunStatus> = Signal::new();
static RUN_ABORTED_CHANNEL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

const MAX_RUN_TIME: Duration = Duration::from_secs(120);
const CURRENT_FAULT_MARGIN_MA: f64 = 0.25;

enum OutputCommand {
    SelfTest,
    Run(RunParams),
}

pub struct RunParams {
    pub lye_units: f64,
    pub max_current_ma: f64,
    pub ramp_time_ms: u16,
}

pub struct RunStatus {
    pub lye_units: f64,
    pub current_ma: f64,
    pub measured_current_ma: f64,
    pub body_resistance: f64,
    pub duration_ms: u32,
}

impl Default for RunStatus {
    fn default() -> Self {
        Self {
            lye_units: 0.0,
            current_ma: 0.0,
            measured_current_ma: 0.0,
            body_resistance: 0.0,
            duration_ms: 0,
        }
    }
}

pub struct OutputHandler<'a> {
    charge_pump: ChargePump<'a>,
    current_dac: CurrentDac<'a>,
    meter: Meter<'a>,
    foot_pedal: Input<'a>,
    watchdog: IndependentWatchdog<'a, IWDG>,
}

impl<'a> OutputHandler<'a> {
    pub fn new(
        mut charge_pump: ChargePump<'a>,
        mut current_dac: CurrentDac<'a>,
        meter: Meter<'a>,
        foot_pedal: Peri<'a, PB2>,
        watchdog: Peri<'a, IWDG>,
    ) -> Self {
        charge_pump.disable();
        current_dac.disable();
        let mut watchdog = IndependentWatchdog::new(watchdog, 2_000_000);
        watchdog.unleash();
        Self {
            charge_pump,
            current_dac,
            meter,
            foot_pedal: Input::new(foot_pedal, Pull::Up),
            watchdog,
        }
    }

    pub async fn tick_forever(&mut self) {
        loop {
            match OUTPUT_COMMAND_CHANNEL
                .receive()
                .with_timeout(Duration::from_millis(100))
                .await
            {
                Ok(OutputCommand::SelfTest) => self.self_test_inner().await,
                Ok(OutputCommand::Run(params)) => self.run_inner(params).await,
                Err(TimeoutError) => {
                    BATTERY_VOLTAGE_CHANNEL.signal(self.meter.battery_voltage());
                }
            }
            self.watchdog.pet();
        }
    }

    fn shutdown(&mut self) {
        self.charge_pump.disable();
        self.current_dac.disable();
        self.watchdog.pet();
    }

    fn inhibited(&self) -> bool {
        USB_CONNECTED.load(Ordering::Relaxed)
    }

    async fn self_test_inner(&mut self) {
        let mut sweep = [SweepPoint::empty(); 32];

        self.current_dac.disable();
        self.charge_pump.enable();
        self.watchdog.pet();
        Timer::after_millis(1000).await;
        self.watchdog.pet();
        let voltage = self.meter.charge_pump_voltage();

        for (i, sample) in sweep.iter_mut().enumerate() {
            self.watchdog.pet();
            let _ = BUZZER_COMMAND_CHANNEL.try_send(BuzzerCommand::new(i as u32 * 100 + 100, 40));
            let requested = MAX_CURRENT_MA * i as f64 / 31.0;
            self.current_dac.set_ma(requested);
            Timer::after_millis(50).await;
            let measured = self.meter.measure_current_ma();
            let delta_v = self.meter.delta_voltage();
            let resistance = if measured > 0.01 {
                delta_v * 1000.0 / measured
            } else {
                0.0
            };
            let mut row = ArrayString::<64>::new();
            let _ = write!(
                &mut row,
                "\t{:.2}\t{:.2}\t{:.2}\t{:.2}",
                requested,
                measured,
                resistance,
                self.meter.charge_pump_voltage()
            );
            defmt::info!("{}", row.as_str());
            *sample = SweepPoint::new(requested, measured);
        }

        self.shutdown();
        SELF_TEST_RESULT_CHANNEL.signal(SelfTestResults::new(voltage, sweep));
    }

    async fn run_inner(&mut self, params: RunParams) {
        self.shutdown();
        // Triugger held when start selected cannot accidentally start treatment for safety
        while self.foot_pedal.is_low() && !self.stop_requested() {
            self.watchdog.pet();
            Timer::after_millis(5).await;
        }

        while !self.stop_requested() {
            self.watchdog.pet();
            if self.foot_pedal.is_low() {
                Timer::after_millis(15).await;
                if self.foot_pedal.is_low() && !self.stop_requested() {
                    self.run_once_inner(&params).await;
                }
                while self.foot_pedal.is_low() && !self.stop_requested() {
                    self.watchdog.pet();
                    Timer::after_millis(5).await;
                }
            }
            Timer::after_millis(2).await;
        }

        self.shutdown();
        RUN_ABORTED_CHANNEL.signal(());
    }

    fn stop_requested(&self) -> bool {
        self.inhibited() || ABORT_REQUESTED.load(Ordering::Relaxed)
    }

    async fn run_once_inner(&mut self, params: &RunParams) {
        self.current_dac.disable();
        self.charge_pump.enable();
        self.watchdog.pet();
        Timer::after_millis(100).await;
        self.watchdog.pet();
        if self.stop_requested() || self.foot_pedal.is_high() {
            self.shutdown();
            return;
        }
        let _ = BUZZER_COMMAND_CHANNEL.try_send(BuzzerCommand::OK);

        let ramp_area = params.ramp_time_ms as f64 * params.max_current_ma / 2.0 / 100.0;
        let ramp_down_start = (params.lye_units - ramp_area).max(params.lye_units / 2.0);
        let ramp_down_span = (params.lye_units - ramp_down_start).max(0.001);
        let mut top_current = params.max_current_ma;
        let mut total_lye = 0.0;
        let mut commanded_current = 0.0;
        let start = Instant::now();
        let mut last = start;

        while total_lye < params.lye_units && self.foot_pedal.is_low() {
            self.watchdog.pet();
            if self.stop_requested() {
                break;
            }
            if start.elapsed() >= MAX_RUN_TIME {
                defmt::error!("maximum treatment duration exceeded");
                ABORT_REQUESTED.store(true, Ordering::Relaxed);
                break;
            }

            let measured = self.meter.measure_current_ma();
            let allowed = (commanded_current + CURRENT_FAULT_MARGIN_MA).max(0.35);
            if !measured.is_finite()
                || measured < -0.02
                || measured > allowed
                || measured > MAX_CURRENT_MA + CURRENT_FAULT_MARGIN_MA
            {
                defmt::error!("current plausibility fault: {} mA", measured);
                ABORT_REQUESTED.store(true, Ordering::Relaxed);
                break;
            }

            let now = Instant::now();
            total_lye += (now - last).as_millis() as f64 * measured.max(0.0) / 100.0;
            last = now;
            let elapsed = now - start;

            let current = if total_lye >= ramp_down_start {
                let remaining_ratio =
                    ((params.lye_units - total_lye) / ramp_down_span).clamp(0.0, 1.0);
                top_current * (remaining_ratio as f32).sqrt() as f64
            } else if params.ramp_time_ms == 0 || elapsed.as_millis() >= params.ramp_time_ms as u64
            {
                top_current = params.max_current_ma;
                params.max_current_ma
            } else {
                let value =
                    params.max_current_ma * elapsed.as_millis() as f64 / params.ramp_time_ms as f64;
                top_current = value;
                value
            };
            self.current_dac.set_ma(current);
            commanded_current = current;

            let delta_v = self.meter.delta_voltage();
            let resistance = if measured > 0.01 {
                delta_v * 1000.0 / measured
            } else {
                0.0
            };
            RUN_STATUS_CHANNEL.signal(RunStatus {
                lye_units: total_lye,
                current_ma: current,
                measured_current_ma: measured,
                body_resistance: resistance,
                duration_ms: elapsed.as_millis().min(u32::MAX as u64) as u32,
            });
            Timer::after_millis(2).await;
        }

        self.shutdown();
        let _ = BUZZER_COMMAND_CHANNEL.try_send(BuzzerCommand::OK);
        Timer::after_millis(15).await;
    }

    pub async fn self_test() {
        OUTPUT_COMMAND_CHANNEL.send(OutputCommand::SelfTest).await;
    }
    pub async fn run(params: RunParams) {
        ABORT_REQUESTED.store(false, Ordering::Relaxed);
        OUTPUT_COMMAND_CHANNEL
            .send(OutputCommand::Run(params))
            .await;
    }
    pub fn abort_run() {
        ABORT_REQUESTED.store(true, Ordering::Relaxed);
    }
    pub fn battery_voltage() -> Option<f64> {
        BATTERY_VOLTAGE_CHANNEL.try_take()
    }
    pub fn try_get_run_status() -> Option<RunStatus> {
        RUN_STATUS_CHANNEL.try_take()
    }
    pub fn run_aborted() -> bool {
        RUN_ABORTED_CHANNEL.try_take().is_some()
    }
    pub async fn self_test_results() -> SelfTestResults {
        SELF_TEST_RESULT_CHANNEL.wait().await
    }
}

impl Drop for OutputHandler<'_> {
    fn drop(&mut self) {
        self.shutdown();
    }
}
