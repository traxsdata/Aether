/* i2c_hid.h - Intel LPSS I2C + HID-over-I2C touchpad/pen. */
#pragma once

/* Probe the Yoga-class LPSS I2C controllers and register a HID pointing
 * device. Safe to call when the hardware is absent. */
void i2c_hid_init(void);

/* Poll the input register at a bounded rate; call from the window manager. */
void i2c_hid_poll(void);

/* Parser self-test for the kernel unit tests (0 = ok). */
int i2c_hid_selftest(void);
