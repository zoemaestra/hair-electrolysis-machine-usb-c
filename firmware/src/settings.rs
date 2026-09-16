use core::sync::atomic::{AtomicU16, Ordering};

use defmt::Debug2Format;
use embassy_stm32::flash::{Blocking, Flash};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};

pub static SAVE_SETTINGS_CHANNEL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

pub static MAX_CURRENT_UA: AtomicU16 = AtomicU16::new(800);
pub static TARGET_LYE_LU: AtomicU16 = AtomicU16::new(50);
pub static RAMP_TIME_MS: AtomicU16 = AtomicU16::new(1000);

const SETTINGS_START: u32 = 124 * 1024;
const PAGE_SIZE: u32 = 2048;
const PAGE_COUNT: usize = 2;
const RECORD_SIZE: usize = 16;
const RECORDS_PER_PAGE: usize = PAGE_SIZE as usize / RECORD_SIZE;
const MAGIC: u32 = 0x4845_4D33; // "HEM3"

#[derive(Clone, Copy)]
struct Record {
    generation: u32,
    current_ua: u16,
    target_lu: u16,
    ramp_ms: u16,
}

impl Record {
    fn encode(self) -> [u8; RECORD_SIZE] {
        let mut out = [0xff; RECORD_SIZE];
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..8].copy_from_slice(&self.generation.to_le_bytes());
        out[8..10].copy_from_slice(&self.current_ua.to_le_bytes());
        out[10..12].copy_from_slice(&self.target_lu.to_le_bytes());
        out[12..14].copy_from_slice(&self.ramp_ms.to_le_bytes());
        let crc = crc16(&out[..14]);
        out[14..16].copy_from_slice(&crc.to_le_bytes());
        out
    }

    fn decode(data: &[u8; RECORD_SIZE]) -> Option<Self> {
        if u32::from_le_bytes(data[0..4].try_into().ok()?) != MAGIC
            || u16::from_le_bytes(data[14..16].try_into().ok()?) != crc16(&data[..14])
        {
            return None;
        }
        let record = Self {
            generation: u32::from_le_bytes(data[4..8].try_into().ok()?),
            current_ua: u16::from_le_bytes(data[8..10].try_into().ok()?),
            target_lu: u16::from_le_bytes(data[10..12].try_into().ok()?),
            ramp_ms: u16::from_le_bytes(data[12..14].try_into().ok()?),
        };
        (record.current_ua <= 2300 && record.target_lu <= 80 && record.ramp_ms <= 5000)
            .then_some(record)
    }
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub struct SettingsStore<'a> {
    flash: Flash<'a, Blocking>,
    generation: u32,
    active_page: usize,
}

impl<'a> SettingsStore<'a> {
    pub async fn new(mut flash: Flash<'a, Blocking>) -> Self {
        let mut latest: Option<(Record, usize)> = None;
        for page in 0..PAGE_COUNT {
            for slot in 0..RECORDS_PER_PAGE {
                let mut bytes = [0; RECORD_SIZE];
                if flash.read(address(page, slot), &mut bytes).is_err() {
                    continue;
                }
                if let Some(record) = Record::decode(&bytes) {
                    let newer = latest
                        .map(|(old, _)| {
                            record.generation.wrapping_sub(old.generation) < 0x8000_0000
                        })
                        .unwrap_or(true);
                    if newer {
                        latest = Some((record, page));
                    }
                }
            }
        }

        if let Some((record, _)) = latest {
            MAX_CURRENT_UA.store(record.current_ua, Ordering::Relaxed);
            TARGET_LYE_LU.store(record.target_lu, Ordering::Relaxed);
            RAMP_TIME_MS.store(record.ramp_ms, Ordering::Relaxed);
        }

        Self {
            flash,
            generation: latest.map(|(record, _)| record.generation).unwrap_or(0),
            active_page: latest.map(|(_, page)| page).unwrap_or(0),
        }
    }

    pub async fn tick_forever(&mut self) {
        loop {
            SAVE_SETTINGS_CHANNEL.wait().await;
            if let Err(error) = self.save() {
                defmt::error!("settings write failed: {}", Debug2Format(&error));
            }
        }
    }

    fn save(&mut self) -> Result<(), embassy_stm32::flash::Error> {
        self.generation = self.generation.wrapping_add(1);
        let record = Record {
            generation: self.generation,
            current_ua: MAX_CURRENT_UA.load(Ordering::Relaxed),
            target_lu: TARGET_LYE_LU.load(Ordering::Relaxed),
            ramp_ms: RAMP_TIME_MS.load(Ordering::Relaxed),
        };

        if let Some(slot) = self.first_erased_slot(self.active_page)? {
            return self
                .flash
                .write(address(self.active_page, slot), &record.encode());
        }

        let next_page = 1 - self.active_page;
        self.flash.erase(
            SETTINGS_START + next_page as u32 * PAGE_SIZE,
            SETTINGS_START + (next_page as u32 + 1) * PAGE_SIZE,
        )?;
        self.flash.write(address(next_page, 0), &record.encode())?;
        self.active_page = next_page;
        Ok(())
    }

    fn first_erased_slot(
        &mut self,
        page: usize,
    ) -> Result<Option<usize>, embassy_stm32::flash::Error> {
        for slot in 0..RECORDS_PER_PAGE {
            let mut bytes = [0; RECORD_SIZE];
            self.flash.read(address(page, slot), &mut bytes)?;
            if bytes.iter().all(|byte| *byte == 0xff) {
                return Ok(Some(slot));
            }
        }
        Ok(None)
    }
}

fn address(page: usize, slot: usize) -> u32 {
    SETTINGS_START + page as u32 * PAGE_SIZE + slot as u32 * RECORD_SIZE as u32
}
