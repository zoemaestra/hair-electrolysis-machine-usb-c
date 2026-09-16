use core::sync::atomic::Ordering;

use arrayvec::ArrayString;
use embassy_stm32::{
    Peri,
    gpio::Pin,
    i2c::{I2c, Master},
    mode::Async,
};
use embedded_graphics::{
    Drawable,
    image::Image,
    mono_font::{MonoTextStyle, ascii::FONT_8X13},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, Point, Size},
    primitives::Rectangle,
};
use embedded_icon::{NewIcon, icons::iconoir::size18px};
use embedded_text::{
    TextBox,
    alignment::HorizontalAlignment,
    style::{HeightMode, TextBoxStyleBuilder},
};
use ssd1306::{
    I2CDisplayInterface, Ssd1306Async,
    mode::{BufferedGraphicsModeAsync, DisplayConfigAsync},
    prelude::{DisplayRotation, I2CInterface},
    size::DisplaySize128x64,
};

use crate::{
    button::Button,
    buzzer::{BUZZER_COMMAND_CHANNEL, BuzzerCommand},
    gui::{
        dialog::Dialog, menu::Menu, run_view::RunView, self_test_results::SelfTestResults,
        settings::Settings,
    },
    output_handler::{OutputHandler, RunParams},
    safety::USB_CONNECTED,
    settings::{MAX_CURRENT_UA, RAMP_TIME_MS, TARGET_LYE_LU},
};

type MyDisplay<'a> = Ssd1306Async<
    I2CInterface<I2c<'a, Async, Master>>,
    DisplaySize128x64,
    BufferedGraphicsModeAsync<DisplaySize128x64>,
>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BatteryLevel {
    Full,
    Half,
    Empty,
}

pub struct App<'a> {
    battery_voltage: f64,
    battery_level: BatteryLevel,
    current_view: View,
    display: MyDisplay<'a>,
    btn_ok: Button<'a>,
}

impl<'a> App<'a> {
    pub async fn new(i2c: I2c<'a, Async, Master>, btn_ok: Peri<'a, impl Pin>) -> Self {
        let mut display = Ssd1306Async::new(
            I2CDisplayInterface::new(i2c),
            DisplaySize128x64,
            DisplayRotation::Rotate0,
        )
        .into_buffered_graphics_mode();
        display.init().await.unwrap();

        Self {
            battery_voltage: 0.0,
            battery_level: BatteryLevel::Full,
            current_view: View::main_menu(),
            display,
            btn_ok: Button::new(btn_ok),
        }
    }

    pub async fn scroll(&mut self, delta: i32) {
        if delta != 0 {
            let _ = BUZZER_COMMAND_CHANNEL.try_send(BuzzerCommand::CLICK);
        }
        self.current_view.scroll(delta);
    }

    pub async fn render(&mut self) {
        if self.btn_ok.clicked() {
            self.click().await;
        }

        let character_style = MonoTextStyle::new(&FONT_8X13, BinaryColor::On);
        let textbox_style = TextBoxStyleBuilder::new()
            .height_mode(HeightMode::FitToText)
            .alignment(HorizontalAlignment::Justified)
            .paragraph_spacing(6)
            .build();
        self.display.clear_buffer();

        let maybe_title = self.current_view.render(&mut self.display).await;
        let title = maybe_title
            .as_ref()
            .map(|x| x.as_str())
            .unwrap_or("meow :3");
        TextBox::with_textbox_style(
            title,
            Rectangle::new(Point::new(0, 0), Size::new(128, 0)),
            character_style,
            textbox_style,
        )
        .draw(&mut self.display)
        .unwrap();
        self.draw_battery_icon();
        self.display.flush().await.unwrap();
    }

    async fn click(&mut self) {
        match &mut self.current_view {
            View::MainMenu(menu) => match menu.selection() {
                0 => {
                    // Start
                    if self.usb_connected() {
                        BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::ERROR).await;
                        self.current_view = View::usb_connected_error();
                    } else {
                        BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::OK).await;
                        self.current_view = View::Run(RunView::new());
                        OutputHandler::run(RunParams {
                            lye_units: TARGET_LYE_LU.load(Ordering::Relaxed) as f64,
                            max_current_ma: MAX_CURRENT_UA.load(Ordering::Relaxed) as f64 / 1000.0,
                            ramp_time_ms: RAMP_TIME_MS.load(Ordering::Relaxed),
                        })
                        .await;
                    }
                }
                1 => {
                    // Self test
                    BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::OK).await;
                    self.current_view = View::self_test_preamble();
                }
                2 => {
                    // Settings
                    BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::OK).await;
                    self.current_view = View::settings_menu();
                }
                _ => unreachable!(),
            },
            View::MainMenuError(_) => self.current_view = View::main_menu(),
            View::Run(_) => {
                OutputHandler::abort_run();
                let _ = BUZZER_COMMAND_CHANNEL.try_send(BuzzerCommand::CANCEL);
            }
            View::SelfTestPreamble(_) => {
                BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::OK).await;
                self.self_test().await;
            }
            View::SelfTestResults(_) => self.current_view = View::main_menu(),
            View::SettingsMenu(menu) => match menu.selection() {
                0 => {
                    // Electrolysis
                    BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::OK).await;
                    self.current_view = View::electrolysis_settings();
                }
                1 => {
                    // About
                    BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::OK).await;
                    self.current_view = View::about();
                }
                2 => {
                    // Back
                    BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::OK).await;
                    self.current_view = View::main_menu();
                }
                _ => unreachable!(),
            },
            View::ElectrolysisSettings(settings) => {
                if settings.click().await {
                    self.current_view = View::settings_menu();
                }
            }
            View::About(_) => {
                BUZZER_COMMAND_CHANNEL.send(BuzzerCommand::OK).await;
                self.current_view = View::settings_menu();
            }
        }
    }

    async fn self_test(&mut self) {
        OutputHandler::self_test().await;
        self.current_view = View::SelfTestResults(OutputHandler::self_test_results().await);
    }

    fn draw_battery_icon(&mut self) {
        let point = Point::new(104, 0);
        if let Some(new_value) = OutputHandler::battery_voltage() {
            // Moving average to better deal with noise
            if self.battery_voltage <= 0.1 {
                self.battery_voltage = new_value;
            } else {
                self.battery_voltage = self.battery_voltage * 0.8 + new_value * 0.2;
            }

            // Hysteresis to prevent values flicking back and forth
            // Maybe a battery % or just straight up voltage would be better? idk
            self.battery_level = match self.battery_level {
                BatteryLevel::Full => {
                    if self.battery_voltage < 3.75 {
                        BatteryLevel::Half
                    } else {
                        BatteryLevel::Full
                    }
                }
                BatteryLevel::Half => {
                    if self.battery_voltage > 3.85 {
                        BatteryLevel::Full
                    } else if self.battery_voltage < 3.55 {
                        BatteryLevel::Empty
                    } else {
                        BatteryLevel::Half
                    }
                }
                BatteryLevel::Empty => {
                    if self.battery_voltage > 3.65 {
                        BatteryLevel::Half
                    } else {
                        BatteryLevel::Empty
                    }
                }
            };
        }

        if self.usb_connected() {
            Image::new(&size18px::BatteryCharging::new(BinaryColor::On), point)
                .draw(&mut self.display)
                .unwrap();
        } else {
            match self.battery_level {
                BatteryLevel::Full => {
                    Image::new(&size18px::BatteryFull::new(BinaryColor::On), point)
                        .draw(&mut self.display)
                        .unwrap()
                }
                BatteryLevel::Half => Image::new(&size18px::Battery50::new(BinaryColor::On), point)
                    .draw(&mut self.display)
                    .unwrap(),

                // Display warning to tell user to charge the battery
                // It's still safe to use but it won't be able to output the same as a charged battery
                BatteryLevel::Empty => {
                    Image::new(&size18px::BatteryWarning::new(BinaryColor::On), point)
                        .draw(&mut self.display)
                        .unwrap()
                }
            }
        }
    }

    fn usb_connected(&self) -> bool {
        USB_CONNECTED.load(Ordering::Relaxed)
    }
}

enum View {
    MainMenu(Menu<3>),
    MainMenuError(Dialog),
    Run(RunView),
    SelfTestPreamble(Dialog),
    SelfTestResults(SelfTestResults),
    SettingsMenu(Menu<3>),
    ElectrolysisSettings(Settings),
    About(Dialog),
}

impl View {
    fn main_menu() -> Self {
        Self::MainMenu(Menu::new(["Start", "Self-test", "Settings"]))
    }
    fn usb_connected_error() -> Self {
        Self::MainMenuError(Dialog::new(
            "Cannot start when USB is connected! This is for safety.",
        ))
    }
    fn self_test_preamble() -> Self {
        Self::SelfTestPreamble(Dialog::new(
            "Disconnect the probe from the body of the device, then press OK\nFor accurate results, run while charging",
        ))
    }
    fn settings_menu() -> Self {
        Self::SettingsMenu(Menu::new(["Electrolysis", "About", "Back"]))
    }
    fn electrolysis_settings() -> Self {
        Self::ElectrolysisSettings(Settings::new())
    }
    fn about() -> Self {
        Self::About(Dialog::new(
            "Zoemaestra's DIY hair electrolysis machine\nhttps://imzoe.me\nBased on n3tcat's design",
        ))
    }

    fn scroll(&mut self, delta: i32) {
        match self {
            Self::MainMenu(menu) | Self::SettingsMenu(menu) => menu.scroll(delta),
            Self::ElectrolysisSettings(settings) => settings.scroll(delta.try_into().unwrap_or(0)),
            _ => {}
        }
    }

    async fn render<E: core::fmt::Debug, D: DrawTarget<Color = BinaryColor, Error = E>>(
        &mut self,
        display: &mut D,
    ) -> Option<ArrayString<12>> {
        match self {
            Self::MainMenu(menu) => {
                menu.render(display);
                Some("Main menu".try_into().unwrap())
            }
            Self::MainMenuError(dialog) => {
                dialog.render(display);
                Some("Wuh-oh!".try_into().unwrap())
            }
            Self::SelfTestPreamble(dialog) => {
                dialog.render(display);
                Some("Self-test".try_into().unwrap())
            }
            Self::SelfTestResults(results) => {
                results.render(display);
                Some("Self-test".try_into().unwrap())
            }
            Self::Run(run_view) => {
                if OutputHandler::run_aborted() {
                    *self = Self::main_menu();
                    Some("Main menu".try_into().unwrap())
                } else {
                    Some(run_view.render(display).await)
                }
            }
            Self::SettingsMenu(menu) => {
                menu.render(display);
                Some("Settings".try_into().unwrap())
            }
            Self::ElectrolysisSettings(settings) => {
                settings.render(display);
                Some("Electrolysis".try_into().unwrap())
            }
            Self::About(dialog) => {
                dialog.render(display);
                Some("About".try_into().unwrap())
            }
        }
    }
}
