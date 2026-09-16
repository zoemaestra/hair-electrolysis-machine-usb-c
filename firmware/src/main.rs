#![no_std]
#![no_main]

mod app;
mod button;
mod buzzer;
mod charge_pump;
mod current_dac;
#[path = "../../src/gui/mod.rs"]
mod gui;
mod meter;
mod output_handler;
mod safety;
mod settings;

use core::sync::atomic::Ordering;

use app::App;
use buzzer::{BUZZER_COMMAND_CHANNEL, Buzzer};
use charge_pump::ChargePump;
use current_dac::CurrentDac;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_stm32::{
    bind_interrupts, dma,
    exti::{self, ExtiInput},
    flash::Flash,
    gpio::{Input, Pull},
    i2c::{self, I2c},
    peripherals::{DMA1_CH1, DMA1_CH2, I2C2},
};
use embassy_time::Timer;
use meter::Meter;
use output_handler::OutputHandler;
use portable_atomic::AtomicI32;
use safety::USB_CONNECTED;
use settings::SettingsStore;

use defmt_rtt as _;

bind_interrupts!(struct Irqs {
    I2C2_3 => i2c::EventInterruptHandler<I2C2>, i2c::ErrorInterruptHandler<I2C2>;
    DMA1_CHANNEL1 => dma::InterruptHandler<DMA1_CH1>;
    DMA1_CHANNEL2_3 => dma::InterruptHandler<DMA1_CH2>;
    EXTI2_3 => exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI2_3>;
});

static ENCODER_DELTA: AtomicI32 = AtomicI32::new(0);

#[embassy_executor::task]
async fn encoder(
    mut r1: ExtiInput<'static, embassy_stm32::mode::Async>,
    mut r2: ExtiInput<'static, embassy_stm32::mode::Async>,
) {
    let mut previous = ((r1.is_high() as u8) << 1) | r2.is_high() as u8;
    let mut steps = 0i8;

    loop {
        match select(r1.wait_for_any_edge(), r2.wait_for_any_edge()).await {
            Either::First(_) | Either::Second(_) => {}
        }

        // Contact debounce settling (1 ms)
        Timer::after_millis(1).await;
        let current = ((r1.is_high() as u8) << 1) | r2.is_high() as u8;
        const STEP: [i8; 16] = [0, -1, 1, 0, 1, 0, 0, -1, -1, 0, 0, 1, 0, 1, -1, 0];
        steps += STEP[((previous << 2) | current) as usize];
        previous = current;

        if steps <= -2 {
            ENCODER_DELTA.fetch_add(1, Ordering::Relaxed);
            steps = 0;
        } else if steps >= 2 {
            ENCODER_DELTA.fetch_add(-1, Ordering::Relaxed);
            steps = 0;
        }
    }
}

#[embassy_executor::task]
async fn buzzer_handler(mut buzzer: Buzzer<'static>) {
    loop {
        buzzer.play(BUZZER_COMMAND_CHANNEL.receive().await).await;
    }
}

#[embassy_executor::task]
async fn store_handler(mut store: SettingsStore<'static>) {
    store.tick_forever().await;
}

#[embassy_executor::task]
async fn output_task(mut handler: OutputHandler<'static>) {
    handler.tick_forever().await;
}

#[embassy_executor::task]
async fn usb_monitor(input: Input<'static>) {
    let mut stable = input.is_high();
    USB_CONNECTED.store(stable, Ordering::Relaxed);
    let mut count: u8 = 0;
    loop {
        let sample = input.is_high();
        if sample != stable {
            count += 1;
            if count >= 25 {
                // 50 ms of consistent new state required to switch
                stable = sample;
                USB_CONNECTED.store(stable, Ordering::Relaxed);
                count = 0;
            }
        } else {
            count = 0;
        }
        Timer::after_millis(2).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_stm32::Config::default();
    config.rcc.pll = Some(embassy_stm32::rcc::Pll {
        source: embassy_stm32::rcc::PllSource::HSI,
        prediv: embassy_stm32::rcc::PllPreDiv::DIV1,
        mul: embassy_stm32::rcc::PllMul::MUL8,
        divp: None,
        divq: None,
        divr: Some(embassy_stm32::rcc::PllRDiv::DIV2),
    });
    config.rcc.sys = embassy_stm32::rcc::Sysclk::PLL1_R;
    let p = embassy_stm32::init(config);

    let store = SettingsStore::new(Flash::new_blocking(p.FLASH)).await;
    spawner.spawn(store_handler(store)).unwrap();

    let r2 = ExtiInput::new(p.PD2, p.EXTI2, Pull::Up, Irqs);
    let r1 = ExtiInput::new(p.PD3, p.EXTI3, Pull::Up, Irqs);
    spawner.spawn(encoder(r1, r2)).unwrap();

    spawner
        .spawn(buzzer_handler(Buzzer::new(p.TIM1, p.PC11)))
        .unwrap();

    spawner
        .spawn(usb_monitor(Input::new(p.PC8, Pull::None)))
        .unwrap();

    let meter = Meter::new(p.ADC1, p.PA0, p.PB1, p.PA5, p.PB12);
    let dac = CurrentDac::new(p.DAC1, p.PA4);
    let pump = ChargePump::new(p.TIM15, p.PC1, p.PC2);
    spawner
        .spawn(output_task(OutputHandler::new(
            pump, dac, meter, p.PB2, p.IWDG,
        )))
        .unwrap();

    let mut i2c_config = i2c::Config::default();
    i2c_config.frequency = embassy_stm32::time::Hertz(400_000);
    let display_i2c = I2c::new(
        p.I2C2, p.PB10, p.PB11, p.DMA1_CH1, p.DMA1_CH2, Irqs, i2c_config,
    );
    let mut app = App::new(display_i2c, p.PD4).await;

    loop {
        app.scroll(ENCODER_DELTA.swap(0, Ordering::Relaxed)).await;
        app.render().await;
        Timer::after_millis(5).await;
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Reset immediately: peripheral reset states disable TIM15 and DAC1.
    cortex_m::peripheral::SCB::sys_reset()
}

#[cortex_m_rt::exception]
unsafe fn HardFault(_frame: &cortex_m_rt::ExceptionFrame) -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}
