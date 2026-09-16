use embassy_stm32::{
    Peri,
    adc::{Adc, SampleTime, VREF_CALIB_MV, VrefInt},
    peripherals::{ADC1, PA0, PA5, PB1, PB12},
};

pub struct Meter<'a> {
    adc: Adc<'a, ADC1>,
    current: Peri<'a, PA0>,
    battery: Peri<'a, PB1>,
    output_low: Peri<'a, PA5>,
    output_high: Peri<'a, PB12>,
    vref: VrefInt,
}

impl<'a> Meter<'a> {
    pub fn new(
        adc: Peri<'a, ADC1>,
        current: Peri<'a, PA0>,
        battery: Peri<'a, PB1>,
        output_low: Peri<'a, PA5>,
        output_high: Peri<'a, PB12>,
    ) -> Self {
        let adc = Adc::new(adc);
        let vref = adc.enable_vrefint();
        Self {
            adc,
            current,
            battery,
            output_low,
            output_high,
            vref,
        }
    }

    fn vdda(&mut self) -> f64 {
        let raw = self
            .adc
            .blocking_read(&mut self.vref, SampleTime::CYCLES160_5);
        if raw == 0 {
            return 3.3;
        }
        VREF_CALIB_MV as f64 * self.vref.calibrated_value() as f64 / raw as f64 / 1000.0
    }

    fn read_voltage<P: embassy_stm32::adc::AdcChannel<ADC1>>(
        adc: &mut Adc<'a, ADC1>,
        pin: &mut P,
        vdda: f64,
    ) -> f64 {
        // average eight conversions here to reduce regulator and pump noise.
        let mut sum = 0u32;
        for _ in 0..8 {
            sum += adc.blocking_read(pin, SampleTime::CYCLES160_5) as u32;
        }
        (sum as f64 / 8.0) * vdda / 4095.0
    }

    pub fn battery_voltage(&mut self) -> f64 {
        let vdda = self.vdda();
        Self::read_voltage(&mut self.adc, &mut self.battery, vdda) * 2.0
    }

    pub fn charge_pump_voltage(&mut self) -> f64 {
        let vdda = self.vdda();
        Self::read_voltage(&mut self.adc, &mut self.output_high, vdda) * 5.7
    }

    pub fn low_side_voltage(&mut self) -> f64 {
        let vdda = self.vdda();
        Self::read_voltage(&mut self.adc, &mut self.output_low, vdda) * 5.7
    }

    pub fn delta_voltage(&mut self) -> f64 {
        self.charge_pump_voltage() - self.low_side_voltage()
    }

    pub fn measure_current_ma(&mut self) -> f64 {
        let vdda = self.vdda();
        Self::read_voltage(&mut self.adc, &mut self.current, vdda) / 0.680
    }
}
