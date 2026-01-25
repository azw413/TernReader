use esp_hal::{
    Blocking,
    analog::adc::{Adc, AdcCalLine, AdcChannel, AdcConfig, AdcPin, Attenuation},
    gpio::{AnalogPin, Input, InputConfig, InputPin},
    peripherals::ADC1,
};
use log::trace;
use microreader_core::input::ButtonState;

const ADC_THRESHOLDS_1: [i16; 4] = [2635, 2015, 1117, 3];
const ADC_THRESHOLDS_2: [i16; 2] = [1680, 3];
const ADC_TOLERANCE: i16 = 400;

type AdcCal<'a> = AdcCalLine<ADC1<'a>>;

pub struct GpioButtonState<'a, Pin1, Pin2>
where
    Pin1: AdcChannel + AnalogPin,
    Pin2: AdcChannel + AnalogPin,
{
    inner: ButtonState,
    pin1: AdcPin<Pin1, ADC1<'a>, AdcCal<'a>>,
    pin2: AdcPin<Pin2, ADC1<'a>, AdcCal<'a>>,
    pin_power: Input<'a>,
    adc: Adc<'a, ADC1<'a>, Blocking>,
}

impl<'a, Pin1: AdcChannel + AnalogPin, Pin2: AdcChannel + AnalogPin>
    GpioButtonState<'a, Pin1, Pin2>
{
    pub fn new(pin1: Pin1, pin2: Pin2, pin_power: impl InputPin + 'a, adc: ADC1<'a>) -> Self {
        let mut adc_config = AdcConfig::new();

        let pin1 = adc_config.enable_pin_with_cal::<_, AdcCal>(pin1, Attenuation::_11dB);
        let pin2 = adc_config.enable_pin_with_cal::<_, AdcCal>(pin2, Attenuation::_11dB);
        let pin_power = Input::new(pin_power, InputConfig::default());
        let adc = Adc::new(adc, adc_config);
        GpioButtonState {
            inner: ButtonState::default(),
            pin1,
            pin2,
            pin_power,
            adc,
        }
    }

    fn get_button_from_adc(value: i16, thresholds: &[i16]) -> Option<u8> {
        if value > 3800 {
            return None;
        }
        for (i, &threshold) in thresholds.iter().enumerate() {
            if (value - threshold).abs() < ADC_TOLERANCE {
                return Some(i as u8);
            }
        }
        None
    }

    pub fn update(&mut self) {
        let mut current: u8 = 0;
        let raw_button1 = nb::block!(self.adc.read_oneshot(&mut self.pin1)).unwrap();
        if let Some(button) = Self::get_button_from_adc(raw_button1 as _, &ADC_THRESHOLDS_1) {
            current |= 1 << button;
        }
        let raw_button2 = nb::block!(self.adc.read_oneshot(&mut self.pin2)).unwrap();
        if let Some(button) = Self::get_button_from_adc(raw_button2 as _, &ADC_THRESHOLDS_2) {
            current |= 1 << (button + 4);
        }
        if self.pin_power.is_low() {
            current |= 1 << 6;
        }
        trace!(
            "Button ADC Readings - Pin1: {}, Pin2: {}, Current State: {:07b}",
            raw_button1, raw_button2, current
        );
        self.inner.update(current);
    }

    pub fn get_buttons(&self) -> ButtonState {
        self.inner
    }
}
