MEMORY
{
  /* STM32G0B1CB: 128 KiB flash. */
  /* The final two 2 KiB pages are a power-fail-safe settings journal. */
  FLASH : ORIGIN = 0x08000000, LENGTH = 124K
  /* Use the parity-capable 128 KiB layout; the optional extra 16 KiB is not linked. */
  RAM   : ORIGIN = 0x20000000, LENGTH = 128K
}

/* The interrupt vector table is 0xBC bytes on this target. Round code up to
 * its required 8-byte alignment instead of placing .text at 0x080000BC. */
_stext = ORIGIN(FLASH) + 0xC0;

__settings_flash_offset = 124K;
