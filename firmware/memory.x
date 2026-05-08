/* These values correspond to the NRF52832 with SoftDevices S132 7.3.0 */
_sd_ram_size = 0x3378;

MEMORY
{
  FLASH : ORIGIN = 0x00000000 + 152K, LENGTH = 256K - 152K
  RAM : ORIGIN = 0x20000000 + _sd_ram_size, LENGTH = 32K - _sd_ram_size
}
